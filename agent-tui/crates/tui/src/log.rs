//! A log as [0] reads it, a subagent's or the main agent's current activity:
//! entries (the prompt, what it said, calls, runs of calls folded together,
//! an activity's older phases folded to a row), how it is going, the cursor and
//! what is expanded, and the transcript written out for `$EDITOR`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::{self, Write};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use agent_core::ports::{
    ActivityDetail, Call, CallKind, CallState, Log, LogEntry, Outcome, Target, ends_with_report,
};

use agent_core::activity::{AgentId, TranscriptRef};

use crate::touched::{Now, Roots, TouchedFile};

/// Consecutive successful explore calls of one tool fold into one row.
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
    /// A message the user sent while the agent worked.
    Queued(&'a str),
    Compacted,
    Call(&'a Call),
    /// Three or more successful explore calls of one tool.
    Group {
        tool: &'a str,
        calls: Vec<&'a Call>,
    },
    /// The same tool and target more than once in a row. The last call's
    /// outcome stands for all.
    Repeat(Vec<&'a Call>),
    /// A finished stretch of an activity folded to one row: what the agent said
    /// and the calls under it, which open with the cursor.
    Phase(Phase<'a>),
    /// A file the activity touched.
    Touched(&'a TouchedFile),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phase<'a> {
    pub entries: Vec<Entry<'a>>,
    /// From its first step to the next phase's.
    pub took: Option<Duration>,
}

impl<'a> Entry<'a> {
    /// The calls the entry stands for.
    pub fn calls(&self) -> Vec<&'a Call> {
        match self {
            Self::Call(call) => vec![call],
            Self::Group { calls, .. } | Self::Repeat(calls) => calls.clone(),
            Self::Phase(phase) => phase.entries.iter().flat_map(Self::calls).collect(),
            Self::Prompt(_)
            | Self::Said(_)
            | Self::Queued(_)
            | Self::Compacted
            | Self::Touched(_) => Vec::new(),
        }
    }
}

fn explored(call: &Call) -> bool {
    call.kind == CallKind::Explore && matches!(call.state, CallState::Done { .. })
}

/// What names an entry whatever else is listed with it: the log entry it
/// starts at, or the path of a touched file. A cursor and a fold keep to
/// their entry when others come and go.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKey {
    Prompt,
    Touched(PathBuf),
    Log(usize),
}

/// An entry's rows in a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRows {
    pub key: EntryKey,
    pub start: usize,
    pub len: usize,
}

/// A subagent's entries, the prompt first, each with its key.
pub fn entries(detail: &Log) -> Vec<(Entry<'_>, EntryKey)> {
    let prompt = detail
        .prompt
        .as_deref()
        .filter(|p| !p.is_empty())
        .map(|p| (Entry::Prompt(p), EntryKey::Prompt));
    prompt
        .into_iter()
        .chain(
            indexed(&detail.log)
                .into_iter()
                .map(|(entry, from)| (entry, EntryKey::Log(from))),
        )
        .collect()
}

/// The entries of `log`, each with the index of the first log entry it
/// stands for.
fn indexed(log: &[LogEntry]) -> Vec<(Entry<'_>, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < log.len() {
        let call = match &log[at] {
            LogEntry::Said { text, .. } => {
                out.push((Entry::Said(text), at));
                at += 1;
                continue;
            }
            LogEntry::Queued { text, .. } => {
                out.push((Entry::Queued(text), at));
                at += 1;
                continue;
            }
            LogEntry::Compacted => {
                out.push((Entry::Compacted, at));
                at += 1;
                continue;
            }
            LogEntry::Called(call) => call.as_ref(),
        };
        let following = |keep: &dyn Fn(&Call) -> bool| {
            log[at..]
                .iter()
                .map_while(|e| match e {
                    LogEntry::Called(c) if keep(c) => Some(c.as_ref()),
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
            let from = at;
            at += run.len();
            out.push((
                Entry::Group {
                    tool: &call.tool,
                    calls: run,
                },
                from,
            ));
            continue;
        }
        let same = following(&|c| c.tool == call.tool && c.target == call.target);
        let from = at;
        at += same.len();
        out.push((
            if same.len() > 1 {
                Entry::Repeat(same)
            } else {
                Entry::Call(call)
            },
            from,
        ));
    }
    out
}

/// What an activity's log is made of: loose entries, and stretches that each
/// begin with something the agent said.
enum Piece<'a> {
    Loose(Entry<'a>, usize),
    Stretch {
        entries: Vec<(Entry<'a>, usize)>,
        from: usize,
        to: usize,
    },
}

pub fn time_of(entry: &LogEntry) -> Option<SystemTime> {
    match entry {
        LogEntry::Said { at, .. } | LogEntry::Queued { at, .. } => *at,
        LogEntry::Called(call) => call.started,
        LogEntry::Compacted => None,
    }
}

/// The indexes of calls that failed since the last one that succeeded.
fn unrecovered(log: &[LogEntry]) -> HashSet<usize> {
    let succeeded = |e: &LogEntry| matches!(e, LogEntry::Called(c) if matches!(c.state, CallState::Done { .. } | CallState::Flagged { .. }));
    let after = log.iter().rposition(succeeded).map_or(0, |at| at + 1);
    log.iter()
        .enumerate()
        .skip(after)
        .filter(|(_, e)| {
            matches!(e, LogEntry::Called(c) if matches!(c.state, CallState::Failed { .. }))
        })
        .map(|(at, _)| at)
        .collect()
}

/// An activity's entries in the order they are shown.
pub struct ActivityEntries<'a> {
    pub prompt: Option<&'a str>,
    /// The files the activity touched, when the view lists them.
    pub touched: &'a [TouchedFile],
    /// The log with every phase but the latest folded to one entry, each
    /// with the index of the first log entry it stands for.
    pub activity: Vec<(Entry<'a>, usize)>,
}

impl<'a> ActivityEntries<'a> {
    /// Every entry the cursor moves over, in order, with its key.
    pub fn flat(&self) -> Vec<(Entry<'a>, EntryKey)> {
        self.prompt
            .map(|p| (Entry::Prompt(p), EntryKey::Prompt))
            .into_iter()
            .chain(
                self.touched
                    .iter()
                    .map(|f| (Entry::Touched(f), EntryKey::Touched(f.path.clone()))),
            )
            .chain(
                self.activity
                    .iter()
                    .map(|(entry, from)| (entry.clone(), EntryKey::Log(*from))),
            )
            .collect()
    }
}

/// An activity's entries. A phase stays open while it holds a running call or a
/// failure nothing has succeeded after. `closing` is the message the activity
/// ended with, which the view shows apart from the log.
pub fn activity_entries<'a>(
    detail: &'a Log,
    closing: Option<&str>,
    touched: &'a [TouchedFile],
) -> ActivityEntries<'a> {
    let log = match closing {
        Some(closing) if ends_with_report(&detail.log, closing) => {
            &detail.log[..detail.log.len() - 1]
        }
        _ => &detail.log[..],
    };
    let mut pieces: Vec<Piece<'a>> = Vec::new();
    let mut out = Vec::new();
    for (entry, at) in indexed(log) {
        match &log[at] {
            LogEntry::Queued { .. } | LogEntry::Compacted => pieces.push(Piece::Loose(entry, at)),
            LogEntry::Said { .. } => pieces.push(Piece::Stretch {
                entries: vec![(entry, at)],
                from: at,
                to: at + 1,
            }),
            LogEntry::Called(_) => match pieces.last_mut() {
                Some(Piece::Stretch { entries, to, .. }) => {
                    *to = at + entry.calls().len();
                    entries.push((entry, at));
                }
                _ => pieces.push(Piece::Stretch {
                    to: at + entry.calls().len(),
                    from: at,
                    entries: vec![(entry, at)],
                }),
            },
        }
    }
    let latest = pieces
        .iter()
        .rposition(|p| matches!(p, Piece::Stretch { .. }));
    let failed = unrecovered(log);
    let starts: Vec<usize> = pieces
        .iter()
        .map(|p| match p {
            Piece::Loose(_, at) => *at,
            Piece::Stretch { from, .. } => *from,
        })
        .collect();
    for (index, piece) in pieces.into_iter().enumerate() {
        match piece {
            Piece::Loose(entry, at) => out.push((entry, at)),
            Piece::Stretch { entries, from, .. } => {
                let end = starts.get(index + 1).copied().unwrap_or(log.len());
                let held = (Some(index) == latest && closing.is_none())
                    || log[from..end.min(log.len())].iter().enumerate().any(|(i, e)| {
                        matches!(e, LogEntry::Called(c) if matches!(c.state, CallState::Running))
                            || failed.contains(&(from + i))
                    });
                if held {
                    out.extend(entries);
                } else {
                    let began = log.get(from).and_then(time_of);
                    let ended = log.get(end).and_then(time_of);
                    let took = began.zip(ended).and_then(|(a, b)| b.duration_since(a).ok());
                    out.push((
                        Entry::Phase(Phase {
                            entries: entries.into_iter().map(|(e, _)| e).collect(),
                            took,
                        }),
                        from,
                    ));
                }
            }
        }
    }
    ActivityEntries {
        prompt: detail.prompt.as_deref().filter(|p| !p.is_empty()),
        touched,
        activity: out,
    }
}

fn calls(detail: &Log) -> impl Iterator<Item = &Call> {
    detail.log.iter().filter_map(|e| match e {
        LogEntry::Called(c) => Some(c.as_ref()),
        LogEntry::Said { .. } | LogEntry::Queued { .. } | LogEntry::Compacted => None,
    })
}

/// How many calls the log holds.
pub fn call_count(detail: &Log) -> usize {
    calls(detail).count()
}

/// How many of them failed.
pub fn error_count(detail: &Log) -> usize {
    calls(detail)
        .filter(|c| matches!(c.state, CallState::Failed { .. }))
        .count()
}

/// How many failed calls nothing has succeeded after.
pub fn unrecovered_count(detail: &Log) -> usize {
    unrecovered(&detail.log).len()
}

/// How many subagents the log's agent started.
pub fn agent_count(detail: &Log) -> usize {
    delegates(&calls(detail).collect::<Vec<_>>())
}

/// How many of `calls` hand work to a subagent.
pub fn delegates(calls: &[&Call]) -> usize {
    calls
        .iter()
        .filter(|c| matches!(c.kind, CallKind::Delegate { .. }))
        .count()
}

/// Where `⏎` takes the keys from an entry of the activity, instead of folding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Jump {
    /// The subagent a call started.
    Subagent(AgentId),
    /// A file's diff, by its path in the checkout.
    File(PathBuf),
}

pub fn jump_of(entry: &Entry<'_>, roots: &Roots) -> Option<Jump> {
    let call = match entry {
        Entry::Call(call) => call,
        Entry::Repeat(calls) => calls.last()?,
        Entry::Touched(file) => return Some(Jump::File(file.path.clone())),
        Entry::Prompt(_)
        | Entry::Said(_)
        | Entry::Queued(_)
        | Entry::Compacted
        | Entry::Group { .. }
        | Entry::Phase(_) => return None,
    };
    call.spawned().cloned().map(Jump::Subagent).or_else(|| {
        call.touched()
            .map(|path| Jump::File(roots.within(Path::new(path))))
    })
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
pub fn failing(detail: &Log) -> Option<String> {
    let all: Vec<&Call> = calls(detail).collect();
    let mut streaks: HashMap<(&str, String), (usize, usize)> = HashMap::new();
    for (at, call) in all.iter().enumerate() {
        let key = (call.tool.as_str(), format!("{:?}", call.target));
        match call.state {
            CallState::Failed { .. } => {
                let streak = streaks.entry(key).or_insert((0, at));
                *streak = (streak.0 + 1, at);
            }
            CallState::Done { .. } | CallState::Flagged { .. } => {
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

/// The cursor and what is expanded, for one log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nav {
    pub cursor: Option<EntryKey>,
    pub open: BTreeSet<EntryKey>,
    pub all: bool,
}

impl Nav {
    /// Where the cursor is among `entries`, which are not empty. A log
    /// still growing starts at its end, one that is over at its start, and
    /// so does a cursor whose entry is gone.
    pub fn at(&self, running: bool, entries: &[(Entry<'_>, EntryKey)]) -> usize {
        let default = if running { entries.len() - 1 } else { 0 };
        self.cursor
            .as_ref()
            .and_then(|key| entries.iter().position(|(_, k)| k == key))
            .unwrap_or(default)
    }

    pub fn is_open(&self, key: &EntryKey) -> bool {
        self.all || self.open.contains(key)
    }

    pub fn toggle(&mut self, key: EntryKey) {
        if self.all {
            self.all = false;
            self.open.clear();
        } else if !self.open.remove(&key) {
            self.open.insert(key);
        }
    }
}

/// The longest run of backticks in `text`.
fn backticks(text: &str) -> usize {
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
    longest
}

/// `text` as an inline code span that no backtick inside it can end early.
fn code_span(text: &str) -> String {
    let tick = "`".repeat(backticks(text) + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{tick}{pad}{text}{pad}{tick}")
}

/// `text` in a code fence long enough that no run of backticks inside it
/// ends the fence early.
fn fenced(text: &str) -> String {
    let fence = "`".repeat((backticks(text) + 1).max(3));
    format!("{fence}\n{text}\n{fence}")
}

/// One entry of a log as Markdown.
fn entry_markdown(entry: &Entry<'_>) -> String {
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
        Entry::Queued(said) => {
            let _ = write!(text, "### Queued message\n\n{said}\n\n");
        }
        Entry::Compacted => text.push_str("Conversation compacted.\n\n"),
        Entry::Call(call) => single(&mut text, call),
        Entry::Group { calls, .. } | Entry::Repeat(calls) => {
            for call in calls {
                single(&mut text, call);
            }
        }
        Entry::Phase(phase) => {
            for inner in &phase.entries {
                text.push_str(&entry_markdown(inner));
            }
        }
        Entry::Touched(file) => {
            let path = file.path.to_string_lossy();
            let _ = writeln!(text, "- {}{}", code_span(&path), touched_note(file));
        }
    }
    text
}

fn touched_note(file: &TouchedFile) -> String {
    use agent_core::ports::{ChangeKind, Lines};
    let kind = match &file.now {
        Now::Changed(change) => change.kind,
        Now::Clean => return " (no difference now)".into(),
        Now::Unknown => return String::new(),
    };
    match kind {
        ChangeKind::Untracked => " (new)".into(),
        ChangeKind::Tracked {
            lines: Lines::Counted { added, removed },
            ..
        } => format!(" (+{added} \u{2212}{removed})"),
        ChangeKind::Tracked {
            lines: Lines::Binary,
            ..
        } => " (binary)".into(),
        ChangeKind::Tracked {
            lines: Lines::Unknown,
            ..
        } => String::new(),
    }
}

/// A Markdown document, and the line each entry written to it starts on.
struct Document {
    text: String,
    newlines: usize,
    starts: Vec<NonZeroUsize>,
}

impl fmt::Write for Document {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.newlines += text.matches('\n').count();
        self.text.push_str(text);
        Ok(())
    }
}

impl Document {
    /// A document titled `title`, noting where the raw transcript is.
    fn header(title: &str, transcript: &TranscriptRef) -> Self {
        let mut doc = Self {
            text: String::new(),
            newlines: 0,
            starts: Vec::new(),
        };
        let _ = write!(doc, "# {title}\n\n");
        if !transcript.as_str().is_empty() {
            let _ = write!(
                doc,
                "Raw transcript: {}\n\n",
                code_span(transcript.as_str())
            );
        }
        doc
    }

    fn entry(&mut self, entry: &Entry<'_>) {
        self.starts
            .push(NonZeroUsize::MIN.saturating_add(self.newlines));
        let _ = write!(self, "{}", entry_markdown(entry));
    }
}

/// The transcript as Markdown for `$EDITOR`, and the line each entry starts
/// on, in the order [`entries`] lists them.
pub fn markdown(detail: &Log, title: &str) -> (String, Vec<NonZeroUsize>) {
    let mut doc = Document::header(title, &detail.transcript);
    if let Outcome::Finished {
        report: Some(report),
    } = &detail.outcome
    {
        let _ = write!(doc, "## Report\n\n{report}\n\n");
    }
    let _ = write!(doc, "## Calls\n\n");
    for (entry, _) in entries(detail) {
        doc.entry(&entry);
    }
    (doc.text, doc.starts)
}

/// An activity as Markdown for `$EDITOR`: its prompt, the closing message, the
/// files it touched, then the activity, and the line each entry of
/// [`ActivityEntries::flat`] starts on.
pub fn activity_markdown(
    detail: &ActivityDetail,
    title: &str,
    closing: Option<&str>,
    touched: &[TouchedFile],
) -> (String, Vec<NonZeroUsize>) {
    let all = activity_entries(&detail.log, closing, touched);
    let mut doc = Document::header(title, &detail.log.transcript);
    let _ = write!(doc, "## Activity\n\n");
    if let Some(prompt) = all.prompt {
        doc.entry(&Entry::Prompt(prompt));
    }
    if let Some(closing) = closing {
        let _ = write!(doc, "### Closing message\n\n{closing}\n\n");
    }
    if !all.touched.is_empty() {
        let _ = write!(doc, "### Touched files\n\n");
    }
    for file in all.touched {
        doc.entry(&Entry::Touched(file));
    }
    if !all.touched.is_empty() {
        let _ = writeln!(doc);
    }
    let _ = write!(doc, "## Calls\n\n");
    for (entry, _) in &all.activity {
        doc.entry(entry);
    }
    (doc.text, doc.starts)
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
        Entry::Prompt(text) | Entry::Said(text) | Entry::Queued(text) => (*text).to_string(),
        Entry::Compacted => "compacted".to_string(),
        Entry::Call(c) => call(c),
        Entry::Group { calls, .. } | Entry::Repeat(calls) => calls
            .iter()
            .map(|c| call(c))
            .collect::<Vec<_>>()
            .join("\n\n"),
        Entry::Phase(phase) => phase
            .entries
            .iter()
            .map(entry_text)
            .collect::<Vec<_>>()
            .join("\n\n"),
        Entry::Touched(file) => file.path.to_string_lossy().into_owned(),
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
    use agent_core::activity::ToolId;
    use agent_core::ports::{ActivityDetail, Excerpt, Outcome, Summary};

    pub fn call(tool: &str, target: Target, state: CallState) -> Call {
        Call {
            id: ToolId::new(""),
            tool: tool.into(),
            kind: match tool {
                "Read" | "Grep" | "Glob" | "LS" => CallKind::Explore,
                "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => CallKind::Edit,
                "Agent" | "Task" => CallKind::Delegate { spawned: None },
                "AskUserQuestion" => CallKind::Ask,
                _ => CallKind::Other,
            },
            target,
            state,
            started: None,
            input: "input".into(),
            output: "output".into(),
        }
    }

    pub fn edit(path: &str) -> LogEntry {
        LogEntry::Called(Box::new(call(
            "Edit",
            Target::Path {
                path: path.into(),
                range: None,
            },
            CallState::Done {
                summary: Summary::Diff {
                    added: 1,
                    removed: 1,
                },
                took: None,
            },
        )))
    }

    pub fn delegate(task: &str, agent: Option<&str>) -> LogEntry {
        LogEntry::Called(Box::new(Call {
            kind: CallKind::Delegate {
                spawned: agent.map(agent_core::activity::AgentId::new),
            },
            ..call(
                "Agent",
                Target::Task(task.into()),
                CallState::Done {
                    summary: Summary::NoOutput,
                    took: None,
                },
            )
        }))
    }

    pub fn read(path: &str) -> LogEntry {
        LogEntry::Called(Box::new(call(
            "Read",
            Target::Path {
                path: path.into(),
                range: None,
            },
            CallState::Done {
                summary: Summary::Lines(10),
                took: None,
            },
        )))
    }

    pub fn bash(command: &str, state: CallState) -> LogEntry {
        LogEntry::Called(Box::new(call(
            "Bash",
            Target::Command(command.into()),
            state,
        )))
    }

    pub fn failed() -> CallState {
        CallState::Failed {
            code: Some(101),
            excerpt: Excerpt::default(),
            took: None,
        }
    }

    pub fn said(text: &str) -> LogEntry {
        LogEntry::Said {
            text: text.into(),
            at: None,
        }
    }

    pub fn activity_of(log: Vec<LogEntry>) -> agent_core::ports::ActivityDetail {
        agent_core::ports::ActivityDetail {
            log: Log {
                prompt: Some("Fix the statusline.".into()),
                ..detail(log)
            },
            mark: Some(agent_core::ports::ActivityMark::new("p1")),
            progress: agent_core::ports::Progress::Running { asked: None },
        }
    }

    pub fn before(now: SystemTime, secs: u64) -> SystemTime {
        now - Duration::from_secs(secs)
    }

    pub fn call_at(
        now: SystemTime,
        id: &str,
        tool: &str,
        target: Target,
        state: CallState,
        secs: u64,
    ) -> LogEntry {
        let input = match &target {
            Target::Command(c) => c.clone(),
            other => crate::log::target_text(other),
        };
        LogEntry::Called(Box::new(Call {
            id: ToolId::new(id),
            started: Some(before(now, secs)),
            input,
            output: String::new(),
            ..call(tool, target, state)
        }))
    }

    pub fn said_at(now: SystemTime, text: &str, secs: u64) -> LogEntry {
        LogEntry::Said {
            text: text.into(),
            at: Some(before(now, secs)),
        }
    }

    pub fn path(p: &str) -> Target {
        Target::Path {
            path: p.into(),
            range: None,
        }
    }

    pub fn done(summary: Summary) -> CallState {
        CallState::Done {
            summary,
            took: None,
        }
    }

    pub fn diff(added: usize, removed: usize) -> CallState {
        done(Summary::Diff { added, removed })
    }

    /// An activity of four phases: reading, a first edit, a hand-off to two
    /// subagents, and a failing headless run that is being fixed.
    pub fn story(now: SystemTime) -> Vec<LogEntry> {
        let mut log = vec![said_at(
            now,
            "I'll read how the statusline loads its providers first.",
            400,
        )];
        for (i, file) in ["a.lua", "b.lua", "c.lua"].iter().enumerate() {
            log.push(call_at(
                now,
                &format!("r{i}"),
                "Read",
                path(file),
                done(Summary::Lines(40)),
                390,
            ));
        }
        log.push(said_at(
            now,
            "The provider needs an LSP client, so wiring it into lsp.lua.",
            330,
        ));
        log.push(call_at(
            now,
            "e1",
            "Edit",
            path("nvim/plugin/lsp.lua"),
            diff(4, 2),
            320,
        ));
        log.push(said_at(now, "Handing QA to a subagent and reviewing the lsp change in parallel while I write the module.", 300));
        log.push(call_at(
            now,
            "g1",
            "Agent",
            Target::Task("QA pass on statusline".into()),
            done(Summary::Text("2 findings".into())),
            290,
        ));
        log.push(call_at(
            now,
            "g2",
            "Agent",
            Target::Task("Review lsp changes".into()),
            done(Summary::Text("fine".into())),
            290,
        ));
        log.push(said_at(now, "The headless run failed on a nil client. status.lua assumed the client exists before attach, so I'm guarding it and running it again.", 60));
        log.push(call_at(
            now,
            "e2",
            "Edit",
            path("nvim/lua/heucristic/status.lua"),
            diff(9, 14),
            55,
        ));
        log.push(call_at(
            now,
            "b1",
            "Bash",
            Target::Command("nvim --headless -c q".into()),
            CallState::Failed {
                code: Some(1),
                excerpt: Excerpt {
                    lines: vec![
                        "E5113: Error while calling lua chunk: attempt to index a nil value".into(),
                    ],
                    more: 0,
                },
                took: None,
            },
            50,
        ));
        log.push(call_at(
            now,
            "e3",
            "Edit",
            path("nvim/lua/heucristic/status.lua"),
            diff(3, 0),
            40,
        ));
        log
    }

    pub fn activity_at(
        now: SystemTime,
        mut log: Vec<LogEntry>,
        tail: Option<LogEntry>,
    ) -> ActivityDetail {
        log.extend(tail);
        ActivityDetail {
            log: agent_core::ports::Log {
                prompt: Some(
                    "Are you able to run manual QA/UX/quality tests?\nOr have the adversarial agent do so."
                        .into(),
                ),
                started: Some(before(now, 450)),
                last_heard: Some(before(now, 5)),
                ..detail(log)
            },
            ..crate::log::fixtures::activity_of(Vec::new())
        }
    }

    pub fn detail(log: Vec<LogEntry>) -> Log {
        Log {
            prompt: Some("Do it.".into()),
            log,
            transcript: agent_core::activity::TranscriptRef::new(""),
            outcome: Outcome::Running,
            model: None,
            started: None,
            last_heard: None,
            truncated: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use agent_core::ports::{Outcome, Summary};

    #[test]
    fn three_explore_calls_fold_and_two_do_not() {
        let two = detail(vec![read("/a"), read("/b"), said("x")]);
        assert_eq!(entries(&two).len(), 1 + 3);
        let three = detail(vec![read("/a"), read("/b"), read("/c"), said("x")]);
        let found = entries(&three);
        assert!(matches!(&found[1].0, Entry::Group { tool: "Read", calls } if calls.len() == 3));
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn a_failed_read_breaks_a_group() {
        let broken = LogEntry::Called(Box::new(call(
            "Read",
            Target::Path {
                path: "/x".into(),
                range: None,
            },
            failed(),
        )));
        let d = detail(vec![read("/a"), read("/b"), broken, read("/c"), read("/d")]);
        assert!(
            entries(&d)
                .iter()
                .all(|(e, _)| !matches!(e, Entry::Group { .. }))
        );
    }

    #[test]
    fn the_same_call_in_a_row_repeats_and_keeps_its_latest_outcome_last() {
        let ok = CallState::Done {
            summary: Summary::NoOutput,
            took: None,
        };
        let d = detail(vec![
            bash("cargo build", failed()),
            bash("cargo build", ok.clone()),
            bash("ls", ok),
        ]);
        let found = entries(&d);
        assert!(matches!(&found[1].0, Entry::Repeat(calls) if calls.len() == 2));
        assert!(matches!(&found[2].0, Entry::Call(_)));
    }

    #[test]
    fn three_failures_of_one_call_since_it_last_succeeded_read_as_failing() {
        let build = || bash("cargo build -p agent-tui", failed());
        let d = detail(vec![build(), said("again"), build(), build()]);
        assert_eq!(failing(&d).as_deref(), Some("cargo build failed 3 times"));
        let ok = bash(
            "cargo build -p agent-tui",
            CallState::Done {
                summary: Summary::NoOutput,
                took: None,
            },
        );
        let healed = detail(vec![build(), build(), ok, build()]);
        assert_eq!(failing(&healed), None);
        assert_eq!(error_count(&d), 3);
        assert_eq!(call_count(&d), 3);
    }

    #[test]
    fn three_of_the_last_five_calls_failing_reads_as_failing_but_old_failures_do_not() {
        let ok = || {
            bash(
                "ls",
                CallState::Done {
                    summary: Summary::NoOutput,
                    took: None,
                },
            )
        };
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
        old.extend((0..6).map(|i| {
            bash(
                &format!("ok{i}"),
                CallState::Done {
                    summary: Summary::NoOutput,
                    took: None,
                },
            )
        }));
        assert_eq!(failing(&detail(old)), None);
    }

    #[test]
    fn the_markdown_says_where_each_entry_starts() {
        let d = detail(vec![
            said("Hello."),
            bash(
                "ls",
                CallState::Done {
                    summary: Summary::NoOutput,
                    took: None,
                },
            ),
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
        let (two, three) = (EntryKey::Log(2), EntryKey::Log(3));
        let mut nav = Nav::default();
        nav.toggle(two.clone());
        assert!(nav.is_open(&two) && !nav.is_open(&three));
        nav.all = true;
        assert!(nav.is_open(&three));
        nav.toggle(two.clone());
        assert!(!nav.is_open(&two) && !nav.is_open(&three));
    }

    fn flat<'a>(
        detail: &'a ActivityDetail,
        touched: &'a [TouchedFile],
    ) -> Vec<(Entry<'a>, EntryKey)> {
        activity_entries(&detail.log, Some("All done."), touched).flat()
    }

    #[test]
    fn the_cursor_and_an_open_fold_keep_their_entries_when_touched_files_come_and_go() {
        let detail = activity_of(vec![
            said("one"),
            bash("a", ok()),
            said("two"),
            bash("b", ok()),
            said("All done."),
        ]);
        let file = |path: &str| TouchedFile {
            path: path.into(),
            now: Now::Unknown,
        };
        let two = [file("a.rs"), file("b.rs")];
        let three = [file("a.rs"), file("b.rs"), file("c.rs")];
        let mut nav = Nav {
            cursor: Some(EntryKey::Log(2)),
            open: [EntryKey::Log(0)].into(),
            all: false,
        };
        let (before, after) = (flat(&detail, &two), flat(&detail, &three));
        let (was, now) = (nav.at(false, &before), nav.at(false, &after));
        assert_eq!(was + 1, now, "one more row above, so one position down");
        assert_eq!(after[now].1, EntryKey::Log(2), "still on the same phase");
        assert!(
            nav.is_open(&EntryKey::Log(0)),
            "and the fold is still the first phase"
        );
        nav.cursor = Some(EntryKey::Touched("b.rs".into()));
        assert_eq!(
            after[nav.at(false, &after)].1,
            EntryKey::Touched("b.rs".into())
        );
    }

    #[test]
    fn output_with_backticks_gets_a_longer_fence() {
        assert_eq!(fenced("ls"), "```\nls\n```");
        assert_eq!(fenced("a ``` b"), "````\na ``` b\n````");
        assert_eq!(fenced("`````x"), "``````\n`````x\n``````");
        let mut c = call(
            "Bash",
            Target::Command("x".into()),
            CallState::Done {
                summary: Summary::NoOutput,
                took: None,
            },
        );
        c.input = "echo '```'".into();
        c.output = "````\nout".into();
        let mut d = detail(vec![LogEntry::Called(Box::new(c))]);
        d.outcome = Outcome::Finished {
            report: Some("done".into()),
        };
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

    fn running() -> LogEntry {
        bash("cargo test", CallState::Running)
    }

    fn ok() -> CallState {
        CallState::Done {
            summary: Summary::NoOutput,
            took: None,
        }
    }

    fn shapes(found: &ActivityEntries<'_>) -> Vec<String> {
        let prompt = found.prompt.map(|_| "prompt@0".to_string());
        let touched = found.touched.iter().map(|_| "touched".to_string());
        let activity = found.activity.iter().map(|(entry, from)| {
            let kind = match entry {
                Entry::Prompt(_) => "prompt",
                Entry::Said(_) => "said",
                Entry::Queued(_) => "queued",
                Entry::Compacted => "compacted",
                Entry::Call(_) => "call",
                Entry::Group { .. } => "group",
                Entry::Repeat(_) => "repeat",
                Entry::Phase(_) => "phase",
                Entry::Touched(_) => "touched",
            };
            format!("{kind}@{from}")
        });
        prompt.into_iter().chain(touched).chain(activity).collect()
    }

    #[test]
    fn every_phase_but_the_latest_folds_to_one_entry_that_remembers_where_it_starts() {
        let detail = activity_of(vec![
            said("one"),
            bash("a", ok()),
            bash("b", ok()),
            said("two"),
            bash("c", ok()),
            said("three"),
            bash("d", ok()),
        ]);
        let found = activity_entries(&detail.log, None, &[]);
        assert_eq!(
            shapes(&found),
            ["prompt@0", "phase@0", "phase@3", "said@5", "call@6"]
        );
        let (Entry::Phase(first), _) = &found.activity[0] else {
            panic!("{:?}", found.activity[0]);
        };
        assert_eq!(first.entries.len(), 3, "its words and both calls");
    }

    #[test]
    fn a_phase_stays_open_for_a_running_call_or_a_failure_nothing_has_followed() {
        let running_early = activity_of(vec![
            said("one"),
            running(),
            said("two"),
            bash("c", ok()),
            said("three"),
            bash("d", ok()),
        ]);
        assert_eq!(
            shapes(&activity_entries(&running_early.log, None, &[])),
            [
                "prompt@0", "said@0", "call@1", "phase@2", "said@4", "call@5"
            ],
            "parallel work in an older phase isn't hidden"
        );

        let failure_stands = activity_of(vec![
            said("one"),
            bash("a", failed()),
            said("two"),
            bash("b", failed()),
        ]);
        assert_eq!(
            shapes(&activity_entries(&failure_stands.log, None, &[])),
            ["prompt@0", "said@0", "call@1", "said@2", "call@3"]
        );

        let recovered = activity_of(vec![
            said("one"),
            bash("a", failed()),
            said("two"),
            bash("b", ok()),
        ]);
        assert_eq!(
            shapes(&activity_entries(&recovered.log, None, &[])),
            ["prompt@0", "phase@0", "said@2", "call@3"],
            "a failure a later success followed is history"
        );
    }

    #[test]
    fn an_activity_with_a_closing_message_folds_every_phase_and_leaves_the_message_out() {
        let detail = activity_of(vec![
            said("one"),
            bash("a", ok()),
            said("two"),
            bash("b", ok()),
            said("All done. Three files changed."),
        ]);
        let found = activity_entries(
            &detail.log,
            Some("All done. Three files changed. And more."),
            &[],
        );
        assert_eq!(shapes(&found), ["prompt@0", "phase@0", "phase@2"]);
        let unmatched = activity_entries(&detail.log, Some("something else"), &[]);
        assert_eq!(
            shapes(&unmatched),
            ["prompt@0", "phase@0", "phase@2", "phase@4"],
            "a closing message that isn't the agent's last line leaves the log whole, folded"
        );
    }

    #[test]
    fn queued_messages_and_compactions_end_a_phase_and_never_fold() {
        let detail = activity_of(vec![
            said("one"),
            bash("a", ok()),
            LogEntry::Queued {
                text: "also lsp.lua".into(),
                at: None,
            },
            LogEntry::Compacted,
            bash("b", ok()),
        ]);
        assert_eq!(
            shapes(&activity_entries(&detail.log, None, &[])),
            ["prompt@0", "phase@0", "queued@2", "compacted@3", "call@4"]
        );
    }

    #[test]
    fn a_phase_knows_how_long_it_took_from_its_first_step_to_the_next_phases() {
        use std::time::{Duration, SystemTime};
        let at = |secs| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
        let detail = activity_of(vec![
            LogEntry::Said {
                text: "one".into(),
                at: at(100),
            },
            bash("a", ok()),
            LogEntry::Said {
                text: "two".into(),
                at: at(190),
            },
        ]);
        let found = activity_entries(&detail.log, None, &[]);
        let (Entry::Phase(first), _) = &found.activity[0] else {
            panic!("{:?}", found.activity[0]);
        };
        assert_eq!(first.took, Some(Duration::from_secs(90)));
    }

    #[test]
    fn counts_of_agents_and_unrecovered_failures() {
        let agent = LogEntry::Called(Box::new(call("Agent", Target::Task("qa".into()), ok())));
        let detail = detail(vec![
            agent,
            bash("x", failed()),
            bash("y", ok()),
            bash("z", failed()),
        ]);
        assert_eq!(agent_count(&detail), 1);
        assert_eq!((error_count(&detail), unrecovered_count(&detail)), (2, 1));
    }
}
