//! A subagent's log as [0] reads it: entries (the prompt, what it said,
//! calls, runs of calls folded together), how it is going, the cursor and
//! what is expanded, and the transcript written out for `$EDITOR`.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;
use std::num::NonZeroUsize;

use agent_core::ports::{Call, CallState, LogEntry, SubagentDetail, Target};

/// Consecutive successful calls of these tools fold into one row.
const EXPLORE: [&str; 4] = ["Read", "Grep", "Glob", "LS"];
const GROUP_AT: usize = 3;
/// Failures of one call in a row, or of the last [`WINDOW`] calls, that
/// make a subagent read as failing.
const FAILING_AT: usize = 3;
const WINDOW: usize = 5;
/// A failing call has to be this recent, in calls, to still count.
const RECENT: usize = 6;

/// What the cursor moves over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry<'a> {
    Prompt(&'a str),
    Said(&'a str),
    Call(&'a Call),
    /// Three or more successful explore calls of one tool.
    Group {
        tool: &'a str,
        calls: Vec<&'a Call>,
    },
    /// The same tool and target more than once in a row. The last call's
    /// outcome stands for all.
    Repeat(Vec<&'a Call>),
}

fn explored(call: &Call) -> bool {
    EXPLORE.contains(&call.tool.as_str()) && matches!(call.state, CallState::Done(_))
}

pub fn entries(detail: &SubagentDetail) -> Vec<Entry<'_>> {
    let mut out = Vec::new();
    if !detail.prompt.is_empty() {
        out.push(Entry::Prompt(&detail.prompt));
    }
    let mut at = 0;
    while at < detail.log.len() {
        let call = match &detail.log[at] {
            LogEntry::Said { text, .. } => {
                out.push(Entry::Said(text));
                at += 1;
                continue;
            }
            LogEntry::Called(call) => call,
        };
        let following = |keep: &dyn Fn(&Call) -> bool| {
            detail.log[at..]
                .iter()
                .map_while(|e| match e {
                    LogEntry::Called(c) if keep(c) => Some(c),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let run = if explored(call) {
            following(&|c| c.tool == call.tool && explored(c))
        } else {
            Vec::new()
        };
        if run.len() >= GROUP_AT {
            at += run.len();
            out.push(Entry::Group {
                tool: &call.tool,
                calls: run,
            });
            continue;
        }
        let same = following(&|c| c.tool == call.tool && c.target == call.target);
        at += same.len();
        out.push(if same.len() > 1 {
            Entry::Repeat(same)
        } else {
            Entry::Call(call)
        });
    }
    out
}

fn calls(detail: &SubagentDetail) -> impl Iterator<Item = &Call> {
    detail.log.iter().filter_map(|e| match e {
        LogEntry::Called(c) => Some(c),
        LogEntry::Said { .. } => None,
    })
}

/// How many calls the log holds.
pub fn call_count(detail: &SubagentDetail) -> usize {
    calls(detail).count()
}

/// How many of them failed.
pub fn error_count(detail: &SubagentDetail) -> usize {
    calls(detail)
        .filter(|c| matches!(c.state, CallState::Failed { .. }))
        .count()
}

/// A short name for a call, to say what keeps failing.
fn label(call: &Call) -> String {
    match &call.target {
        Target::Command(command) => command
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" "),
        Target::Path { path, .. } => {
            format!("{} {}", call.tool, path.rsplit('/').next().unwrap_or(path))
        }
        _ => call.tool.clone(),
    }
}

/// Why it reads as failing: one call that failed three times since it last
/// succeeded, or three of the last five calls failing.
pub fn failing(detail: &SubagentDetail) -> Option<String> {
    let all: Vec<&Call> = calls(detail).collect();
    let mut streaks: HashMap<(&str, String), (usize, usize)> = HashMap::new();
    for (at, call) in all.iter().enumerate() {
        let key = (call.tool.as_str(), format!("{:?}", call.target));
        match call.state {
            CallState::Failed { .. } => {
                let streak = streaks.entry(key).or_insert((0, at));
                *streak = (streak.0 + 1, at);
            }
            CallState::Done(_) | CallState::Flagged { .. } => {
                streaks.remove(&key);
            }
            CallState::Running => {}
        }
    }
    let worst = streaks
        .into_iter()
        .filter(|(_, (n, at))| *n >= FAILING_AT && all.len() - at <= RECENT)
        .max_by_key(|(_, (n, at))| (*n, *at));
    if let Some((_, (n, at))) = worst {
        return Some(format!("{} failed {n} times", label(all[at])));
    }
    let recent: Vec<&&Call> = all
        .iter()
        .filter(|c| !matches!(c.state, CallState::Running))
        .rev()
        .take(WINDOW)
        .collect();
    let failed = recent
        .iter()
        .filter(|c| matches!(c.state, CallState::Failed { .. }))
        .count();
    (recent.len() == WINDOW && failed >= FAILING_AT)
        .then(|| format!("{failed} of the last {WINDOW} calls failed"))
}

/// The cursor and what is expanded, for one subagent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nav {
    pub cursor: Option<usize>,
    pub open: BTreeSet<usize>,
    pub all: bool,
}

impl Nav {
    pub fn is_open(&self, entry: usize) -> bool {
        self.all || self.open.contains(&entry)
    }

    pub fn toggle(&mut self, entry: usize) {
        if self.all {
            self.all = false;
            self.open.clear();
        } else if !self.open.remove(&entry) {
            self.open.insert(entry);
        }
    }
}

/// What the view and the keys share about [0]'s subagent log.
#[derive(Debug, Default)]
pub struct LogView {
    pub navs: HashMap<
        (
            agent_core::names::SessionName,
            agent_core::activity::AgentId,
        ),
        Nav,
    >,
    /// Scroll to show the cursor's entry at the next draw.
    pub reveal: bool,
    /// Move the cursor onto the page at the next draw.
    pub follow_page: bool,
}

/// `text` in a code fence long enough that no run of backticks inside it
/// ends the fence early.
fn fenced(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}\n{text}\n{fence}")
}

/// The transcript as Markdown for `$EDITOR`, and the line each entry starts
/// on, in the order [`entries`] lists them.
pub fn markdown(detail: &SubagentDetail, title: &str) -> (String, Vec<NonZeroUsize>) {
    let mut out = String::new();
    let mut starts = Vec::new();
    let _ = write!(out, "# {title}\n\n");
    if !detail.transcript.as_os_str().is_empty() {
        let _ = write!(out, "Raw transcript: `{}`\n\n", detail.transcript.display());
    }
    if let Some(report) = &detail.report {
        let _ = write!(out, "## Report\n\n{report}\n\n");
    }
    out.push_str("## Activity\n\n");
    let mut newlines = out.matches('\n').count();
    for entry in entries(detail) {
        starts.push(NonZeroUsize::MIN.saturating_add(newlines));
        let mut text = String::new();
        let single = |text: &mut String, call: &Call| {
            let _ = write!(text, "### {} {}\n\n", call.tool, target_text(&call.target));
            let _ = write!(text, "{}\n\n", fenced(&call.input));
            if !call.output.is_empty() {
                let _ = write!(text, "Result:\n\n{}\n\n", fenced(&call.output));
            }
        };
        match entry {
            Entry::Prompt(prompt) => {
                let _ = write!(text, "### Prompt\n\n{prompt}\n\n");
            }
            Entry::Said(said) => {
                let _ = write!(text, "{said}\n\n");
            }
            Entry::Call(call) => single(&mut text, call),
            Entry::Group { calls, .. } | Entry::Repeat(calls) => {
                for call in calls {
                    single(&mut text, call);
                }
            }
        }
        newlines += text.matches('\n').count();
        out.push_str(&text);
    }
    (out, starts)
}

/// What `y` copies for an entry: the prompt or text, or a call's input and
/// result.
pub fn entry_text(entry: &Entry<'_>) -> String {
    let call = |c: &Call| {
        if c.output.is_empty() {
            c.input.clone()
        } else {
            format!("{}\n\n{}", c.input, c.output)
        }
    };
    match entry {
        Entry::Prompt(text) | Entry::Said(text) => (*text).to_string(),
        Entry::Call(c) => call(c),
        Entry::Group { calls, .. } | Entry::Repeat(calls) => calls
            .iter()
            .map(|c| call(c))
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

/// A target as one plain line.
pub fn target_text(target: &Target) -> String {
    match target {
        Target::None => String::new(),
        Target::Command(c) | Target::Pattern(c) | Target::Url(c) | Target::Other(c) => c.clone(),
        Target::Path { path, range } => match range {
            Some((from, Some(to))) => format!("{path}:{from}-{to}"),
            Some((from, None)) => format!("{path}:{from}-"),
            None => path.clone(),
        },
        Target::Search { pattern, path } => match path {
            Some(path) => format!("\"{pattern}\" in {path}"),
            None => format!("\"{pattern}\""),
        },
        Target::Query(q) => format!("\"{q}\""),
        Target::Task(t) => t.clone(),
        Target::Todos {
            done,
            total,
            current,
        } => match current {
            Some(current) => format!("{done} of {total} done · {current}"),
            None => format!("{done} of {total} done"),
        },
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use agent_core::ports::{Excerpt, Summary};

    pub fn call(tool: &str, target: Target, state: CallState) -> Call {
        Call {
            id: String::new(),
            tool: tool.into(),
            target,
            state,
            started: None,
            took: None,
            input: "input".into(),
            output: "output".into(),
        }
    }

    pub fn read(path: &str) -> LogEntry {
        LogEntry::Called(call(
            "Read",
            Target::Path {
                path: path.into(),
                range: None,
            },
            CallState::Done(Summary::Lines(10)),
        ))
    }

    pub fn bash(command: &str, state: CallState) -> LogEntry {
        LogEntry::Called(call("Bash", Target::Command(command.into()), state))
    }

    pub fn failed() -> CallState {
        CallState::Failed {
            code: Some(101),
            excerpt: Excerpt::default(),
        }
    }

    pub fn said(text: &str) -> LogEntry {
        LogEntry::Said {
            text: text.into(),
            at: None,
        }
    }

    pub fn detail(log: Vec<LogEntry>) -> SubagentDetail {
        SubagentDetail {
            prompt: "Do it.".into(),
            log,
            ..SubagentDetail::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use agent_core::ports::Summary;

    #[test]
    fn three_explore_calls_fold_and_two_do_not() {
        let two = detail(vec![read("/a"), read("/b"), said("x")]);
        assert_eq!(entries(&two).len(), 1 + 3);
        let three = detail(vec![read("/a"), read("/b"), read("/c"), said("x")]);
        let found = entries(&three);
        assert!(matches!(&found[1], Entry::Group { tool: "Read", calls } if calls.len() == 3));
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn a_failed_read_breaks_a_group() {
        let broken = LogEntry::Called(call(
            "Read",
            Target::Path {
                path: "/x".into(),
                range: None,
            },
            failed(),
        ));
        let d = detail(vec![read("/a"), read("/b"), broken, read("/c"), read("/d")]);
        assert!(
            entries(&d)
                .iter()
                .all(|e| !matches!(e, Entry::Group { .. }))
        );
    }

    #[test]
    fn the_same_call_in_a_row_repeats_and_keeps_its_latest_outcome_last() {
        let ok = CallState::Done(Summary::NoOutput);
        let d = detail(vec![
            bash("cargo build", failed()),
            bash("cargo build", ok.clone()),
            bash("ls", ok),
        ]);
        let found = entries(&d);
        assert!(matches!(&found[1], Entry::Repeat(calls) if calls.len() == 2));
        assert!(matches!(&found[2], Entry::Call(_)));
    }

    #[test]
    fn three_failures_of_one_call_since_it_last_succeeded_read_as_failing() {
        let build = || bash("cargo build -p agent-tui", failed());
        let d = detail(vec![build(), said("again"), build(), build()]);
        assert_eq!(failing(&d).as_deref(), Some("cargo build failed 3 times"));
        let ok = bash(
            "cargo build -p agent-tui",
            CallState::Done(Summary::NoOutput),
        );
        let healed = detail(vec![build(), build(), ok, build()]);
        assert_eq!(failing(&healed), None);
        assert_eq!(error_count(&d), 3);
        assert_eq!(call_count(&d), 3);
    }

    #[test]
    fn three_of_the_last_five_calls_failing_reads_as_failing_but_old_failures_do_not() {
        let ok = || bash("ls", CallState::Done(Summary::NoOutput));
        let mixed = detail(vec![
            bash("a", failed()),
            ok(),
            bash("b", failed()),
            ok(),
            bash("c", failed()),
        ]);
        assert_eq!(
            failing(&mixed).as_deref(),
            Some("3 of the last 5 calls failed")
        );
        let mut old = vec![
            bash("a", failed()),
            bash("a", failed()),
            bash("a", failed()),
        ];
        old.extend((0..6).map(|i| bash(&format!("ok{i}"), CallState::Done(Summary::NoOutput))));
        assert_eq!(failing(&detail(old)), None);
    }

    #[test]
    fn the_markdown_says_where_each_entry_starts() {
        let d = detail(vec![
            said("Hello."),
            bash("ls", CallState::Done(Summary::NoOutput)),
        ]);
        let (text, starts) = markdown(&d, "Title");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(starts.len(), 3);
        assert_eq!(lines[starts[0].get() - 1], "### Prompt");
        assert_eq!(lines[starts[1].get() - 1], "Hello.");
        assert_eq!(lines[starts[2].get() - 1], "### Bash ls");
    }

    #[test]
    fn toggling_with_all_open_collapses_everything() {
        let mut nav = Nav::default();
        nav.toggle(2);
        assert!(nav.is_open(2) && !nav.is_open(3));
        nav.all = true;
        assert!(nav.is_open(3));
        nav.toggle(2);
        assert!(!nav.is_open(2) && !nav.is_open(3));
    }

    #[test]
    fn output_with_backticks_gets_a_longer_fence() {
        assert_eq!(fenced("ls"), "```\nls\n```");
        assert_eq!(fenced("a ``` b"), "````\na ``` b\n````");
        assert_eq!(fenced("`````x"), "``````\n`````x\n``````");
        let mut c = call(
            "Bash",
            Target::Command("x".into()),
            CallState::Done(Summary::NoOutput),
        );
        c.input = "echo '```'".into();
        c.output = "````\nout".into();
        let mut d = detail(vec![LogEntry::Called(c)]);
        d.report = Some("done".into());
        let (text, starts) = markdown(&d, "T");
        assert!(text.contains("````\necho '```'\n````\n"), "{text}");
        assert!(text.contains("`````\n````\nout\n`````\n"), "{text}");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[starts[1].get() - 1],
            "### Bash x",
            "starts stay right after a report and a long fence"
        );
    }
}
