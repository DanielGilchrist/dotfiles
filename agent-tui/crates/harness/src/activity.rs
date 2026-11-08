//! The current activity of a Claude Code transcript: the last prompt the user
//! typed and everything after it.

use std::io;
use std::path::Path;
use std::time::SystemTime;

use agent_core::activity::TranscriptRef;
use agent_core::ports::{
    ActivityDetail, ActivityMark, Asked, Log, Outcome, Progress, ends_with_report,
};
use serde_json::Value;

use crate::{TAIL_BYTES, TAIL_BYTES_MAX, long, read_tail, text_of, transcript};

/// What the user typed to start an activity.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Opening {
    pub text: String,
    /// The `promptId` every record of the activity's user messages shares.
    pub id: String,
    pub at: Option<SystemTime>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Cut {
    /// The last prompt, if the text holds one.
    pub opening: Option<Opening>,
    /// Where its record starts in the text, or 0 without one.
    pub from: usize,
    /// The activity's `promptId`: the prompt's, or the first one seen when the
    /// prompt itself was left unread.
    pub id: Option<String>,
}

/// Records that start with these are the harness talking to itself, not a
/// prompt: a local command's output and an interrupt's marker.
const NOT_PROMPTS: &[&str] = &["<local-command-", "[Request interrupted by user"];

/// What a user record says when it opens an activity.
enum Said {
    /// The user's own words.
    Typed(String),
    /// A slash command. It opens an activity only if work follows it.
    Command(String),
}

/// Whether `record` is a user message that isn't meta, a compaction summary,
/// an interrupt, local output or a background agent's notification, and
/// holds text instead of tool results.
fn said(record: &Value) -> Option<Said> {
    let flag = |key: &str| record.get(key).and_then(Value::as_bool) == Some(true);
    if record.get("type").and_then(Value::as_str) != Some("user")
        || flag("isMeta")
        || flag("isCompactSummary")
        || flag("isSidechain")
    {
        return None;
    }
    let content = record.pointer("/message/content")?;
    if let Value::Array(parts) = content {
        fn kind(part: &Value) -> Option<&str> {
            part.get("type").and_then(Value::as_str)
        }
        if parts.iter().any(|p| kind(p) == Some("tool_result"))
            || !parts.iter().any(|p| kind(p) == Some("text"))
        {
            return None;
        }
    }
    let text = text_of(content);
    let harness = NOT_PROMPTS
        .iter()
        .chain(crate::NOTIFICATION_PREFIXES)
        .any(|prefix| text.starts_with(prefix));
    if text.is_empty() || harness {
        return None;
    }
    Some(if text.starts_with("<command-") {
        Said::Command(command_text(&text))
    } else {
        Said::Typed(text)
    })
}

/// `/simplify args` from the tags Claude Code wraps a slash command in.
fn command_text(text: &str) -> String {
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let from = text.find(&open)? + open.len();
        let to = text[from..].find(&format!("</{name}>"))?;
        Some(text[from..from + to].trim().to_string())
    };
    let name = tag("command-name").unwrap_or_default();
    match tag("command-args").filter(|args| !args.is_empty()) {
        Some(args) => format!("{name} {args}"),
        None => name,
    }
}

fn is_meta(record: &Value) -> bool {
    record.get("isMeta").and_then(Value::as_bool) == Some(true)
}

/// Finds the last prompt in `text`, whole records one per line. A slash
/// command counts once work follows it: the expansion the harness adds as a
/// meta message, or the agent's own reply. One that only prints (`/model`,
/// `/clear`) leaves the activity it interrupted alone.
pub(crate) fn cut(text: &str) -> Cut {
    const USER: &str = r#""type":"user""#;
    const ASSISTANT: &str = r#""type":"assistant""#;
    let mut found: Option<(usize, Opening)> = None;
    let mut command: Option<(usize, Opening)> = None;
    let mut first_id: Option<String> = None;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let user = line.contains(USER);
        if !(user || command.is_some() && line.contains(ASSISTANT)) {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if !user {
            found = command.take().or(found);
            continue;
        }
        let id = ["promptId", "uuid"]
            .iter()
            .find_map(|key| record.get(key).and_then(Value::as_str))
            .map(str::to_string);
        if first_id.is_none() {
            first_id.clone_from(&id);
        }
        let opening = |text: String| Opening {
            text: long(&text),
            id: id.clone().unwrap_or_default(),
            at: record
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(transcript::parse_time),
        };
        match said(&record) {
            Some(Said::Typed(text)) => {
                command = None;
                found = Some((start, opening(text)));
            }
            Some(Said::Command(text)) => command = Some((start, opening(text))),
            None => {
                let own = text_of(record.pointer("/message/content").unwrap_or(&Value::Null));
                if own.starts_with("<local-command-") {
                    command = None;
                } else if is_meta(&record) {
                    found = command.take().or(found);
                }
            }
        }
    }
    match found {
        Some((from, opening)) => Cut {
            id: Some(opening.id.clone()),
            opening: Some(opening),
            from,
        },
        None => Cut {
            opening: None,
            from: 0,
            id: first_id,
        },
    }
}

/// The current activity in the transcript at `path`, read from its end. `None`
/// when the user hasn't said anything in it yet.
pub(crate) fn read(path: &Path) -> io::Result<Option<ActivityDetail>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut window = TAIL_BYTES;
    let (tail, cut) = loop {
        let tail = read_tail(&mut file, len, window, TAIL_BYTES_MAX)?;
        let cut = cut(tail.text());
        if cut.opening.is_some() || tail.reaches_start() || window >= TAIL_BYTES_MAX {
            break (tail, cut);
        }
        window = (window * 4).min(TAIL_BYTES_MAX);
    };
    if cut.opening.is_none() && tail.reaches_start() {
        return Ok(None);
    }
    let mut parsed = transcript::parse(&tail.text()[cut.from..]);
    let report = parsed
        .last_said
        .take()
        .filter(|said| ends_with_report(&parsed.log, said));
    let (outcome, progress) = match (parsed.took, parsed.stopped.take()) {
        (Some(took), _) => (
            Outcome::Finished { report },
            Progress::Finished { took: Some(took) },
        ),
        (None, Some(reason)) => (Outcome::Stopped { reason }, Progress::Stopped),
        (None, None) => (
            Outcome::Running,
            Progress::Running {
                asked: parsed
                    .asking
                    .take()
                    .map(|(call, questions)| Asked { call, questions }),
            },
        ),
    };
    let truncated = parsed.dropped || cut.opening.is_none();
    let (prompt, started) = match cut.opening {
        Some(opening) => (Some(opening.text), opening.at),
        None => (None, None),
    };
    Ok(Some(ActivityDetail {
        log: Log {
            prompt,
            log: std::mem::take(&mut parsed.log),
            transcript: TranscriptRef::new(path.to_string_lossy()),
            outcome,
            model: None,
            started,
            last_heard: parsed.last_heard,
            truncated,
        },
        mark: cut.id.filter(|id| !id.is_empty()).map(ActivityMark::new),
        progress,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{answer, ended, said, typed, use_of, user};
    use agent_core::activity::ToolId;
    use agent_core::ports::{CallState, LogEntry};

    /// An earlier activity, then every kind of user record that isn't a prompt,
    /// then the real one.
    fn history() -> Vec<String> {
        vec![
            typed("p1", "2026-11-08T09:00:00.000Z", "an old question"),
            said("2026-11-08T09:00:01.000Z", "an old answer"),
            ended("2026-11-08T09:00:02.000Z", 2000),
            user(
                "p2",
                "2026-11-08T09:59:00.000Z",
                r#""This session is being continued from a previous conversation""#,
                r#","isCompactSummary":true"#,
            ),
            typed(
                "p2",
                "2026-11-08T09:59:01.000Z",
                "<command-name>/compact</command-name>",
            ),
            typed(
                "p2",
                "2026-11-08T09:59:02.000Z",
                "<local-command-stdout>done</local-command-stdout>",
            ),
            typed(
                "p2",
                "2026-11-08T09:59:03.000Z",
                "<task-notification>a subagent finished</task-notification>",
            ),
            typed("p3", "2026-11-08T10:00:00.000Z", "fix the statusline"),
            user(
                "p3",
                "2026-11-08T10:00:00.500Z",
                r#"[{"type":"text","text":"[Image: source: /tmp/1.png]"}]"#,
                r#","isMeta":true"#,
            ),
            said("2026-11-08T10:00:01.000Z", "Reading the provider first."),
            use_of(
                "a",
                "Read",
                r#"{"file_path":"/x/a.lua"}"#,
                "2026-11-08T10:00:02.000Z",
            ),
            answer("p3", "a", "local x = 1", "2026-11-08T10:00:03.000Z"),
        ]
    }

    #[test]
    fn the_cut_is_the_last_real_prompt_not_meta_tool_results_summaries_or_commands() {
        let text = history().join("\n") + "\n";
        let cut = cut(&text);
        let opening = cut.opening.expect("a prompt");
        assert_eq!(opening.text, "fix the statusline");
        assert_eq!(opening.id, "p3");
        assert_eq!(cut.id.as_deref(), Some("p3"));
        assert!(
            text[cut.from..]
                .lines()
                .next()
                .is_some_and(|line| line.contains(r#""promptId":"p3""#))
        );
        assert!(
            !text[cut.from..].contains("an old answer"),
            "nothing of the earlier activity is in the cut"
        );
    }

    #[test]
    fn a_prompt_with_pasted_images_counts_and_an_interrupt_marker_does_not() {
        let paste = user(
            "p5",
            "2026-11-08T10:00:00.000Z",
            r#"[{"type":"text","text":"what is wrong here"},{"type":"image","source":{}}]"#,
            "",
        );
        let interrupt = user(
            "p5",
            "2026-11-08T10:01:00.000Z",
            r#"[{"type":"text","text":"[Request interrupted by user]"}]"#,
            "",
        );
        let cut = cut(&format!("{paste}\n{interrupt}\n"));
        assert_eq!(
            cut.opening.map(|o| o.text).as_deref(),
            Some("what is wrong here")
        );
    }

    #[test]
    fn with_the_prompt_out_of_reach_the_activity_is_known_by_its_first_prompt_id() {
        let text = [
            answer("p9", "a", "x", "2026-11-08T10:00:03.000Z"),
            answer("p9", "b", "y", "2026-11-08T10:00:04.000Z"),
        ]
        .join("\n");
        let cut = cut(&text);
        assert_eq!((cut.opening, cut.from), (None, 0));
        assert_eq!(cut.id.as_deref(), Some("p9"));
    }

    fn write(dir: &Path, lines: &[String]) -> std::path::PathBuf {
        let path = dir.join("session.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").expect("writes");
        path
    }

    #[test]
    fn an_activity_in_progress_is_its_log_with_the_running_call_open() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let mut lines = history();
        lines.push(use_of(
            "b",
            "Bash",
            r#"{"command":"cargo test"}"#,
            "2026-11-08T10:00:04.000Z",
        ));
        let detail = read(&write(dir.path(), &lines))
            .expect("reads")
            .expect("an activity");
        assert_eq!(detail.log.prompt.as_deref(), Some("fix the statusline"));
        assert_eq!(detail.mark, Some(ActivityMark::new("p3")));
        assert_eq!(detail.log.outcome, Outcome::Running);
        assert_eq!(detail.progress, Progress::Running { asked: None });
        assert!(!detail.log.truncated);
        assert_eq!(
            detail.log.started,
            transcript::parse_time("2026-11-08T10:00:00.000Z")
        );
        let [
            LogEntry::Said { text, .. },
            LogEntry::Called(read),
            LogEntry::Called(bash),
        ] = detail.log.log.as_slice()
        else {
            panic!("{:?}", detail.log.log);
        };
        assert_eq!(text, "Reading the provider first.");
        assert!(matches!(read.state, CallState::Done { .. }));
        assert_eq!(bash.state, CallState::Running);
    }

    #[test]
    fn the_turn_duration_record_ends_it_and_the_last_line_is_the_closing_message() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let mut lines = history();
        lines.push(said("2026-11-08T10:00:09.000Z", "All fixed."));
        lines.push(ended("2026-11-08T10:00:10.000Z", 10_000));
        let detail = read(&write(dir.path(), &lines))
            .expect("reads")
            .expect("an activity");
        assert_eq!(
            detail.progress,
            Progress::Finished {
                took: Some(std::time::Duration::from_secs(10))
            }
        );
        assert_eq!(
            detail.log.outcome,
            Outcome::Finished {
                report: Some("All fixed.".into())
            }
        );

        lines.push(use_of(
            "c",
            "Bash",
            r#"{"command":"ls"}"#,
            "2026-11-08T10:00:11.000Z",
        ));
        let again = read(&write(dir.path(), &lines))
            .expect("reads")
            .expect("an activity");
        assert_eq!(
            (again.progress, again.log.outcome),
            (Progress::Running { asked: None }, Outcome::Running),
            "work after the record means the activity went on"
        );
    }

    #[test]
    fn an_ended_activity_whose_last_step_was_a_call_has_no_closing_message() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let mut lines = history();
        lines.push(use_of(
            "c",
            "Bash",
            r#"{"command":"ls"}"#,
            "2026-11-08T10:00:05.000Z",
        ));
        lines.push(ended("2026-11-08T10:00:06.000Z", 6000));
        let detail = read(&write(dir.path(), &lines))
            .expect("reads")
            .expect("an activity");
        assert_eq!(detail.log.outcome, Outcome::Finished { report: None });
    }

    #[test]
    fn queued_messages_and_compactions_are_entries_and_the_agents_own_notices_are_not() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let attachment = |at: &str, prompt: &str| {
            format!(
                r#"{{"type":"attachment","timestamp":"{at}","attachment":{{"type":"queued_command","prompt":{}}}}}"#,
                serde_json::to_string(prompt).expect("json")
            )
        };
        let mut lines = history();
        lines.push(attachment("2026-11-08T10:00:04.000Z", "also check lsp.lua"));
        lines.push(attachment(
            "2026-11-08T10:00:05.000Z",
            "<task-notification>done</task-notification>",
        ));
        lines.push(
            r#"{"type":"system","subtype":"compact_boundary","timestamp":"2026-11-08T10:00:06.000Z"}"#
                .to_string(),
        );
        let detail = read(&write(dir.path(), &lines))
            .expect("reads")
            .expect("an activity");
        let tail = &detail.log.log[2..];
        assert!(
            matches!(tail, [LogEntry::Queued { text, .. }, LogEntry::Compacted] if text == "also check lsp.lua"),
            "{tail:?}"
        );
    }

    #[test]
    fn a_question_without_an_answer_is_what_the_agent_is_asking() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let input = r#"{"questions":[{"question":"Drop the grid?","header":"Grid","multiSelect":true,"options":[{"label":"Drop it","description":"No grid."},{"label":"Keep","description":"For now."}]}]}"#;
        let mut lines = history();
        lines.push(use_of(
            "q",
            "AskUserQuestion",
            input,
            "2026-11-08T10:00:04.000Z",
        ));
        let detail = read(&write(dir.path(), &lines))
            .expect("reads")
            .expect("an activity");
        let Progress::Running { asked: Some(asked) } = &detail.progress else {
            panic!("{:?}", detail.progress);
        };
        assert_eq!(asked.call, ToolId::new("q"));
        let [question] = asked.questions.as_slice() else {
            panic!("{:?}", asked.questions);
        };
        assert_eq!(
            (
                question.header.as_str(),
                question.text.as_str(),
                question.multi
            ),
            ("Grid", "Drop the grid?", true)
        );
        assert_eq!(question.options.len(), 2);
        assert_eq!(question.options[1].label, "Keep");
        assert_eq!(question.options[1].description, "For now.");

        lines.push(answer("p3", "q", "answered", "2026-11-08T10:00:30.000Z"));
        let answered = read(&write(dir.path(), &lines))
            .expect("reads")
            .expect("an activity");
        assert_eq!(
            answered.progress,
            Progress::Running { asked: None },
            "answered"
        );
    }

    #[test]
    fn no_prompt_anywhere_is_no_activity() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = write(
            dir.path(),
            &[
                r#"{"type":"mode","mode":"auto"}"#.to_string(),
                history()[3].clone(),
            ],
        );
        assert_eq!(read(&path).expect("reads"), None);
    }

    #[test]
    fn a_long_activity_widens_the_read_until_its_prompt_and_a_longer_one_starts_truncated() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let filler = |n: usize| {
            let text = "x".repeat(1000);
            (0..n)
                .map(|i| said("2026-11-08T10:00:01.000Z", &format!("{i} {text}")))
                .collect::<Vec<_>>()
        };
        let mut lines = vec![typed("p1", "2026-11-08T10:00:00.000Z", "start")];
        lines.extend(filler(1500));
        let detail = read(&write(dir.path(), &lines))
            .expect("reads")
            .expect("an activity");
        assert_eq!(
            detail.log.prompt.as_deref(),
            Some("start"),
            "found beyond the first megabyte"
        );
        assert!(
            detail.log.truncated,
            "the log keeps only its newest entries"
        );

        let mut lines = vec![typed("p1", "2026-11-08T10:00:00.000Z", "start")];
        lines.extend(filler(9000));
        let detail = read(&write(dir.path(), &lines))
            .expect("reads")
            .expect("an activity");
        assert_eq!(detail.log.prompt, None, "beyond the widest read");
        assert!(detail.log.truncated);
    }

    #[test]
    fn a_slash_command_starts_an_activity_when_work_follows_it_and_not_when_it_only_prints() {
        let command = |at: &str, text: &str| typed("c", at, text);
        let skill = command(
            "2026-11-08T10:05:00.000Z",
            "<command-message>simplify</command-message><command-name>/simplify</command-name><command-args>the diff</command-args>",
        );
        let expansion = user(
            "c",
            "2026-11-08T10:05:00.100Z",
            r#""Review the changed code""#,
            r#","isMeta":true"#,
        );
        let mut lines = history();
        lines.extend([skill, expansion]);
        let found = cut(&(lines.join("\n") + "\n"));
        assert_eq!(
            found.opening.map(|o| o.text).as_deref(),
            Some("/simplify the diff")
        );

        let mut lines = history();
        lines.extend([
            command(
                "2026-11-08T10:05:00.000Z",
                "<command-name>/model</command-name>",
            ),
            command(
                "2026-11-08T10:05:00.100Z",
                "<local-command-stdout>Set model</local-command-stdout>",
            ),
        ]);
        let found = cut(&(lines.join("\n") + "\n"));
        assert_eq!(
            found.opening.map(|o| o.text).as_deref(),
            Some("fix the statusline"),
            "printing something doesn't end the activity it came in the middle of"
        );

        let mut lines = history();
        lines.extend([
            command(
                "2026-11-08T10:05:00.000Z",
                "<command-name>/loop</command-name>",
            ),
            said("2026-11-08T10:05:01.000Z", "On it."),
        ]);
        let found = cut(&(lines.join("\n") + "\n"));
        assert_eq!(found.opening.map(|o| o.text).as_deref(), Some("/loop"));
    }

    #[test]
    fn a_queued_message_with_images_keeps_its_text() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let mut lines = history();
        lines.push(
            r#"{"type":"attachment","timestamp":"2026-11-08T10:00:04.000Z","attachment":{"type":"queued_command","prompt":[{"type":"text","text":"look at this"},{"type":"image","source":{}}]}}"#
                .to_string(),
        );
        let detail = read(&write(dir.path(), &lines))
            .expect("reads")
            .expect("an activity");
        assert!(
            matches!(detail.log.log.last(), Some(LogEntry::Queued { text, .. }) if text == "look at this"),
            "{:?}",
            detail.log.log
        );
    }
}
