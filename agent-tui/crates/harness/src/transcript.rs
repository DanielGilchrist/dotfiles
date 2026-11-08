//! A subagent's transcript read as a log: what it said, each tool call with
//! how it went, and what it handed back.

use std::collections::HashMap;
use std::fmt::Write;
use std::time::{Duration, SystemTime};

use agent_core::activity::ToolId;
use agent_core::ports::{Call, CallState, Excerpt, LogEntry, Summary, Target};
use serde_json::Value;

/// How much of what a subagent said a log entry keeps.
const SAID: usize = 1200;
/// Log entries kept per subagent, the newest.
pub(crate) const LOG_ENTRIES: usize = 400;
/// How much of a call's input and result are kept for expanding it.
const INPUT: usize = 1000;
const OUTPUT: usize = 2000;
/// How much of a report is kept.
const REPORT: usize = 8000;
/// Lines shown under a call that failed or was flagged.
const EXCERPT: usize = 3;
const SUMMARY: usize = 160;
const TARGET: usize = 240;

#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub log: Vec<LogEntry>,
    /// The last `SubagentHandback` message.
    pub handed_back: Option<String>,
    /// The last thing said, which is the report when nothing was handed back.
    pub last_said: Option<String>,
    pub last_heard: Option<SystemTime>,
    /// Entries older than [`LOG_ENTRIES`] were dropped.
    pub dropped: bool,
    pub stopped: Option<String>,
}

/// The records in `text`, one JSON per line. Lines that aren't whole records
/// are skipped.
pub(crate) fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut calls: HashMap<String, usize> = HashMap::new();
    for record in text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        let at = record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_time);
        out.last_heard = out.last_heard.max(at);
        let kind = record.get("type").and_then(Value::as_str);
        if kind == Some("assistant") {
            if record.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
                let text = record
                    .pointer("/message/content")
                    .map(crate::text_of)
                    .unwrap_or_default();
                out.stopped = Some(first_line(&text, SUMMARY));
            } else {
                out.stopped = None;
            }
        }
        let Some(parts) = record.pointer("/message/content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            let field = |key: &str| part.get(key).and_then(Value::as_str);
            match (kind, field("type")) {
                (Some("assistant"), Some("text")) => {
                    let said = field("text").unwrap_or_default().trim();
                    if !said.is_empty() {
                        out.last_said = Some(said.chars().take(REPORT).collect());
                        out.log.push(LogEntry::Said {
                            text: said.chars().take(SAID).collect(),
                            at,
                        });
                    }
                }
                (Some("assistant"), Some("tool_use")) => {
                    let input = part.get("input").unwrap_or(&Value::Null);
                    if field("name") == Some("SubagentHandback") {
                        out.handed_back = input
                            .get("message")
                            .and_then(Value::as_str)
                            .map(|m| m.trim().chars().take(REPORT).collect());
                        continue;
                    }
                    let tool = field("name").unwrap_or("a tool");
                    if let Some(id) = field("id") {
                        calls.insert(id.to_string(), out.log.len());
                    }
                    out.log.push(LogEntry::Called(Call {
                        id: ToolId::new(field("id").unwrap_or_default()),
                        tool: tool_name(tool),
                        target: target_of(tool, input),
                        state: CallState::Running,
                        started: at,
                        input: input_text(tool, input),
                        output: String::new(),
                    }));
                }
                (Some("user"), Some("tool_result")) => {
                    let Some(&index) = field("tool_use_id").and_then(|id| calls.get(id)) else {
                        continue;
                    };
                    let Some(LogEntry::Called(call)) = out.log.get_mut(index) else {
                        continue;
                    };
                    let text = part.get("content").map(crate::text_of).unwrap_or_default();
                    let is_error = part.get("is_error").and_then(Value::as_bool) == Some(true);
                    let result = record.get("toolUseResult");
                    let took = call
                        .started
                        .zip(at)
                        .and_then(|(from, to)| to.duration_since(from).ok());
                    call.state = outcome(&call.tool, &text, is_error, result, took);
                    call.output = text.chars().take(OUTPUT).collect();
                }
                _ => {}
            }
        }
    }
    let over = out.log.len().saturating_sub(LOG_ENTRIES);
    out.dropped = over > 0;
    out.log.drain(..over);
    out
}

/// `mcp__server__tool` reads `server.tool`.
fn tool_name(name: &str) -> String {
    match name.strip_prefix("mcp__").map(|rest| rest.split_once("__")) {
        Some(Some((server, tool))) => format!("{server}.{tool}"),
        _ => name.to_string(),
    }
}

/// `text` with control characters turned into spaces.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The first non-empty line of `text`, plain, cut to `max` characters.
fn first_line(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default();
    plain(line.trim()).chars().take(max).collect()
}

fn status(todo: &Value) -> Option<&str> {
    todo.get("status").and_then(Value::as_str)
}

/// A command without a leading `cd <dir> && `, on one line.
fn command_target(command: &str) -> String {
    let mut line = command
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim();
    while let Some(rest) = line.strip_prefix("cd ") {
        let Some(at) = rest.find(" && ").or_else(|| rest.find("; ")) else {
            break;
        };
        let next = rest[at..].trim_start_matches([' ', '&', ';']);
        if next.is_empty() {
            break;
        }
        line = next;
    }
    plain(line)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(TARGET)
        .collect()
}

fn target_of(tool: &str, input: &Value) -> Target {
    let text = |key: &str| {
        input
            .get(key)
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
    };
    let number = |key: &str| input.get(key).and_then(Value::as_u64);
    match tool {
        "Bash" => text("command").map_or(Target::None, |c| Target::Command(command_target(c))),
        "Read" | "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
            let Some(path) = text("file_path").or_else(|| text("notebook_path")) else {
                return Target::None;
            };
            let range = match (number("offset"), number("limit")) {
                (Some(from), Some(limit)) => Some((from, Some(from + limit.max(1) - 1))),
                (Some(from), None) => Some((from, None)),
                (None, Some(limit)) => Some((1, Some(limit))),
                (None, None) => None,
            };
            Target::Path {
                path: path.to_string(),
                range: range.filter(|_| tool == "Read"),
            }
        }
        "Grep" => text("pattern").map_or(Target::None, |pattern| Target::Search {
            pattern: first_line(pattern, TARGET),
            path: text("path").map(str::to_string),
        }),
        "Glob" => text("pattern").map_or(Target::None, |p| Target::Pattern(first_line(p, TARGET))),
        "WebFetch" => text("url").map_or(Target::None, |url| {
            let bare = url.split_once("://").map_or(url, |(_, rest)| rest);
            Target::Url(first_line(bare, TARGET))
        }),
        "WebSearch" => text("query").map_or(Target::None, |q| Target::Query(first_line(q, TARGET))),
        "Agent" | "Task" => {
            text("description").map_or(Target::None, |d| Target::Task(first_line(d, TARGET)))
        }
        "TodoWrite" => {
            let todos = input.get("todos").and_then(Value::as_array);
            todos.map_or(Target::None, |todos| Target::Todos {
                done: todos
                    .iter()
                    .filter(|t| status(t) == Some("completed"))
                    .count(),
                total: todos.len(),
                current: todos
                    .iter()
                    .find(|t| status(t) == Some("in_progress"))
                    .and_then(|t| t.get("content").and_then(Value::as_str))
                    .map(|c| first_line(c, TARGET)),
            })
        }
        _ => [
            "command",
            "file_path",
            "pattern",
            "url",
            "description",
            "query",
        ]
        .iter()
        .find_map(|key| text(key))
        .map_or(Target::None, |t| Target::Other(first_line(t, TARGET))),
    }
}

/// A call's whole input as text.
fn input_text(tool: &str, input: &Value) -> String {
    if tool == "Bash"
        && let Some(command) = input.get("command").and_then(Value::as_str)
    {
        return command.trim().chars().take(INPUT).collect();
    }
    let Some(fields) = input.as_object() else {
        return String::new();
    };
    let mut out = String::new();
    for (key, value) in fields {
        let value = match value {
            Value::String(s) => s.chars().take(300).collect::<String>(),
            other => other.to_string().chars().take(300).collect(),
        };
        let _ = writeln!(out, "{key}: {value}");
    }
    out.trim_end().chars().take(INPUT).collect()
}

fn exit_code(text: &str) -> Option<i32> {
    text.strip_prefix("Exit code ")?
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .next()?
        .parse()
        .ok()
}

fn lines_of(text: &str) -> Vec<&str> {
    text.lines().filter(|l| !l.trim().is_empty()).collect()
}

fn excerpt(lines: &[&str]) -> Excerpt {
    Excerpt {
        lines: lines
            .iter()
            .take(EXCERPT)
            .map(|l| plain(l.trim_end()).chars().take(SUMMARY).collect())
            .collect(),
        more: lines.len().saturating_sub(EXCERPT),
    }
}

/// Whether a line of a command's output reports a failure. Deliberately
/// short: a miss leaves a plain row, and a passing run never reads as failed.
fn is_failure_line(line: &str) -> bool {
    let line = line.trim_start();
    line.contains("test result: FAILED")
        || line.contains(" FAILED")
        || line.contains("panicked at")
        || line.starts_with("FAIL")
        || line.strip_prefix("error").is_some_and(|rest| {
            rest.starts_with(':')
                || rest.strip_prefix("[E").is_some_and(|code| {
                    code.trim_start_matches(|c: char| c.is_ascii_digit())
                        .starts_with("]:")
                })
        })
        || counts_failures(line)
}

/// `3 failed`, but not `0 failed`.
fn counts_failures(line: &str) -> bool {
    line.match_indices(" failed").any(|(at, _)| {
        let before = &line[..at];
        let digits: String = before
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        digits.parse::<usize>().is_ok_and(|n| n > 0)
    })
}

fn outcome(
    tool: &str,
    text: &str,
    is_error: bool,
    result: Option<&Value>,
    took: Option<Duration>,
) -> CallState {
    let cleaned = text
        .replace("<tool_use_error>", "")
        .replace("</tool_use_error>", "");
    let code = exit_code(&cleaned);
    if is_error || code.is_some_and(|c| c != 0) {
        let stderr = result
            .and_then(|r| r.get("stderr"))
            .and_then(Value::as_str)
            .map(lines_of)
            .filter(|l| !l.is_empty());
        let body: Vec<&str> = lines_of(&cleaned)
            .into_iter()
            .filter(|l| !l.starts_with("Exit code "))
            .collect();
        return CallState::Failed {
            code,
            excerpt: excerpt(&stderr.unwrap_or(body)),
            took,
        };
    }
    let summary = summarize(tool, text, result);
    if tool == "Bash" {
        let failing: Vec<&str> = lines_of(text)
            .into_iter()
            .filter(|l| is_failure_line(l))
            .collect();
        if !failing.is_empty() {
            return CallState::Flagged {
                summary,
                excerpt: excerpt(&failing),
                took,
            };
        }
    }
    CallState::Done { summary, took }
}

fn summarize(tool: &str, text: &str, result: Option<&Value>) -> Summary {
    let number = |path: &str| {
        result
            .and_then(|r| r.pointer(path))
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
    };
    let lines = lines_of(text);
    match tool {
        "Read" => {
            if result.and_then(|r| r.get("type")).and_then(Value::as_str) == Some("image") {
                return Summary::Image;
            }
            Summary::Lines(number("/file/numLines").unwrap_or_else(|| text.lines().count()))
        }
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
            let patch = result
                .and_then(|r| r.get("structuredPatch"))
                .and_then(Value::as_array)
                .filter(|p| !p.is_empty());
            if let Some(patch) = patch {
                let (mut added, mut removed) = (0, 0);
                for line in patch
                    .iter()
                    .filter_map(|hunk| hunk.get("lines").and_then(Value::as_array))
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    match line.chars().next() {
                        Some('+') => added += 1,
                        Some('-') => removed += 1,
                        _ => {}
                    }
                }
                return Summary::Diff { added, removed };
            }
            if result.and_then(|r| r.get("type")).and_then(Value::as_str) == Some("create") {
                let content = result
                    .and_then(|r| r.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                return Summary::NewFile {
                    lines: content.lines().count(),
                };
            }
            text_summary(&lines)
        }
        "Grep" => {
            let mode = result.and_then(|r| r.get("mode")).and_then(Value::as_str);
            match (mode, number("/numFiles"), number("/numLines")) {
                (_, _, Some(matches)) => Summary::Matches(matches),
                (_, Some(files), None) => Summary::Files(files),
                _ if lines.is_empty() => Summary::Matches(0),
                _ => Summary::Matches(lines.len()),
            }
        }
        "Glob" => Summary::Files(number("/numFiles").unwrap_or(lines.len())),
        _ => text_summary(&lines),
    }
}

/// No output, the one line there is, or how many lines.
fn text_summary(lines: &[&str]) -> Summary {
    match lines {
        [] => Summary::NoOutput,
        [one] => Summary::Text(first_line(one, SUMMARY)),
        many => Summary::Lines(many.len()),
    }
}

/// `2026-11-08T15:55:57.721Z` as a time.
pub(crate) fn parse_time(stamp: &str) -> Option<SystemTime> {
    let stamp = stamp.strip_suffix('Z')?;
    let (date, time) = stamp.split_once('T')?;
    let mut d = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let (clock, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = clock.split(':').map(str::parse::<u64>);
    let (hour, minute, second) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    let millis: u64 = format!("{fraction:0<3}")[..3].parse().ok()?;
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = u64::try_from(era * 146_097 + doe - 719_468).ok()?;
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    Some(SystemTime::UNIX_EPOCH + Duration::from_millis(secs * 1000 + millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: &str, at: &str, content: &str, extra: &str) -> String {
        format!(
            r#"{{"type":"{kind}","timestamp":"{at}"{extra},"message":{{"content":{content}}}}}"#
        )
    }

    fn use_of(id: &str, name: &str, input: &str, at: &str) -> String {
        record(
            "assistant",
            at,
            &format!(r#"[{{"type":"tool_use","id":"{id}","name":"{name}","input":{input}}}]"#),
            "",
        )
    }

    fn result_of(id: &str, text: &str, error: bool, at: &str, extra: &str) -> String {
        record(
            "user",
            at,
            &format!(
                r#"[{{"type":"tool_result","tool_use_id":"{id}","is_error":{error},"content":{}}}]"#,
                serde_json::to_string(text).expect("json")
            ),
            extra,
        )
    }

    fn calls(parsed: &Parsed) -> Vec<&Call> {
        parsed
            .log
            .iter()
            .filter_map(|e| match e {
                LogEntry::Called(c) => Some(c),
                LogEntry::Said { .. } => None,
            })
            .collect()
    }

    #[test]
    fn timestamps_parse_to_the_millisecond() {
        let t = parse_time("2026-11-08T15:55:57.721Z").expect("parses");
        let epoch = t.duration_since(SystemTime::UNIX_EPOCH).expect("after");
        assert_eq!(epoch.as_millis(), 1_794_153_357_721);
        assert_eq!(
            parse_time("1970-01-01T00:00:01Z").map(|t| t
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("after")
                .as_secs()),
            Some(1)
        );
        assert_eq!(parse_time("yesterday"), None);
    }

    #[test]
    fn a_call_is_running_until_its_result_and_takes_the_time_between() {
        let text = [
            use_of(
                "a",
                "Bash",
                r#"{"command":"cd /x/y && cargo test"}"#,
                "2026-11-08T10:00:00.000Z",
            ),
            use_of(
                "b",
                "Bash",
                r#"{"command":"sleep 9"}"#,
                "2026-11-08T10:00:01.000Z",
            ),
            result_of("a", "ok", false, "2026-11-08T10:00:51.500Z", ""),
        ]
        .join("\n");
        let parsed = parse(&text);
        let calls = calls(&parsed);
        assert_eq!(calls[0].target, Target::Command("cargo test".into()));
        assert_eq!(
            calls[0].state,
            CallState::Done {
                summary: Summary::Text("ok".into()),
                took: Some(Duration::from_millis(51_500))
            }
        );
        assert_eq!(calls[1].state, CallState::Running);
        assert_eq!(calls[1].state.took(), None);
        assert_eq!(parsed.last_heard, parse_time("2026-11-08T10:00:51.500Z"));
    }

    #[test]
    fn failures_come_from_is_error_and_exit_codes_with_stderr_first() {
        let text = [
            use_of("a", "Bash", r#"{"command":"cargo build"}"#, "2026-11-08T10:00:00.000Z"),
            result_of(
                "a",
                "Exit code 101\nout1\nerror[E0502]: borrow\n  --> x.rs\nmore\nmore2",
                true,
                "2026-11-08T10:00:14.000Z",
                r#","toolUseResult":{"stderr":"error[E0502]: borrow\n  --> x.rs\nlast\nlast2\nlast3"}"#,
            ),
            use_of("b", "Edit", r#"{"file_path":"/a.rs"}"#, "2026-11-08T10:00:15.000Z"),
            result_of("b", "<tool_use_error>String to replace not found in file.</tool_use_error>", true, "2026-11-08T10:00:15.100Z", ""),
        ]
        .join("\n");
        let parsed = parse(&text);
        let calls = calls(&parsed);
        assert_eq!(
            calls[0].state,
            CallState::Failed {
                code: Some(101),
                excerpt: Excerpt {
                    lines: vec![
                        "error[E0502]: borrow".into(),
                        "  --> x.rs".into(),
                        "last".into()
                    ],
                    more: 2
                },
                took: Some(Duration::from_secs(14))
            }
        );
        assert_eq!(
            calls[1].state,
            CallState::Failed {
                code: None,
                excerpt: Excerpt {
                    lines: vec!["String to replace not found in file.".into()],
                    more: 0
                },
                took: Some(Duration::from_millis(100))
            }
        );
    }

    #[test]
    fn a_passing_run_is_never_flagged_and_a_failing_one_that_exited_zero_is() {
        let pass = "test result: ok. 411 passed; 0 failed; 0 ignored";
        let fail = "running\ntest a ... FAILED\ntest result: FAILED. 409 passed; 3 failed\nmore";
        let text = [
            use_of(
                "a",
                "Bash",
                r#"{"command":"cargo test | tail"}"#,
                "2026-11-08T10:00:00.000Z",
            ),
            result_of("a", pass, false, "2026-11-08T10:00:01.000Z", ""),
            use_of(
                "b",
                "Bash",
                r#"{"command":"cargo test | tail"}"#,
                "2026-11-08T10:00:02.000Z",
            ),
            result_of("b", fail, false, "2026-11-08T10:00:03.000Z", ""),
        ]
        .join("\n");
        let parsed = parse(&text);
        let calls = calls(&parsed);
        assert_eq!(
            calls[0].state,
            CallState::Done {
                summary: Summary::Text(pass.into()),
                took: Some(Duration::from_secs(1))
            }
        );
        assert_eq!(
            calls[1].state,
            CallState::Flagged {
                summary: Summary::Lines(4),
                excerpt: Excerpt {
                    lines: vec![
                        "test a ... FAILED".into(),
                        "test result: FAILED. 409 passed; 3 failed".into()
                    ],
                    more: 0
                },
                took: Some(Duration::from_secs(1))
            }
        );
    }

    #[test]
    fn summaries_read_the_structured_result_and_targets_the_input() {
        let text = [
            use_of("a", "Read", r#"{"file_path":"/p/q.rs","offset":120,"limit":81}"#, "2026-11-08T10:00:00.000Z"),
            result_of("a", "x", false, "2026-11-08T10:00:01.000Z", r#","toolUseResult":{"type":"text","file":{"numLines":730}}"#),
            use_of("b", "Edit", r#"{"file_path":"/p/q.rs"}"#, "2026-11-08T10:00:02.000Z"),
            result_of("b", "ok", false, "2026-11-08T10:00:03.000Z", r#","toolUseResult":{"structuredPatch":[{"lines":[" a","-b","+c","+d"]}]}"#),
            use_of("c", "Write", r#"{"file_path":"/p/n.rs"}"#, "2026-11-08T10:00:04.000Z"),
            result_of("c", "ok", false, "2026-11-08T10:00:05.000Z", r#","toolUseResult":{"type":"create","content":"a\nb\nc","structuredPatch":[]}"#),
            use_of("d", "Grep", r#"{"pattern":"branch_row","path":"crates"}"#, "2026-11-08T10:00:06.000Z"),
            result_of("d", "a\nb", false, "2026-11-08T10:00:07.000Z", r#","toolUseResult":{"mode":"content","numLines":11,"numFiles":3}"#),
            use_of("e", "Grep", r#"{"pattern":"x"}"#, "2026-11-08T10:00:08.000Z"),
            result_of("e", "a\nb", false, "2026-11-08T10:00:09.000Z", r#","toolUseResult":{"mode":"files_with_matches","numFiles":6}"#),
            use_of("f", "mcp__linear__get_issue", r#"{"id":"X-1"}"#, "2026-11-08T10:00:10.000Z"),
            use_of("g", "TodoWrite", r#"{"todos":[{"status":"completed","content":"a"},{"status":"in_progress","content":"b"},{"status":"pending","content":"c"}]}"#, "2026-11-08T10:00:11.000Z"),
            use_of("h", "Bash", r#"{"command":"ls"}"#, "2026-11-08T10:00:12.000Z"),
            result_of("h", "", false, "2026-11-08T10:00:13.000Z", ""),
        ]
        .join("\n");
        let parsed = parse(&text);
        let calls = calls(&parsed);
        let done = |summary| CallState::Done {
            summary,
            took: Some(Duration::from_secs(1)),
        };
        assert_eq!(
            calls[0].target,
            Target::Path {
                path: "/p/q.rs".into(),
                range: Some((120, Some(200)))
            }
        );
        assert_eq!(calls[0].state, done(Summary::Lines(730)));
        assert_eq!(
            calls[1].state,
            done(Summary::Diff {
                added: 2,
                removed: 1
            })
        );
        assert_eq!(calls[2].state, done(Summary::NewFile { lines: 3 }));
        assert_eq!(calls[3].state, done(Summary::Matches(11)));
        assert_eq!(calls[4].state, done(Summary::Files(6)));
        assert_eq!(calls[5].tool, "linear.get_issue");
        assert_eq!(
            calls[6].target,
            Target::Todos {
                done: 1,
                total: 3,
                current: Some("b".into())
            }
        );
        assert_eq!(calls[7].state, done(Summary::NoOutput));
    }

    #[test]
    fn a_handback_is_the_report_and_not_a_row() {
        let text = [
            record(
                "assistant",
                "2026-11-08T10:00:00.000Z",
                r#"[{"type":"text","text":"Working."}]"#,
                "",
            ),
            use_of(
                "a",
                "SubagentHandback",
                r#"{"message":"All done."}"#,
                "2026-11-08T10:00:01.000Z",
            ),
        ]
        .join("\n");
        let parsed = parse(&text);
        assert_eq!(parsed.handed_back.as_deref(), Some("All done."));
        assert_eq!(parsed.last_said.as_deref(), Some("Working."));
        assert_eq!(parsed.log.len(), 1);
    }

    #[test]
    fn an_api_error_record_stops_it_until_the_next_good_message() {
        let error = record(
            "assistant",
            "2026-11-08T10:00:00.000Z",
            r#"[{"type":"text","text":"API Error: overloaded"}]"#,
            r#","isApiErrorMessage":true"#,
        );
        assert_eq!(
            parse(&error).stopped.as_deref(),
            Some("API Error: overloaded")
        );
        let ok = record(
            "assistant",
            "2026-11-08T10:00:01.000Z",
            r#"[{"type":"text","text":"back"}]"#,
            "",
        );
        assert_eq!(parse(&format!("{error}\n{ok}")).stopped, None);
    }

    #[test]
    fn command_targets_lose_their_cd_prefix_and_keep_a_heredoc_opener() {
        assert_eq!(
            command_target("cd ~/a/b && cd c; ls  -la\tx\nmore"),
            "ls -la x"
        );
        assert_eq!(
            command_target("python3 - <<'E'\nprint(1)\nE"),
            "python3 - <<'E'"
        );
        assert_eq!(command_target("cd /only"), "cd /only");
    }
}
