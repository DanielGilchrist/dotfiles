//! The output file Claude Code keeps for a background shell command.
//!
//! The transcript says where it is, in text a command could have shaped, so
//! nothing is read through a path: a path is opened once, checked on the
//! handle, and only that [`OutputFile`] is read.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use agent_core::activity::TaskId;
use agent_core::ports::{CommandOutput, Cut};
use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::unistd::Uid;
use serde_json::Value;

use crate::{LinePrefixes, Window, read_window, resolved_inside, text_of};

/// How much of the end of a command's output is read.
pub(crate) const OUTPUT_BYTES: u64 = 64 * 1024;

/// How much of each transcript line is parsed when looking for where a
/// command writes. The record that says so is small.
const RECORD_BYTES: u64 = 1024 * 1024;

/// How many sessions are remembered as searched.
const SEARCHED_SESSIONS: usize = 64;

/// How much unsearched transcript one request reads.
const SEARCH_BYTES: u64 = 32 * 1024 * 1024;

const OUTPUT_MARKER: &str = "Output is being written to: ";
const STARTED: &str = "Command running in background with ID: ";
const TIMED_OUT: &str = "Command did not complete within its ";
const MOVED: &str = "moved to the background (ID: ";

/// Why a path the transcript named isn't read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    Gone,
    Outside,
    Misnamed,
    NotRegular,
    /// Claude Code's directory for the task, or the file, belongs to another
    /// user than the one running this.
    NotOurs,
    Unreadable(io::ErrorKind),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gone => f.write_str("there is nothing at the path"),
            Self::Outside => f.write_str("the path is not under the task output directory"),
            Self::Misnamed => f.write_str("the file is not named for the task"),
            Self::NotRegular => f.write_str("the path is not a regular file"),
            Self::NotOurs => f.write_str("the output belongs to another user"),
            Self::Unreadable(kind) => write!(f, "the file can't be opened: {kind}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// A command's output file, opened and known to be where Claude Code writes
/// it (`<root>/claude-<uid>/<project>/<session>/tasks/<task>.output`,
/// resolved through any links), a regular file, and owned, with its
/// directory, by the user running this. It is the handle that was checked,
/// so the path can't be swapped for another file afterwards.
#[derive(Debug)]
pub(crate) struct OutputFile {
    file: File,
}

impl OutputFile {
    /// The only way in. `claimed` is the path the transcript names, `me` the
    /// user running this.
    pub(crate) fn open(
        root: &Path,
        claimed: &Path,
        task: &TaskId,
        me: Uid,
    ) -> Result<Self, Refusal> {
        if !claimed.is_absolute() {
            return Err(Refusal::Outside);
        }
        root.canonicalize()
            .map_err(|e| Refusal::Unreadable(e.kind()))?;
        match std::fs::symlink_metadata(claimed) {
            Ok(meta) if meta.is_symlink() => return Err(Refusal::NotRegular),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Refusal::Gone),
            Err(e) => return Err(Refusal::Unreadable(e.kind())),
        }
        let (path, inside) = match resolved_inside(root, claimed) {
            Ok(Some(found)) => found,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Refusal::Gone),
            Ok(None) | Err(_) => return Err(Refusal::Outside),
        };
        let parts: Vec<&str> = inside
            .components()
            .map(|part| match part {
                Component::Normal(part) => part.to_str().ok_or(Refusal::Outside),
                _ => Err(Refusal::Outside),
            })
            .collect::<Result<_, _>>()?;
        let [user, _project, _session, "tasks", name] = parts[..] else {
            return Err(Refusal::Outside);
        };
        let Some(owner) = user
            .strip_prefix("claude-")
            .and_then(|digits| digits.parse::<u32>().ok())
        else {
            return Err(Refusal::Outside);
        };
        if name != format!("{}.output", task.as_str()) {
            return Err(Refusal::Misnamed);
        }
        let directory = path.ancestors().nth(4).ok_or(Refusal::Outside)?;
        let directory_owner = std::fs::metadata(directory)
            .map_err(|e| Refusal::Unreadable(e.kind()))?
            .uid();
        if owner != me.as_raw() || directory_owner != owner {
            return Err(Refusal::NotOurs);
        }
        let file = open_regular(&path)?;
        let file_owner = file
            .metadata()
            .map_err(|e| Refusal::Unreadable(e.kind()))?
            .uid();
        if file_owner != owner {
            return Err(Refusal::NotOurs);
        }
        Ok(Self { file })
    }

    /// The last `window` bytes. Only a file with no bytes is no output: one
    /// whose window holds no whole line shows the window.
    pub(crate) fn tail(mut self, window: u64) -> io::Result<CommandOutput> {
        let len = self.file.metadata()?.len();
        let Window { bytes, start } = read_window(&mut self.file, len, window)?;
        Ok(decode(&bytes, start))
    }
}

/// `path` opened for reading without following a link or waiting on a pipe,
/// if it is a regular file.
fn open_regular(path: &Path) -> Result<File, Refusal> {
    let flags = (OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK).bits();
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(code) if code == Errno::ELOOP as i32 => Refusal::NotRegular,
            _ if e.kind() == io::ErrorKind::NotFound => Refusal::Gone,
            _ => Refusal::Unreadable(e.kind()),
        })?;
    let regular = file
        .metadata()
        .map_err(|e| Refusal::Unreadable(e.kind()))?
        .is_file();
    if regular {
        Ok(file)
    } else {
        Err(Refusal::NotRegular)
    }
}

/// What `bytes`, read from `start` in the file with one byte before them when
/// `start` isn't 0, say as text. The byte before tells whether the first
/// line starts at the window's edge.
fn decode(bytes: &[u8], start: u64) -> CommandOutput {
    if start == 0 {
        return CommandOutput::of(String::from_utf8_lossy(bytes).into_owned(), Cut::Whole);
    }
    let window = &bytes[1..];
    let first_break = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .filter(|at| at + 1 < bytes.len());
    let (skipped, shown) = if let Some(at) = first_break {
        (at, &bytes[at + 1..])
    } else {
        let half = window
            .iter()
            .take_while(|byte| **byte & 0b1100_0000 == 0b1000_0000)
            .count();
        (half, &window[half..])
    };
    CommandOutput::of(
        String::from_utf8_lossy(shown).into_owned(),
        Cut::Last {
            bytes: window.len() as u64,
            from: start + skipped as u64,
        },
    )
}

/// Where the transcripts read so far say commands write. What each session's
/// transcripts have been searched up to, and what they announced, is kept
/// together per session, so a transcript is only read where it grew.
#[derive(Default)]
pub(crate) struct Locations {
    state: Mutex<Searches>,
}

#[derive(Default)]
struct Searches {
    clock: u64,
    sessions: HashMap<PathBuf, (u64, Arc<Mutex<Session>>)>,
}

/// What looking for where a command writes came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Located {
    At(PathBuf),
    /// Nothing the transcript or its subagents' say.
    Nowhere,
    /// There was more to search than one request reads.
    Searching,
}

/// How far a session's transcript and its subagents' have been searched, and
/// the paths they announced, by task.
#[derive(Default)]
struct Session {
    /// For each transcript, how far its whole lines were searched.
    done: HashMap<PathBuf, u64>,
    found: HashMap<String, PathBuf>,
}

impl Locations {
    /// Where the transcript, or one of its subagents' beside it, says `task`
    /// writes. A request reads at most [`SEARCH_BYTES`] of what hasn't been
    /// searched, and says `Searching` when that left some. A second request
    /// for a session waits for the first and goes on from what it searched.
    pub(crate) fn of(&self, transcript: &Path, task: &TaskId) -> io::Result<Located> {
        let session = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.clock += 1;
            let used = state.clock;
            let (last_used, session) = state.sessions.entry(transcript.to_path_buf()).or_default();
            *last_used = used;
            let session = Arc::clone(session);
            if state.sessions.len() > SEARCHED_SESSIONS
                && let Some(oldest) = state
                    .sessions
                    .iter()
                    .min_by_key(|(_, (used, _))| *used)
                    .map(|(path, _)| path.clone())
            {
                state.sessions.remove(&oldest);
            }
            session
        };
        let mut session = session.lock().unwrap_or_else(PoisonError::into_inner);
        session.search(transcript, task)
    }
}

impl Session {
    fn search(&mut self, transcript: &Path, task: &TaskId) -> io::Result<Located> {
        if let Some(found) = self.found.get(task.as_str()) {
            return Ok(Located::At(found.clone()));
        }
        let mut budget = SEARCH_BYTES;
        let mut unfinished = false;
        let mut files = vec![transcript.to_path_buf()];
        files.extend(subagent_transcripts(transcript)?);
        for file in files {
            let len = match std::fs::metadata(&file) {
                Ok(meta) => meta.len(),
                Err(e) if e.kind() == io::ErrorKind::NotFound && file != transcript => continue,
                Err(e) => return Err(e),
            };
            let done = self.done.entry(file.clone()).or_insert(0);
            if len < *done {
                *done = 0;
            }
            if len == *done {
                continue;
            }
            if budget == 0 {
                unfinished = true;
                continue;
            }
            let until = len.min(*done + budget);
            let mut announced = Vec::new();
            let (searched, read) = search_lines(&file, *done, until, &mut announced)?;
            *done = searched;
            budget = budget.saturating_sub(read);
            unfinished |= until < len;
            for (id, path) in announced {
                self.found.entry(id).or_insert(path);
            }
            if let Some(found) = self.found.get(task.as_str()) {
                return Ok(Located::At(found.clone()));
            }
        }
        Ok(if unfinished {
            Located::Searching
        } else {
            Located::Nowhere
        })
    }
}

/// Searches the whole lines of `file` from `from` up to `until`, pushing what
/// they announce. Gives how far whole lines went, and how much was read. A
/// last line still being written is left for the next time.
fn search_lines(
    file: &Path,
    from: u64,
    until: u64,
    announced: &mut Vec<(String, PathBuf)>,
) -> io::Result<(u64, u64)> {
    let mut file = File::open(file)?;
    file.seek(SeekFrom::Start(from))?;
    let consumed = Rc::new(Cell::new(0));
    let reader = Counting {
        inner: BufReader::new(file.take(until - from)),
        consumed: Rc::clone(&consumed),
    };
    let mut done = from;
    for line in LinePrefixes::new(reader, RECORD_BYTES) {
        let line = line?;
        let whole = line.last() == Some(&b'\n') || line.len() as u64 >= RECORD_BYTES;
        if !whole {
            break;
        }
        done = from + consumed.get();
        announced.extend(announcement(&line));
    }
    Ok((done, consumed.get()))
}

/// A reader that counts what its reader consumed.
struct Counting<R> {
    inner: R,
    consumed: Rc<Cell<u64>>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.consumed.set(self.consumed.get() + read as u64);
        Ok(read)
    }
}

impl<R: BufRead> BufRead for Counting<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.consumed.set(self.consumed.get() + amount as u64);
        self.inner.consume(amount);
    }
}

/// The transcripts of the subagents of the session `transcript` belongs to.
fn subagent_transcripts(transcript: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(transcript.with_extension("").join("subagents")) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let named = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with("agent-")
                    && Path::new(name)
                        .extension()
                        .is_some_and(|ext| ext == "jsonl")
            });
        if named {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// The task and the path a transcript line says it writes to, if the line is
/// the result of the call that started it: a user record whose tool result
/// starts with what Claude Code says when it moves a command to the
/// background, and whose `backgroundTaskId`, if it has one, is that task.
fn announcement(line: &[u8]) -> Option<(String, PathBuf)> {
    if !line
        .windows(OUTPUT_MARKER.len())
        .any(|window| window == OUTPUT_MARKER.as_bytes())
    {
        return None;
    }
    let record: Value = serde_json::from_slice(line).ok()?;
    if record.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let recorded = record
        .pointer("/toolUseResult/backgroundTaskId")
        .and_then(Value::as_str);
    result_texts(&record).into_iter().find_map(|text| {
        let (id, path) = announced_in(&text)?;
        recorded
            .is_none_or(|recorded| recorded == id)
            .then(|| (id.to_string(), PathBuf::from(path)))
    })
}

/// The task and the path a tool result's text, which must start with the
/// announcement, says it writes to.
fn announced_in(text: &str) -> Option<(&str, &str)> {
    let (id, ends) = if let Some(rest) = text.strip_prefix(STARTED) {
        (id_at_start(rest)?, '.')
    } else if text.starts_with(TIMED_OUT) {
        let rest = &text[text.find(MOVED)? + MOVED.len()..];
        (id_at_start(rest)?, ')')
    } else {
        return None;
    };
    let after = &text[text.find(id)? + id.len()..];
    if !after.starts_with(ends) {
        return None;
    }
    let rest = &text[text.find(OUTPUT_MARKER)? + OUTPUT_MARKER.len()..];
    let name = format!("{id}.output");
    let end = rest.find(&name)? + name.len();
    Some((id, &rest[..end]))
}

/// The id `text` starts with.
fn id_at_start(text: &str) -> Option<&str> {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(text.len());
    (end > 0).then(|| &text[..end])
}

/// The text of each tool result in a user record.
fn result_texts(record: &Value) -> Vec<String> {
    let Some(Value::Array(parts)) = record.pointer("/message/content") else {
        return Vec::new();
    };
    parts
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("tool_result"))
        .filter_map(|part| part.get("content"))
        .map(text_of)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn task(id: &str) -> TaskId {
        id.parse().expect("visible")
    }

    fn me() -> Uid {
        nix::unistd::getuid()
    }

    /// A tasks directory as Claude Code lays it out under a scratch root.
    struct Tasks {
        root: tempfile::TempDir,
        dir: PathBuf,
    }

    impl Tasks {
        fn new() -> Self {
            let root = tempfile::tempdir().expect("tempdir");
            let dir = root
                .path()
                .join(format!("claude-{}/-work-repo/session-1/tasks", me()));
            std::fs::create_dir_all(&dir).expect("dirs");
            Self { root, dir }
        }

        fn write(&self, id: &str, bytes: &[u8]) -> PathBuf {
            let path = self.dir.join(format!("{id}.output"));
            std::fs::write(&path, bytes).expect("write");
            path
        }

        fn open(&self, claimed: &Path, id: &str) -> Result<OutputFile, Refusal> {
            OutputFile::open(self.root.path(), claimed, &task(id), me())
        }

        fn tail(&self, id: &str, window: u64) -> CommandOutput {
            let path = self.dir.join(format!("{id}.output"));
            self.open(&path, id)
                .expect("opens")
                .tail(window)
                .expect("reads")
        }
    }

    fn shown(output: &CommandOutput) -> (&str, Cut) {
        match output {
            CommandOutput::Tail(tail) => (tail.text(), tail.cut()),
            other => panic!("not a tail: {other:?}"),
        }
    }

    #[test]
    fn a_file_named_for_the_task_in_the_tasks_directory_is_accepted() {
        let tasks = Tasks::new();
        let path = tasks.write("b1", b"hi\n");
        assert!(tasks.open(&path, "b1").is_ok());
    }

    #[test]
    fn a_path_outside_the_root_is_refused() {
        let tasks = Tasks::new();
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let path = elsewhere
            .path()
            .join(format!("claude-{}/p/s/tasks/b1.output", me()));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(&path, b"x").expect("write");
        assert_eq!(tasks.open(&path, "b1").unwrap_err(), Refusal::Outside);
    }

    #[test]
    fn a_path_that_goes_up_out_of_the_root_is_refused() {
        let tasks = Tasks::new();
        let secret = tempfile::tempdir().expect("tempdir");
        std::fs::write(secret.path().join("b1.output"), b"x").expect("write");
        let sneaky = tasks
            .dir
            .join("../".repeat(32))
            .join(secret.path().strip_prefix("/").expect("absolute"))
            .join("b1.output");
        assert_eq!(tasks.open(&sneaky, "b1").unwrap_err(), Refusal::Outside);
    }

    #[test]
    fn a_file_with_another_name_is_refused() {
        let tasks = Tasks::new();
        let path = tasks.write("other", b"x");
        assert_eq!(tasks.open(&path, "b1").unwrap_err(), Refusal::Misnamed);
    }

    #[test]
    fn a_file_not_in_the_tasks_directory_is_refused() {
        let tasks = Tasks::new();
        let path = tasks
            .root
            .path()
            .join(format!("claude-{}/-work-repo/b1.output", me()));
        std::fs::write(&path, b"x").expect("write");
        assert_eq!(tasks.open(&path, "b1").unwrap_err(), Refusal::Outside);
    }

    #[test]
    fn a_directory_that_is_not_named_for_a_user_is_refused() {
        let tasks = Tasks::new();
        let dir = tasks.root.path().join("other/p/s/tasks");
        std::fs::create_dir_all(&dir).expect("dirs");
        let path = dir.join("b1.output");
        std::fs::write(&path, b"x").expect("write");
        assert_eq!(tasks.open(&path, "b1").unwrap_err(), Refusal::Outside);
    }

    #[test]
    fn a_directory_named_for_another_user_is_refused() {
        let tasks = Tasks::new();
        let dir = tasks
            .root
            .path()
            .join(format!("claude-{}/p/s/tasks", me().as_raw() + 1));
        std::fs::create_dir_all(&dir).expect("dirs");
        let path = dir.join("b1.output");
        std::fs::write(&path, b"x").expect("write");
        assert_eq!(tasks.open(&path, "b1").unwrap_err(), Refusal::NotOurs);
    }

    #[test]
    fn a_symlink_is_refused_even_to_a_file_in_the_tasks_directory() {
        let tasks = Tasks::new();
        let target = tasks.write("other", b"x");
        let link = tasks.dir.join("b1.output");
        symlink(&target, &link).expect("symlink");
        assert_eq!(tasks.open(&link, "b1").unwrap_err(), Refusal::NotRegular);
    }

    #[test]
    fn opening_a_symlink_that_replaced_the_file_after_the_checks_is_refused() {
        let tasks = Tasks::new();
        let key = tasks.write("key", b"secret");
        let link = tasks.dir.join("b1.output");
        symlink(&key, &link).expect("symlink");
        assert_eq!(open_regular(&link).unwrap_err(), Refusal::NotRegular);
    }

    #[test]
    fn opening_a_pipe_that_replaced_the_file_after_the_checks_is_refused_at_once() {
        let tasks = Tasks::new();
        let path = tasks.dir.join("b1.output");
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo runs");
        assert!(made.success());
        assert_eq!(open_regular(&path).unwrap_err(), Refusal::NotRegular);
    }

    #[test]
    fn a_pipe_is_refused_without_waiting_for_a_writer() {
        let tasks = Tasks::new();
        let path = tasks.dir.join("b1.output");
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("mkfifo runs");
        assert!(made.success());
        assert_eq!(tasks.open(&path, "b1").unwrap_err(), Refusal::NotRegular);
    }

    #[test]
    fn a_directory_named_like_the_output_is_refused() {
        let tasks = Tasks::new();
        let path = tasks.dir.join("b1.output");
        std::fs::create_dir(&path).expect("dir");
        assert_eq!(tasks.open(&path, "b1").unwrap_err(), Refusal::NotRegular);
    }

    #[test]
    fn a_task_directory_root_that_cannot_be_resolved_is_a_failure_and_not_a_gone_file() {
        let tasks = Tasks::new();
        let path = tasks.write("b1", b"x");
        let missing = tasks.root.path().join("no-such-root");
        let opened = OutputFile::open(&missing, &path, &task("b1"), me());
        assert_eq!(
            opened.unwrap_err(),
            Refusal::Unreadable(io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn a_missing_file_is_gone() {
        let tasks = Tasks::new();
        let path = tasks.dir.join("b1.output");
        assert_eq!(tasks.open(&path, "b1").unwrap_err(), Refusal::Gone);
    }

    #[test]
    fn a_relative_path_is_refused() {
        let tasks = Tasks::new();
        assert_eq!(
            tasks.open(Path::new("tasks/b1.output"), "b1").unwrap_err(),
            Refusal::Outside
        );
    }

    #[test]
    fn a_small_file_is_read_whole() {
        let tasks = Tasks::new();
        tasks.write("b1", b"one\ntwo\n");
        assert_eq!(shown(&tasks.tail("b1", 1024)), ("one\ntwo\n", Cut::Whole));
    }

    #[test]
    fn only_a_file_with_no_bytes_has_no_output_yet() {
        let tasks = Tasks::new();
        tasks.write("b1", b"");
        assert_eq!(tasks.tail("b1", 1024), CommandOutput::Empty);
    }

    #[test]
    fn a_big_file_gives_only_its_last_whole_lines() {
        let tasks = Tasks::new();
        let mut lines = (0..100_000)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        lines.push('\n');
        tasks.write("b1", lines.as_bytes());
        let output = tasks.tail("b1", 1000);
        let (text, cut) = shown(&output);
        let Cut::Last { bytes, from } = cut else {
            panic!("cut: {cut:?}")
        };
        assert_eq!(bytes, 1000);
        assert_eq!(
            from + text.len() as u64,
            lines.len() as u64,
            "ends where the file does"
        );
        assert!(text.ends_with("line 99999\n"));
        assert!(text.starts_with("line "), "starts mid-line: {text:?}");
        assert!(text.len() < 1000);
        assert!(!text.contains("line 0\n"));
    }

    #[test]
    fn a_window_starting_exactly_on_a_line_keeps_that_whole_line() {
        let tasks = Tasks::new();
        tasks.write("b1", b"dropped\nabcd\nefgh\n");
        let output = tasks.tail("b1", 10);
        assert_eq!(
            shown(&output),
            ("abcd\nefgh\n", Cut::Last { bytes: 10, from: 8 })
        );
    }

    #[test]
    fn a_window_inside_one_long_line_whose_only_break_is_last_is_not_no_output() {
        let tasks = Tasks::new();
        let mut long = "x".repeat(5000);
        long.push('\n');
        tasks.write("b1", long.as_bytes());
        let output = tasks.tail("b1", 100);
        let (text, cut) = shown(&output);
        assert_eq!(
            cut,
            Cut::Last {
                bytes: 100,
                from: 4901
            }
        );
        assert_eq!(text.trim_end().len(), 99);
    }

    #[test]
    fn a_window_with_no_line_break_keeps_its_text_without_a_half_character() {
        let tasks = Tasks::new();
        tasks.write("b1", "éééééé".as_bytes());
        let output = tasks.tail("b1", 5);
        assert_eq!(shown(&output).0, "éé");
    }

    #[test]
    fn bytes_that_are_not_utf8_are_read_lossily() {
        let tasks = Tasks::new();
        tasks.write("b1", b"ok \xff\xfe done\n");
        assert_eq!(
            shown(&tasks.tail("b1", 1024)).0,
            "ok \u{fffd}\u{fffd} done\n"
        );
    }

    #[test]
    fn escape_bytes_are_returned_as_written_for_the_screen_to_strip() {
        let tasks = Tasks::new();
        tasks.write("b1", b"\x1b[31mred\x1b[0m\n");
        assert_eq!(shown(&tasks.tail("b1", 1024)).0, "\x1b[31mred\x1b[0m\n");
    }

    fn announcement_line(id: &str, path: &Path, extra: &str) -> String {
        let text = format!(
            "{STARTED}{id}. Output is being written to: {}. You will be notified.",
            path.display()
        );
        result_line(&text, extra)
    }

    fn result_line(text: &str, extra: &str) -> String {
        format!(
            r#"{{"type":"user"{extra},"message":{{"content":[{{"type":"tool_result","tool_use_id":"t","content":{}}}]}}}}"#,
            serde_json::to_string(text).expect("json")
        )
    }

    fn transcript(lines: &[String]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").expect("write");
        (dir, path)
    }

    fn find(file: &Path, id: &str) -> Option<PathBuf> {
        match Locations::default().of(file, &task(id)).expect("reads") {
            Located::At(path) => Some(path),
            Located::Nowhere | Located::Searching => None,
        }
    }

    const WHERE_IT_WRITES: &str = "/tmp/claude-501/p/s/tasks/b1.output";

    #[test]
    fn the_task_id_field_and_the_text_together_find_the_path() {
        let line = announcement_line(
            "b1",
            Path::new(WHERE_IT_WRITES),
            r#","toolUseResult":{"backgroundTaskId":"b1"}"#,
        );
        let (_dir, file) = transcript(&[line]);
        assert_eq!(find(&file, "b1"), Some(PathBuf::from(WHERE_IT_WRITES)));
    }

    #[test]
    fn the_text_alone_finds_the_path_when_there_is_no_task_id_field() {
        let line = announcement_line("b1", Path::new(WHERE_IT_WRITES), "");
        let (_dir, file) = transcript(&[line]);
        assert_eq!(find(&file, "b1"), Some(PathBuf::from(WHERE_IT_WRITES)));
    }

    #[test]
    fn a_task_id_field_that_names_another_task_than_the_text_finds_nothing() {
        let line = announcement_line(
            "b1",
            Path::new(WHERE_IT_WRITES),
            r#","toolUseResult":{"backgroundTaskId":"zz"}"#,
        );
        let (_dir, file) = transcript(&[line]);
        assert_eq!(find(&file, "b1"), None);
    }

    #[test]
    fn a_command_moved_to_the_background_after_its_timeout_is_found() {
        let text = format!(
            "{TIMED_OUT}120s timeout and was {MOVED}b1). Output is being written to: {WHERE_IT_WRITES}. You will be notified"
        );
        let (_dir, file) = transcript(&[result_line(&text, "")]);
        assert_eq!(find(&file, "b1"), Some(PathBuf::from(WHERE_IT_WRITES)));
    }

    #[test]
    fn only_the_whole_id_matches() {
        let longer = "/tmp/claude-501/p/s/tasks/xb1.output";
        let (_dir, file) = transcript(&[announcement_line("xb1", Path::new(longer), "")]);
        assert_eq!(find(&file, "b1"), None, "ends the same way");
        let (_dir, file) = transcript(&[announcement_line("b12", Path::new(longer), "")]);
        assert_eq!(find(&file, "b1"), None, "starts the same way");
    }

    #[test]
    fn text_that_only_mentions_the_announcement_is_not_one() {
        let quoted =
            format!("notes:\n{STARTED}b1. Output is being written to: {WHERE_IT_WRITES}. You");
        let pid = format!("PID: b1. Output is being written to: {WHERE_IT_WRITES}.");
        let lines = [result_line(&quoted, ""), result_line(&pid, "")];
        let (_dir, file) = transcript(&lines);
        assert_eq!(find(&file, "b1"), None);
    }

    #[test]
    fn a_record_that_is_not_a_users_tool_result_is_not_an_announcement() {
        let text = format!("{STARTED}b1. Output is being written to: {WHERE_IT_WRITES}. You");
        let assistant = result_line(&text, "").replace(r#""type":"user""#, r#""type":"assistant""#);
        let typed = format!(
            r#"{{"type":"user","message":{{"content":{}}}}}"#,
            serde_json::to_string(&text).expect("json")
        );
        let (_dir, file) = transcript(&[assistant, typed]);
        assert_eq!(find(&file, "b1"), None);
    }

    #[test]
    fn a_transcript_that_never_names_the_task_finds_nothing() {
        let line = announcement_line(
            "other",
            Path::new("/tmp/claude-501/p/s/tasks/other.output"),
            "",
        );
        let (_dir, file) = transcript(&[line]);
        assert_eq!(find(&file, "b1"), None);
    }

    #[test]
    fn a_command_a_subagent_started_is_found_in_its_transcript() {
        let (dir, file) = transcript(&["{}".into()]);
        let subagents = dir.path().join("session/subagents");
        std::fs::create_dir_all(&subagents).expect("dirs");
        let line = announcement_line("b1", Path::new(WHERE_IT_WRITES), "");
        std::fs::write(subagents.join("agent-a1.jsonl"), line + "\n").expect("write");
        std::fs::write(subagents.join("notes.txt"), "x").expect("write");
        assert_eq!(find(&file, "b1"), Some(PathBuf::from(WHERE_IT_WRITES)));
    }

    fn at(path: &str) -> Located {
        Located::At(PathBuf::from(path))
    }

    #[test]
    fn a_found_location_is_final_and_needs_no_transcript() {
        let line = announcement_line("b1", Path::new(WHERE_IT_WRITES), "");
        let (_dir, file) = transcript(&[line]);
        let locations = Locations::default();
        assert_eq!(
            locations.of(&file, &task("b1")).expect("reads"),
            at(WHERE_IT_WRITES)
        );
        std::fs::remove_file(&file).expect("remove");
        assert_eq!(
            locations
                .of(&file, &task("b1"))
                .expect("answers from memory"),
            at(WHERE_IT_WRITES)
        );
    }

    #[test]
    fn an_unchanged_miss_is_not_searched_again() {
        let line = announcement_line("b1", Path::new(WHERE_IT_WRITES), "");
        let (_dir, file) = transcript(&[" ".repeat(line.len())]);
        let locations = Locations::default();
        assert_eq!(
            locations.of(&file, &task("b1")).expect("reads"),
            Located::Nowhere
        );
        std::fs::write(&file, line + "\n").expect("same length, now naming it");
        assert_eq!(
            locations.of(&file, &task("b1")).expect("reads"),
            Located::Nowhere,
            "a transcript that hasn't grown isn't read again"
        );
    }

    #[test]
    fn a_session_with_eighty_subagent_transcripts_is_read_once() {
        let line = announcement_line("b1", Path::new(WHERE_IT_WRITES), "");
        let (dir, file) = transcript(&[" ".repeat(line.len())]);
        let subagents = dir.path().join("session/subagents");
        std::fs::create_dir_all(&subagents).expect("dirs");
        for n in 0..80 {
            let filler = " ".repeat(line.len()) + "\n";
            std::fs::write(subagents.join(format!("agent-{n:03}.jsonl")), filler).expect("write");
        }
        let locations = Locations::default();
        assert_eq!(
            locations.of(&file, &task("b1")).expect("reads"),
            Located::Nowhere
        );
        std::fs::write(subagents.join("agent-000.jsonl"), line.clone() + "\n").expect("swap");
        std::fs::write(subagents.join("agent-079.jsonl"), line + "\n").expect("swap");
        assert_eq!(
            locations.of(&file, &task("b1")).expect("reads"),
            Located::Nowhere,
            "none of the 81 transcripts was read again"
        );
    }

    #[test]
    fn a_request_reads_a_bounded_amount_says_searching_and_the_next_request_goes_on() {
        let line = announcement_line("b1", Path::new(WHERE_IT_WRITES), "");
        let (_dir, file) = transcript(&["{}".into()]);
        let mut grown = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .expect("opens");
        let filler = " ".repeat(1024 * 1024) + "\n";
        for _ in 0..(SEARCH_BYTES / (1024 * 1024) + 2) {
            std::io::Write::write_all(&mut grown, filler.as_bytes()).expect("append");
        }
        std::io::Write::write_all(&mut grown, format!("{line}\n").as_bytes()).expect("append");
        let locations = Locations::default();
        assert_eq!(
            locations.of(&file, &task("b1")).expect("reads"),
            Located::Searching
        );
        assert_eq!(
            locations.of(&file, &task("b1")).expect("reads"),
            at(WHERE_IT_WRITES)
        );
    }

    #[test]
    fn a_miss_resumes_where_it_stopped_once_the_transcript_has_grown() {
        let (_dir, file) = transcript(&["{}".into()]);
        let locations = Locations::default();
        assert_eq!(
            locations.of(&file, &task("b1")).expect("reads"),
            Located::Nowhere
        );
        let line = announcement_line("b1", Path::new(WHERE_IT_WRITES), "");
        let mut grown = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .expect("opens");
        std::io::Write::write_all(&mut grown, format!("{line}\n").as_bytes()).expect("append");
        assert_eq!(
            locations.of(&file, &task("b1")).expect("reads"),
            at(WHERE_IT_WRITES)
        );
    }

    #[test]
    fn a_line_still_being_written_is_searched_once_it_is_whole() {
        let line = announcement_line("b1", Path::new(WHERE_IT_WRITES), "");
        let (cut, rest) = line.split_at(line.len() / 2);
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("session.jsonl");
        std::fs::write(&file, format!("{{}}\n{cut}")).expect("write");
        let locations = Locations::default();
        assert_eq!(
            locations.of(&file, &task("b1")).expect("reads"),
            Located::Nowhere
        );
        std::fs::write(&file, format!("{{}}\n{cut}{rest}\n")).expect("write");
        assert_eq!(
            locations.of(&file, &task("b1")).expect("reads"),
            at(WHERE_IT_WRITES)
        );
    }

    #[test]
    fn two_sessions_that_reuse_a_task_id_keep_their_own_paths() {
        let first = "/tmp/claude-501/p/one/tasks/b1.output";
        let second = "/tmp/claude-501/p/two/tasks/b1.output";
        let (_a, one) = transcript(&[announcement_line("b1", Path::new(first), "")]);
        let (_b, two) = transcript(&[announcement_line("b1", Path::new(second), "")]);
        let locations = Locations::default();
        assert_eq!(locations.of(&one, &task("b1")).expect("reads"), at(first));
        assert_eq!(locations.of(&two, &task("b1")).expect("reads"), at(second));
        assert_eq!(locations.of(&one, &task("b1")).expect("reads"), at(first));
    }

    #[test]
    fn evicting_a_session_drops_its_paths_with_it_and_a_fresh_scan_finds_them_again() {
        let locations = Locations::default();
        let (_keep, target) =
            transcript(&[announcement_line("b1", Path::new(WHERE_IT_WRITES), "")]);
        assert_eq!(
            locations.of(&target, &task("b1")).expect("reads"),
            at(WHERE_IT_WRITES)
        );
        let dir = tempfile::tempdir().expect("tempdir");
        for n in 0..SEARCHED_SESSIONS {
            let file = dir.path().join(format!("s{n}.jsonl"));
            std::fs::write(&file, "{}\n").expect("write");
            locations.of(&file, &task("b2")).expect("reads");
        }
        assert!(
            !locations
                .state
                .lock()
                .expect("lock")
                .sessions
                .contains_key(&target)
        );
        assert_eq!(
            locations.of(&target, &task("b1")).expect("reads"),
            at(WHERE_IT_WRITES)
        );
    }

    #[test]
    fn evicting_one_session_leaves_another_sessions_paths() {
        let locations = Locations::default();
        let (_keep, target) =
            transcript(&[announcement_line("b1", Path::new(WHERE_IT_WRITES), "")]);
        let dir = tempfile::tempdir().expect("tempdir");
        for n in 0..SEARCHED_SESSIONS {
            let file = dir.path().join(format!("s{n}.jsonl"));
            std::fs::write(&file, "{}\n").expect("write");
            locations.of(&file, &task("b2")).expect("reads");
            locations.of(&target, &task("b1")).expect("reads");
        }
        std::fs::remove_file(&target).expect("remove");
        assert_eq!(
            locations
                .of(&target, &task("b1"))
                .expect("answers from memory"),
            at(WHERE_IT_WRITES),
            "recently used, so it stayed"
        );
    }

    #[test]
    fn a_request_for_a_session_being_searched_waits_and_goes_on_from_its_progress() {
        let line = announcement_line("b1", Path::new(WHERE_IT_WRITES), "");
        let (_dir, file) = transcript(&["{}".into(), line]);
        let locations = Arc::new(Locations::default());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let (locations, file) = (Arc::clone(&locations), file.clone());
                std::thread::spawn(move || locations.of(&file, &task("b1")).expect("reads"))
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().expect("joins"), at(WHERE_IT_WRITES));
        }
    }

    #[test]
    fn searched_sessions_are_forgotten_beyond_a_bound_oldest_first() {
        let locations = Locations::default();
        let dir = tempfile::tempdir().expect("tempdir");
        let mut first = None;
        for n in 0..=SEARCHED_SESSIONS {
            let file = dir.path().join(format!("s{n}.jsonl"));
            std::fs::write(&file, "{}\n").expect("write");
            locations.of(&file, &task("b1")).expect("reads");
            first.get_or_insert(file);
        }
        let state = locations.state.lock().expect("lock");
        assert_eq!(state.sessions.len(), SEARCHED_SESSIONS);
        assert!(!state.sessions.contains_key(&first.expect("one")));
    }
}
