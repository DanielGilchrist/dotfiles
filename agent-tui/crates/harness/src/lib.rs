use std::ffi::OsString;
use std::io::{self, BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agent_core::activity::{
    About, AgentId, Background, Common, Input, MainEvent, Reported, SubagentEvent, ToolId,
    TranscriptRef,
};
use agent_core::names::Prompt;
use agent_core::ports::{
    ActivityDetail, Harness, HarnessError, HarnessId, Harnesses, LaunchCtx, Log, Outcome,
    PendingPrompt, Seen, ends_with_report,
};
use agent_core::process::{LaunchSpec, quiet};
use serde::Deserialize;

mod activity;
#[cfg(test)]
mod fixtures;
mod transcript;

const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// The hook events that move an agent's state. Notification only matters
/// for questions: permission prompts arrive as `PermissionRequest` with more
/// detail, and `idle_prompt` fires while background work still runs.
const HOOKS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("PreToolUse", None),
    ("PostToolUse", None),
    ("PostToolUseFailure", None),
    ("PermissionRequest", None),
    ("PermissionDenied", None),
    ("Notification", Some("elicitation_dialog")),
    ("SubagentStart", None),
    ("SubagentStop", None),
    ("Stop", None),
    ("StopFailure", None),
    ("SessionEnd", None),
];
/// How much of a tool's target or a message the TUI keeps.
const SNIPPET: usize = 160;
/// How much of a prompt or an agent's last message the TUI keeps, whole
/// lines and all, for [0] to show it in full.
const LONG: usize = 8000;
/// How much of the end of a subagent's transcript is read for its log, and
/// the most it widens to when that holds no whole record.
const TAIL_BYTES: u64 = 1024 * 1024;
const TAIL_BYTES_MAX: u64 = 8 * 1024 * 1024;
/// How much of its start is read for the prompt.
const HEAD_BYTES: u64 = 256 * 1024;

/// Variables an agent inherits when spawned from inside another Claude Code
/// session; they make Claude treat it as a child and stop writing its
/// transcript, which breaks resuming.
const INHERITED_SESSION_ENV: &[&str] = &[
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

pub struct Claude {
    id: HarnessId,
    /// The `claude` executable: `$AGENT_CLAUDE`, or `claude` on the PATH.
    program: OsString,
    /// The model for one-off questions.
    quick_model: String,
    projects: PathBuf,
    /// Where Claude Code registers its live sessions, one JSON per process.
    sessions: PathBuf,
    permission_mode: String,
    add_dirs: Vec<PathBuf>,
}

/// A live Claude Code process, as its registry entry records it.
#[derive(serde::Deserialize)]
struct Registered {
    pid: i32,
    #[serde(rename = "sessionId")]
    session_id: String,
}

impl Claude {
    pub fn new(home: &Path) -> Self {
        Self::with_program(
            home,
            std::env::var_os("AGENT_CLAUDE").unwrap_or_else(|| "claude".into()),
        )
    }

    fn with_program(home: &Path, program: OsString) -> Self {
        Self {
            id: "claude".parse().expect("valid id"),
            program,
            quick_model: "haiku".into(),
            projects: home.join(".claude/projects"),
            sessions: home.join(".claude/sessions"),
            permission_mode: "auto".into(),
            add_dirs: vec![PathBuf::from("/tmp")],
        }
    }

    /// A one-off `claude` invocation, outside any session this process is
    /// itself running in.
    fn command(&self) -> std::process::Command {
        let mut cmd = quiet(&self.program);
        for key in INHERITED_SESSION_ENV {
            cmd.env_remove(key);
        }
        cmd
    }

    /// Whether the conversation `id` is still running as a background
    /// session: Claude Code keeps a session alive when its terminal goes,
    /// and `--resume` refuses one that's alive. A registry entry whose
    /// process is gone doesn't count.
    fn in_background(&self, id: &str) -> io::Result<bool> {
        let entries = match std::fs::read_dir(&self.sessions) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Ok(registered) =
                serde_json::from_str::<Registered>(&std::fs::read_to_string(&path)?)
            else {
                continue;
            };
            if registered.session_id == id && process_alive(registered.pid) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Stops the background session `id` so it can be resumed with a
    /// prompt, which `attach` can't carry.
    fn stop(&self, id: &str) -> Result<(), HarnessError> {
        let mut cmd = self.command();
        cmd.args(["stop", id]);
        agent_core::process::output_within(&mut cmd, STOP_TIMEOUT).map_err(|e| HarnessError {
            context: format!("stopping background session {id}"),
            source: io::Error::other(e),
        })?;
        Ok(())
    }

    /// Claude Code stores transcripts under the working directory with every
    /// non-alphanumeric character replaced by `-`.
    fn project_dir(&self, workdir: &Path) -> PathBuf {
        let slug: String = workdir
            .to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        self.projects.join(slug)
    }

    /// Transcripts under `workdir`'s project dir.
    fn transcripts(&self, workdir: &Path) -> io::Result<Vec<(PathBuf, std::fs::Metadata)>> {
        let entries = match std::fs::read_dir(self.project_dir(workdir)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut found = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "jsonl") {
                let meta = std::fs::metadata(&path)?;
                found.push((path, meta));
            }
        }
        Ok(found)
    }

    /// The transcript of subagent `id` in any of `workdir`'s sessions.
    fn subagent_file(&self, workdir: &Path, id: &AgentId) -> io::Result<Option<PathBuf>> {
        let id = id.as_str();
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Ok(None);
        }
        let entries = match std::fs::read_dir(self.project_dir(workdir)) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut newest: Option<(SystemTime, PathBuf)> = None;
        for entry in entries {
            let candidate = entry?
                .path()
                .join("subagents")
                .join(format!("agent-{id}.jsonl"));
            let Ok(modified) = std::fs::metadata(&candidate).and_then(|m| m.modified()) else {
                continue;
            };
            if newest.as_ref().is_none_or(|(at, _)| modified > *at) {
                newest = Some((modified, candidate));
            }
        }
        Ok(newest.map(|(_, path)| path))
    }

    /// The transcript of the agent's current conversation: the one the hooks
    /// named, if it is a transcript file under the projects directory, and
    /// otherwise the one
    /// `launch` would resume.
    fn activity_file(
        &self,
        workdir: &Path,
        at: Option<&TranscriptRef>,
    ) -> io::Result<Option<PathBuf>> {
        let projects = self.projects.canonicalize().ok();
        let named = at
            .and_then(|at| at.path().canonicalize().ok())
            .filter(|path| path.extension().is_some_and(|e| e == "jsonl") && path.is_file())
            .filter(|path| projects.as_ref().is_some_and(|dir| path.starts_with(dir)));
        if named.is_some() {
            return Ok(named);
        }
        let workdir = workdir.canonicalize()?;
        Ok(self
            .resumable(&workdir)?
            .map(|id| self.project_dir(&workdir).join(format!("{id}.jsonl"))))
    }

    /// The conversation to resume: the newest transcript written since the
    /// worktree was created that the user has spoken in. Older ones belong
    /// to a previous agent that used the same name. `/clear` starts a new
    /// transcript, so the newest is the one the user means, and a launch
    /// that died before any message leaves one with no user prompt, which is
    /// skipped.
    fn resumable(&self, workdir: &Path) -> io::Result<Option<String>> {
        let born = std::fs::metadata(workdir)?
            .created()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut recent: Vec<(SystemTime, PathBuf)> = self
            .transcripts(workdir)?
            .into_iter()
            .map(|(path, meta)| Ok((meta.modified()?, path)))
            .collect::<io::Result<_>>()?;
        recent.retain(|(modified, _)| *modified >= born);
        recent.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        for (_, path) in recent {
            if !has_user_prompt(&path)? {
                continue;
            }
            if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
                return Ok(Some(id.to_string()));
            }
        }
        Ok(None)
    }
}

impl Harness for Claude {
    fn id(&self) -> &HarnessId {
        &self.id
    }

    fn label(&self) -> &'static str {
        "claude"
    }

    fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchSpec, HarnessError> {
        let workdir = ctx.workdir.canonicalize().map_err(|source| HarnessError {
            context: format!("resolving {}", ctx.workdir.display()),
            source,
        })?;
        let resume = self.resumable(&workdir).map_err(|source| HarnessError {
            context: "looking for a conversation to resume".into(),
            source,
        })?;

        if let Some(id) = &resume
            && self.in_background(id).map_err(|source| HarnessError {
                context: "reading Claude's session registry".into(),
                source,
            })?
        {
            match ctx.bootstrap {
                // Joining keeps everything the session had, hooks
                // included, since this harness launched it.
                None => {
                    return Ok(LaunchSpec::new(&self.program)
                        .cwd(&workdir)
                        .env_remove(INHERITED_SESSION_ENV.iter().copied())
                        .args(["attach".to_string(), id.clone()])
                        .untraced());
                }
                Some(_) => self.stop(id)?,
            }
        }

        let mut spec = LaunchSpec::new(&self.program)
            .cwd(&workdir)
            .env_remove(INHERITED_SESSION_ENV.iter().copied())
            .env("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE", "1");
        for dir in &self.add_dirs {
            spec = spec.arg("--add-dir").arg(dir);
        }
        spec = spec.args(["--permission-mode", self.permission_mode.as_str()]);
        spec = spec
            .arg("--settings")
            .arg(hook_settings(ctx.report, ctx.session.as_str()));
        if let Some(id) = resume {
            spec = spec.args(["--resume".to_string(), id]);
        }
        Ok(match ctx.bootstrap {
            Some(prompt) => spec.arg("--").arg(prompt.as_str()),
            None => spec,
        })
    }

    fn event(&self, payload: &str) -> Option<Reported> {
        claude_event(payload)
    }

    fn acknowledged(
        &self,
        workdir: &Path,
        pending: &PendingPrompt,
    ) -> Result<Option<Seen>, HarnessError> {
        let context = || format!("reading transcripts for {}", workdir.display());
        let workdir = workdir.canonicalize().map_err(|source| HarnessError {
            context: context(),
            source,
        })?;
        let transcripts = self.transcripts(&workdir).map_err(|source| HarnessError {
            context: context(),
            source,
        })?;
        let latest = transcripts
            .iter()
            .filter_map(|(_, meta)| meta.modified().ok())
            .max();
        Ok(latest.filter(|m| *m >= pending.queued).map(Seen::at))
    }

    /// `claude -p` with the quick model: print one answer and exit.
    fn ask(&self, question: &Prompt) -> Result<Option<String>, HarnessError> {
        let mut cmd = self.command();
        cmd.args(["-p", "--model", self.quick_model.as_str()])
            .arg(question.as_str());
        let out = agent_core::process::output_within(&mut cmd, ASK_TIMEOUT).map_err(|e| {
            HarnessError {
                context: "asking claude".into(),
                source: io::Error::other(e),
            }
        })?;
        Ok(out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
    }

    /// Claude Code writes each subagent's transcript beside the session's,
    /// in `<session>/subagents/agent-<id>.jsonl`. The hooks name the
    /// subagent but not the session, so the project's sessions are searched.
    fn subagent(&self, workdir: &Path, id: &AgentId) -> Result<Option<Log>, HarnessError> {
        let workdir = workdir
            .canonicalize()
            .unwrap_or_else(|_| workdir.to_path_buf());
        let context = || format!("reading the transcript of subagent {}", id.as_str());
        let Some(file) = self
            .subagent_file(&workdir, id)
            .map_err(|source| HarnessError {
                context: context(),
                source,
            })?
        else {
            return Ok(None);
        };
        read_subagent(&file)
            .map(Some)
            .map_err(|source| HarnessError {
                context: context(),
                source,
            })
    }

    /// The activity in the transcript the hooks last named, or failing that the
    /// worktree's newest conversation.
    fn activity_detail(
        &self,
        workdir: &Path,
        at: Option<&TranscriptRef>,
    ) -> Result<Option<ActivityDetail>, HarnessError> {
        let context = |source| HarnessError {
            context: "reading the transcript of the current activity".into(),
            source,
        };
        let Some(file) = self.activity_file(workdir, at).map_err(context)? else {
            return Ok(None);
        };
        activity::read(&file).map_err(context)
    }

    /// Bracketed paste keeps a multi-line prompt as one message; Enter submits it.
    fn submit(&self, prompt: &Prompt) -> Vec<u8> {
        format!("\x1b[200~{prompt}\x1b[201~\r").into_bytes()
    }
}

pub struct Registry {
    claude: Claude,
}

impl Registry {
    pub fn new(home: &Path) -> Self {
        Self {
            claude: Claude::new(home),
        }
    }
}

impl Harnesses for Registry {
    fn get(&self, id: &HarnessId) -> Option<&dyn Harness> {
        self.all().into_iter().find(|h| h.id() == id)
    }

    fn default_harness(&self) -> &dyn Harness {
        &self.claude
    }

    fn all(&self) -> Vec<&dyn Harness> {
        vec![&self.claude]
    }
}

fn process_alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

/// How much of each transcript line is looked at. A record can hold a
/// whole file or tool result, and what is searched for comes first.
const LINE_PREFIX_BYTES: u64 = 64 * 1024;

/// The first `max` bytes of each line of `reader`, so a line of any length
/// costs at most that much memory.
struct LinePrefixes<R> {
    reader: R,
    max: u64,
}

impl<R: BufRead> LinePrefixes<R> {
    fn new(reader: R, max: u64) -> Self {
        Self { reader, max }
    }

    fn skip_rest_of_line(&mut self) -> io::Result<()> {
        loop {
            let buf = self.reader.fill_buf()?;
            let end = buf.iter().position(|b| *b == b'\n').map(|at| at + 1);
            let used = end.unwrap_or(buf.len());
            self.reader.consume(used);
            if end.is_some() || used == 0 {
                return Ok(());
            }
        }
    }
}

impl<R: BufRead> Iterator for LinePrefixes<R> {
    type Item = io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut line = Vec::new();
        match (&mut self.reader)
            .take(self.max)
            .read_until(b'\n', &mut line)
        {
            Ok(0) => None,
            Err(e) => Some(Err(e)),
            Ok(_) => {
                let cut = line.last() != Some(&b'\n') && line.len() as u64 == self.max;
                Some(if cut {
                    self.skip_rest_of_line().map(|()| line)
                } else {
                    Ok(line)
                })
            }
        }
    }
}

/// Whether the user has said anything in the transcript at `path`. The
/// first user prompt comes early, so this reads little of a long file.
fn has_user_prompt(path: &Path) -> io::Result<bool> {
    const MARKER: &[u8] = br#""type":"user""#;
    let file = std::fs::File::open(path)?;
    for line in LinePrefixes::new(io::BufReader::new(file), LINE_PREFIX_BYTES) {
        if line?.windows(MARKER.len()).any(|w| w == MARKER) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A subagent's prompt, log and outcome, from its transcript and the
/// `meta.json` beside it. A transcript still being written is read as far
/// as it goes.
fn read_subagent(transcript: &Path) -> io::Result<Log> {
    let mut file = std::fs::File::open(transcript)?;
    let len = file.metadata()?.len();
    let head = read_head(&file)?;
    let tail = read_tail(&mut file, len, TAIL_BYTES, TAIL_BYTES_MAX)?;
    let meta = read_meta(transcript);
    let mut parsed = transcript::parse(tail.text());
    let outcome = outcome_of(&mut parsed, meta.finished.get().copied().unwrap_or(false));
    Ok(Log {
        prompt: head.prompt,
        log: parsed.log,
        transcript: TranscriptRef::new(transcript.to_string_lossy()),
        outcome,
        model: meta.model.0,
        started: head.started,
        last_heard: parsed.last_heard,
        truncated: parsed.dropped || !tail.reaches_start(),
    })
}

/// What Claude Code records beside a subagent's transcript.
#[derive(Deserialize, Default)]
struct Meta {
    #[serde(default)]
    finished: Lenient<bool>,
    #[serde(default)]
    model: Lenient<String>,
}

fn read_meta(transcript: &Path) -> Meta {
    std::fs::read_to_string(transcript.with_extension("meta.json"))
        .ok()
        .and_then(|meta| serde_json::from_str(&meta).ok())
        .unwrap_or_default()
}

struct Head {
    prompt: Option<String>,
    started: Option<SystemTime>,
}

/// The first record of the transcript: the prompt and when it began.
fn read_head(file: &std::fs::File) -> io::Result<Head> {
    let mut first = Vec::new();
    io::BufReader::new(file.take(HEAD_BYTES)).read_until(b'\n', &mut first)?;
    let first = String::from_utf8_lossy(&first);
    let record: Option<serde_json::Value> = serde_json::from_str(first.trim()).ok();
    let started = record
        .as_ref()
        .and_then(|r| r.get("timestamp")?.as_str())
        .and_then(transcript::parse_time);
    let prompt = record.as_ref().and_then(prompt_of);
    Ok(Head { prompt, started })
}

/// How the subagent stands: what it handed back, what it last said once
/// `finished` says it's done, or why it stopped, if it did.
fn outcome_of(parsed: &mut transcript::Parsed, finished: bool) -> Outcome {
    match (parsed.handed_back.take(), finished) {
        (Some(report), _) => Outcome::Finished {
            report: Some(report),
        },
        (None, true) => {
            let said = parsed.last_said.take();
            if said
                .as_deref()
                .is_some_and(|said| ends_with_report(&parsed.log, said))
            {
                parsed.log.pop();
            }
            Outcome::Finished { report: said }
        }
        (None, false) => match parsed.stopped.take() {
            Some(reason) => Outcome::Stopped { reason },
            None => Outcome::Running,
        },
    }
}

/// The whole records read from the end of a transcript.
#[derive(Debug, PartialEq, Eq)]
enum Tail {
    /// They reach the start of the file.
    FromStart(String),
    /// Older records were left unread.
    Partial(String),
}

impl Tail {
    fn text(&self) -> &str {
        match self {
            Self::FromStart(text) | Self::Partial(text) => text,
        }
    }

    fn reaches_start(&self) -> bool {
        matches!(self, Self::FromStart(_))
    }
}

/// The whole records in the last `window` bytes of a `len` byte file,
/// widening the window fourfold up to `max` while it holds none (one huge
/// record).
/// The window reaches one byte back so a record starting exactly at its edge
/// is kept, and the partial one before it is dropped.
fn read_tail(file: &mut std::fs::File, len: u64, mut window: u64, max: u64) -> io::Result<Tail> {
    loop {
        let start = len.saturating_sub(window);
        let back = start.min(1);
        file.seek(SeekFrom::Start(start - back))?;
        let mut bytes = Vec::new();
        file.by_ref().take(window + back).read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes);
        let whole = match (start, text.find('\n')) {
            (0, _) => Some(&text[..]),
            (_, Some(newline)) => Some(&text[newline + 1..]),
            (_, None) => None,
        };
        match whole {
            Some(whole) if whole.contains('\n') || start == 0 || window >= max => {
                let whole = whole.to_string();
                return Ok(if start == 0 {
                    Tail::FromStart(whole)
                } else {
                    Tail::Partial(whole)
                });
            }
            None if window >= max => return Ok(Tail::Partial(String::new())),
            _ => window = (window * 4).min(max),
        }
    }
}

/// The prompt in a subagent transcript's first record.
fn prompt_of(record: &serde_json::Value) -> Option<String> {
    let text = text_of(record.pointer("/message/content")?);
    (!text.is_empty()).then_some(text)
}

/// Message content as text: a string, or the text parts of a list.
fn text_of(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.trim().to_string(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string(),
        _ => String::new(),
    }
}

/// `--settings` JSON adding hooks that report each event to the TUI with
/// `<report> event <session>`. Claude merges hooks from every settings
/// source, so the user's own hooks keep running.
fn hook_settings(report: &Path, session: &str) -> String {
    let command = shell_words::join([report.to_string_lossy().as_ref(), "event", session]);
    let hooks: serde_json::Map<String, serde_json::Value> = HOOKS
        .iter()
        .map(|(event, matcher)| {
            let mut group =
                serde_json::json!({ "hooks": [{ "type": "command", "command": command }] });
            if let Some(matcher) = matcher {
                group["matcher"] = serde_json::Value::from(*matcher);
            }
            ((*event).to_string(), serde_json::Value::Array(vec![group]))
        })
        .collect();
    serde_json::json!({ "hooks": hooks }).to_string()
}

/// The first line of `text`, cut to [`SNIPPET`] characters.
fn snippet(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .chars()
        .take(SNIPPET)
        .collect()
}

/// `text` trimmed and cut to [`LONG`] characters, keeping its lines.
fn long(text: &str) -> String {
    text.trim().chars().take(LONG).collect()
}

/// Prompts Claude Code writes to tell the agent something, like a
/// background subagent finishing.
const NOTIFICATION_PREFIXES: &[&str] = &["<agent-message", "<task-notification"];

/// An id or name Claude Code gave a tool call or a subagent. An empty one
/// would match every other empty one, so a payload carrying one is refused.
struct NonEmpty(String);

impl<'de> Deserialize<'de> for NonEmpty {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.is_empty() {
            return Err(serde::de::Error::custom("empty id"));
        }
        Ok(Self(text))
    }
}

/// A field of a shape that varies by tool or version: the value if it has
/// the expected one, and nothing otherwise.
struct Lenient<T>(Option<T>);

impl<T> Default for Lenient<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<'de, T: serde::de::DeserializeOwned> Deserialize<'de> for Lenient<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(Self(serde_json::from_value(value).ok()))
    }
}

impl<T> Lenient<T> {
    fn get(&self) -> Option<&T> {
        self.0.as_ref()
    }
}

impl Lenient<String> {
    fn text(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

/// A hook payload: who fired it, and which hook.
#[derive(Deserialize)]
struct Payload {
    /// Set when a subagent fired the hook.
    #[serde(default)]
    agent_id: Option<NonEmpty>,
    /// The agent's transcript file.
    #[serde(default)]
    transcript_path: Lenient<String>,
    #[serde(flatten)]
    hook: Hook,
}

/// The hooks that move an agent's state, with the fields read from each.
/// Ones that can't be told apart without an id or a tool name carry it as
/// a required field, so a payload without one fails to parse and is
/// dropped.
#[derive(Deserialize)]
#[serde(tag = "hook_event_name")]
enum Hook {
    SessionStart {
        #[serde(default)]
        source: Option<String>,
    },
    UserPromptSubmit {
        #[serde(default)]
        prompt: Option<String>,
    },
    PreToolUse {
        tool_use_id: NonEmpty,
        tool_name: NonEmpty,
        #[serde(default)]
        tool_input: Lenient<ToolInput>,
    },
    PostToolUse {
        tool_use_id: NonEmpty,
        #[serde(default)]
        tool_input: Lenient<ToolInput>,
        #[serde(default)]
        tool_response: Lenient<ToolResponse>,
    },
    PostToolUseFailure {
        tool_use_id: NonEmpty,
    },
    PermissionRequest {
        tool_name: NonEmpty,
        #[serde(default)]
        tool_input: Lenient<ToolInput>,
    },
    PermissionDenied {
        #[serde(default)]
        tool_use_id: Option<NonEmpty>,
    },
    Notification {
        #[serde(default)]
        message: Option<String>,
    },
    SubagentStart {
        #[serde(default)]
        agent_type: Option<String>,
    },
    SubagentStop {},
    Stop {
        #[serde(default)]
        last_assistant_message: Option<String>,
        #[serde(default)]
        background_tasks: Lenient<Vec<Lenient<BackgroundTask>>>,
    },
    StopFailure {
        #[serde(default)]
        error: Option<String>,
    },
    SessionEnd {},
}

#[derive(Deserialize, Default)]
struct ToolInput {
    #[serde(default)]
    command: Lenient<String>,
    #[serde(default)]
    file_path: Lenient<String>,
    #[serde(default)]
    pattern: Lenient<String>,
    #[serde(default)]
    url: Lenient<String>,
    #[serde(default)]
    description: Lenient<String>,
    #[serde(default)]
    subagent_type: Lenient<String>,
    #[serde(default)]
    questions: Lenient<Vec<Question>>,
}

impl ToolInput {
    /// What a tool is acting on: a command, a file, a search or a URL.
    fn target(&self) -> Option<String> {
        [
            &self.command,
            &self.file_path,
            &self.pattern,
            &self.url,
            &self.description,
        ]
        .into_iter()
        .find_map(Lenient::text)
        .map(snippet)
    }

    fn first_question(&self) -> Option<&str> {
        self.questions.get()?.first()?.question.text()
    }
}

#[derive(Deserialize)]
struct Question {
    #[serde(default)]
    question: Lenient<String>,
}

/// What a tool handed back. Only a backgrounded subagent's is read.
#[derive(Deserialize)]
struct ToolResponse {
    #[serde(default, rename = "agentId")]
    agent_id: Lenient<NonEmpty>,
    #[serde(default)]
    status: Lenient<String>,
    #[serde(default)]
    description: Lenient<String>,
}

impl ToolResponse {
    fn launched(&self) -> Option<&NonEmpty> {
        self.agent_id
            .get()
            .filter(|_| self.status.text() == Some("async_launched"))
    }
}

#[derive(Deserialize)]
struct BackgroundTask {
    #[serde(default)]
    id: Option<NonEmpty>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

/// Who a hook speaks for: the agent, one of its subagents, or either.
enum Routed {
    Main(MainEvent),
    Subagent(SubagentEvent),
    /// The agent started the subagent named here, whoever reports it.
    Launched(AgentId, SubagentEvent),
    Common(Common),
}

/// A hook payload as a [`Reported`] event.
///
/// Claude Code fires no hook while a trust or resume dialog is up or when
/// the user interrupts with Esc, so the core leans on silence for both.
/// Hooks only the agent itself fires, like `Stop`, are dropped when they
/// name a subagent, and the other way round. So are payloads that aren't
/// one of the hooks above or lack an id or tool name they need.
fn claude_event(payload: &str) -> Option<Reported> {
    let Payload {
        agent_id,
        transcript_path,
        hook,
    } = serde_json::from_str(payload).ok()?;
    let agent = agent_id.map(|id| AgentId::new(id.0));
    let transcript = transcript_path
        .text()
        .filter(|path| !path.is_empty())
        .map(TranscriptRef::new);
    let routed = match hook {
        Hook::SessionStart { source } if source.as_deref() == Some("compact") => {
            Routed::Main(MainEvent::Compacted)
        }
        Hook::SessionStart { .. } => Routed::Main(MainEvent::Started),
        Hook::UserPromptSubmit { prompt } => {
            let prompt = prompt.unwrap_or_default();
            let notification = NOTIFICATION_PREFIXES
                .iter()
                .any(|p| prompt.trim_start().starts_with(p));
            Routed::Main(MainEvent::Prompted(if notification {
                Input::Notification
            } else {
                Input::User(long(&prompt))
            }))
        }
        Hook::PreToolUse {
            tool_use_id,
            tool_name,
            tool_input,
        } => pre_tool_use(ToolId::new(tool_use_id.0), tool_name.0, &tool_input),
        Hook::PostToolUse {
            tool_use_id,
            tool_input,
            tool_response,
        } => post_tool_use(ToolId::new(tool_use_id.0), &tool_input, tool_response.get()),
        Hook::PostToolUseFailure { tool_use_id }
        | Hook::PermissionDenied {
            tool_use_id: Some(tool_use_id),
        } => Routed::Common(Common::ToolFinished {
            id: ToolId::new(tool_use_id.0),
        }),
        Hook::PermissionDenied { tool_use_id: None } => Routed::Common(Common::Declined),
        // Its question already arrived with its PreToolUse.
        Hook::PermissionRequest { tool_name, .. } if tool_name.0 == "AskUserQuestion" => {
            return None;
        }
        Hook::PermissionRequest {
            tool_name,
            tool_input,
        } => permission_request(tool_name.0, &tool_input),
        Hook::Notification { message } => Routed::Common(Common::Asked {
            question: snippet(message.as_deref().unwrap_or("has a question")),
            about: About::Unknown,
        }),
        Hook::SubagentStart { agent_type } => Routed::Subagent(SubagentEvent::Started {
            kind: agent_type.unwrap_or_else(|| DEFAULT_SUBAGENT_KIND.to_string()),
        }),
        Hook::SubagentStop {} => Routed::Subagent(SubagentEvent::Stopped),
        Hook::Stop {
            last_assistant_message,
            background_tasks,
        } => Routed::Main(MainEvent::Finished {
            summary: last_assistant_message.as_deref().map(long),
            running: background_subagents(background_tasks.get().map_or(&[][..], Vec::as_slice)),
        }),
        Hook::StopFailure { error } => Routed::Main(MainEvent::Failed(
            error.as_deref().unwrap_or("error").replace('_', " "),
        )),
        Hook::SessionEnd {} => Routed::Main(MainEvent::Ended),
    };
    match (routed, agent) {
        (Routed::Launched(id, event), _) | (Routed::Subagent(event), Some(id)) => {
            Some(Reported::subagent(id, event))
        }
        (Routed::Common(event), Some(id)) => Some(Reported::subagent(id, event)),
        (Routed::Main(event), None) => Some(Reported::main(event).at_transcript(transcript)),
        (Routed::Common(event), None) => Some(Reported::main(event).at_transcript(transcript)),
        (Routed::Main(_), Some(_)) | (Routed::Subagent(_), None) => None,
    }
}

fn pre_tool_use(id: ToolId, tool: String, input: &Lenient<ToolInput>) -> Routed {
    let fallback = ToolInput::default();
    let input = input.get().unwrap_or(&fallback);
    if tool == "AskUserQuestion" {
        return Routed::Common(Common::Asked {
            question: snippet(input.first_question().unwrap_or("has a question")),
            about: About::Tool(id),
        });
    }
    Routed::Common(Common::ToolStarted {
        id,
        tool,
        target: input.target(),
    })
}

/// A finished tool call, or the launch of a background subagent, which
/// names the subagent in its response.
fn post_tool_use(
    tool: ToolId,
    input: &Lenient<ToolInput>,
    response: Option<&ToolResponse>,
) -> Routed {
    let Some((response, launched)) = response.and_then(|r| Some((r, r.launched()?))) else {
        return Routed::Common(Common::ToolFinished { id: tool });
    };
    let input = input.get();
    let description = response
        .description
        .text()
        .or(input.and_then(|i| i.description.text()));
    let kind = input
        .and_then(|i| i.subagent_type.text())
        .unwrap_or(DEFAULT_SUBAGENT_KIND);
    Routed::Launched(
        AgentId::new(launched.0.clone()),
        SubagentEvent::Launched {
            tool,
            kind: kind.to_string(),
            description: snippet(description.unwrap_or_default()),
        },
    )
}

fn permission_request(tool: String, input: &Lenient<ToolInput>) -> Routed {
    let target = input.get().and_then(ToolInput::target);
    Routed::Common(Common::Asked {
        question: match &target {
            Some(target) => format!("{tool}: {target}"),
            None => tool.clone(),
        },
        about: About::Call { tool, target },
    })
}

const DEFAULT_SUBAGENT_KIND: &str = "subagent";

/// The subagents a finished activity left running. Other background work
/// (shells, monitors) doesn't hand anything back to the agent. A task that
/// can't be read is skipped without hiding the others.
fn background_subagents(tasks: &[Lenient<BackgroundTask>]) -> Vec<Background> {
    tasks
        .iter()
        .filter_map(Lenient::get)
        .filter(|task| task.kind.as_deref() == Some("subagent"))
        .filter(|task| matches!(task.status.as_deref(), None | Some("running" | "pending")))
        .filter_map(|task| {
            Some(Background {
                id: AgentId::new(task.id.as_ref()?.0.clone()),
                kind: task
                    .agent_type
                    .clone()
                    .unwrap_or_else(|| DEFAULT_SUBAGENT_KIND.to_string()),
                description: task.description.as_deref().map(snippet),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::ports::{CallState, LogEntry};

    #[test]
    fn project_dir_matches_claude_codes_slugs() {
        let claude = Claude::new(Path::new("/h"));
        assert_eq!(
            claude.project_dir(Path::new("/Users/d/.config")),
            Path::new("/h/.claude/projects/-Users-d--config")
        );
        assert_eq!(
            claude.project_dir(Path::new("/Users/d/repos/tanda_cli")),
            Path::new("/h/.claude/projects/-Users-d-repos-tanda-cli")
        );
    }

    #[test]
    fn launches_fresh_with_the_bootstrap_then_resumes_the_newest_spoken_transcript() {
        let home = tempfile::tempdir().expect("a temp dir");
        let work = tempfile::tempdir().expect("a temp dir");
        let claude = Claude::with_program(home.path(), "/usr/bin/true".into());

        let go: Prompt = "--go".parse().expect("parses");
        let session = "fix".parse().expect("parses");
        let ctx = |bootstrap| LaunchCtx {
            session: &session,
            workdir: work.path(),
            bootstrap,
            report: Path::new("/bin/agent"),
        };
        let fresh = claude.launch(&ctx(Some(&go))).expect("succeeds");
        let tail: Vec<_> = fresh.arguments().iter().rev().take(2).collect();
        assert_eq!(
            tail,
            ["--go", "--"],
            "the prompt can't be mistaken for a flag"
        );
        assert!(
            fresh
                .removed_environment()
                .iter()
                .any(|k| k == "CLAUDECODE")
        );

        let dir = claude.project_dir(&work.path().canonicalize().expect("exists"));
        std::fs::create_dir_all(&dir).expect("creates");
        let user = r#"{"type":"user","message":{"role":"user","content":"hi"}}"#;
        let at = |secs: u64, name: &str, body: &str| {
            let path = dir.join(format!("{name}.jsonl"));
            std::fs::write(&path, body).expect("writes");
            std::fs::File::open(&path)
                .expect("opens")
                .set_modified(SystemTime::now() + std::time::Duration::from_secs(secs))
                .expect("sets mtime");
        };
        at(
            1,
            "before-clear",
            &format!("{user}\n{user}\n{user}\n{user}\n"),
        );
        at(2, "after-clear", &format!("{user}\n"));
        at(3, "died-at-launch", r#"{"type":"summary"}"#);
        let resumed = claude.launch(&ctx(None)).expect("succeeds");
        let args = resumed.arguments();
        assert_eq!(
            args[args.len() - 2..],
            ["--resume", "after-clear"],
            "the newest the user spoke in, not the largest or the empty newer one"
        );

        // A session Claude Code kept running in the background is attached
        // to, unless there's a prompt to deliver, which attach can't carry.
        let sessions = home.path().join(".claude/sessions");
        std::fs::create_dir_all(&sessions).expect("creates");
        let register = |file: &str, pid: u32| {
            std::fs::write(
                sessions.join(file),
                format!(r#"{{"pid":{pid},"sessionId":"after-clear","cwd":"x"}}"#),
            )
            .expect("writes");
        };
        register("dead.json", 999_999_999);
        let args = claude.launch(&ctx(None)).expect("succeeds");
        assert_eq!(
            args.arguments()[args.arguments().len() - 2..],
            ["--resume", "after-clear"],
            "a registry entry whose process is gone doesn't count"
        );
        register("alive.json", std::process::id());
        let attached = claude.launch(&ctx(None)).expect("succeeds");
        assert_eq!(attached.arguments(), ["attach", "after-clear"]);
        let stopped = claude.launch(&ctx(Some(&go))).expect("succeeds");
        let args = stopped.arguments();
        assert_eq!(
            args[args.len() - 4..],
            ["--resume", "after-clear", "--", "--go"],
            "with a prompt the background session is stopped and resumed"
        );
    }

    #[test]
    fn launches_report_every_state_change_through_the_agent_binary() {
        let settings: serde_json::Value =
            serde_json::from_str(&hook_settings(Path::new("/my bin/agent"), "fix")).expect("json");
        let command = settings
            .pointer("/hooks/Stop/0/hooks/0/command")
            .and_then(serde_json::Value::as_str);
        assert_eq!(command, Some("'/my bin/agent' event fix"));
        assert_eq!(
            settings
                .pointer("/hooks/Notification/0/matcher")
                .and_then(serde_json::Value::as_str),
            Some("elicitation_dialog")
        );
    }

    #[test]
    fn hook_payloads_become_events() {
        let event = claude_event;
        let main = |e: MainEvent| Some(Reported::main(e));
        let common = |c: Common| Some(Reported::main(c));
        assert_eq!(
            event(
                r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"cargo test\n--all"}}"#
            ),
            common(Common::ToolStarted {
                id: ToolId::new("t1"),
                tool: "Bash".into(),
                target: Some("cargo test".into())
            })
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf tmp"}}"#
            ),
            common(Common::Asked {
                question: "Bash: rm -rf tmp".into(),
                about: About::Call {
                    tool: "Bash".into(),
                    target: Some("rm -rf tmp".into())
                }
            })
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_use_id":"q1","tool_input":{"questions":[{"question":"Which DB?"}]}}"#
            ),
            common(Common::Asked {
                question: "Which DB?".into(),
                about: About::Tool(ToolId::new("q1"))
            })
        );
        assert_eq!(
            event(r#"{"hook_event_name":"SessionStart","source":"compact"}"#),
            main(MainEvent::Compacted)
        );
        assert_eq!(
            event(r#"{"hook_event_name":"SessionStart","source":"resume"}"#),
            main(MainEvent::Started)
        );
        assert_eq!(
            event(r#"{"hook_event_name":"StopFailure","error":"rate_limit"}"#),
            main(MainEvent::Failed("rate limit".into()))
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"UserPromptSubmit","prompt":"<agent-message from=\"a1\">done</agent-message>"}"#
            ),
            main(MainEvent::Prompted(Input::Notification))
        );
    }

    #[test]
    fn subagent_payloads_say_which_subagent_they_came_from() {
        let event = claude_event;
        let a1 = |e: SubagentEvent| Some(Reported::subagent(AgentId::new("a1"), e));
        let a1_common = |c: Common| Some(Reported::subagent(AgentId::new("a1"), c));
        assert_eq!(
            event(
                r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","agent_id":"a1","tool_input":{"command":"ls"}}"#
            ),
            a1_common(Common::Asked {
                question: "Bash: ls".into(),
                about: About::Call {
                    tool: "Bash".into(),
                    target: Some("ls".into())
                }
            })
        );
        assert_eq!(
            event(r#"{"hook_event_name":"SubagentStart","agent_id":"a1","agent_type":"Explore"}"#),
            a1(SubagentEvent::Started {
                kind: "Explore".into()
            })
        );
        assert_eq!(
            event(r#"{"hook_event_name":"SubagentStop","agent_id":"a1"}"#),
            a1(SubagentEvent::Stopped)
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"PostToolUse","tool_name":"Agent","agent_id":null,"tool_use_id":"t9","tool_input":{"subagent_type":"general-purpose","description":"review"},"tool_response":{"status":"async_launched","agentId":"a1","description":"Adversarial review"}}"#
            ),
            a1(SubagentEvent::Launched {
                tool: ToolId::new("t9"),
                kind: "general-purpose".into(),
                description: "Adversarial review".into()
            })
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"Stop","agent_id":null,"last_assistant_message":"Waiting.","background_tasks":[{"id":"a1","type":"subagent","status":"running","agent_type":"Explore","description":"look"},{"id":"s1","type":"shell","status":"running"},{"id":"a2","type":"subagent","status":"completed"}]}"#
            ),
            Some(Reported::main(MainEvent::Finished {
                summary: Some("Waiting.".into()),
                running: vec![Background {
                    id: AgentId::new("a1"),
                    kind: "Explore".into(),
                    description: Some("look".into())
                }]
            }))
        );
    }

    #[test]
    fn hooks_only_one_side_fires_are_dropped_from_the_other() {
        let event = claude_event;
        assert_eq!(event(r#"{"hook_event_name":"Stop","agent_id":"a1"}"#), None);
        assert_eq!(event(r#"{"hook_event_name":"SubagentStop"}"#), None);
    }

    #[test]
    fn hooks_missing_an_id_they_need_are_dropped() {
        for payload in [
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash"}"#,
            r#"{"hook_event_name":"PreToolUse","tool_use_id":"t1"}"#,
            r#"{"hook_event_name":"PreToolUse","tool_use_id":"t1","tool_name":""}"#,
            r#"{"hook_event_name":"PermissionRequest","tool_input":{"command":"ls"}}"#,
            r#"{"hook_event_name":"PermissionDenied","tool_use_id":""}"#,
            r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":""}"#,
            r#"{"hook_event_name":"PostToolUse","tool_input":{}}"#,
            r#"{"hook_event_name":"PostToolUseFailure"}"#,
            r#"{"hook_event_name":"SubagentStart","agent_id":""}"#,
            r#"{"hook_event_name":"Unheard","tool_use_id":"t1"}"#,
            r#"{"tool_use_id":"t1"}"#,
            "not json",
        ] {
            assert_eq!(claude_event(payload), None, "{payload}");
        }
    }

    #[test]
    fn a_denial_without_a_call_is_a_decline() {
        assert_eq!(
            claude_event(r#"{"hook_event_name":"PermissionDenied"}"#),
            Some(Reported::main(Common::Declined))
        );
    }

    #[test]
    fn one_unreadable_background_task_does_not_hide_the_others() {
        assert_eq!(
            claude_event(
                r#"{"hook_event_name":"Stop","background_tasks":[7,{"id":"","type":"subagent"},{"id":"a1","type":"subagent"}]}"#
            ),
            Some(Reported::main(MainEvent::Finished {
                summary: None,
                running: vec![Background {
                    id: AgentId::new("a1"),
                    kind: "subagent".into(),
                    description: None
                }]
            }))
        );
    }

    #[test]
    fn fields_of_an_unexpected_shape_are_left_out_not_fatal() {
        assert_eq!(
            claude_event(
                r#"{"hook_event_name":"PostToolUse","tool_use_id":"t1","tool_input":"ls","tool_response":"done"}"#
            ),
            Some(Reported::main(Common::ToolFinished {
                id: ToolId::new("t1")
            }))
        );
        assert_eq!(
            claude_event(
                r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":7,"url":"http://x"}}"#
            ),
            Some(Reported::main(Common::ToolStarted {
                id: ToolId::new("t1"),
                tool: "Bash".into(),
                target: Some("http://x".into())
            }))
        );
    }

    #[test]
    fn line_prefixes_bound_a_long_line_and_keep_the_ones_after() {
        let text = format!("{}\nshort\nlast", "x".repeat(50));
        let lines: Vec<Vec<u8>> = LinePrefixes::new(text.as_bytes(), 10)
            .collect::<io::Result<_>>()
            .expect("reads");
        assert_eq!(
            lines,
            [
                b"xxxxxxxxxx".to_vec(),
                b"short\n".to_vec(),
                b"last".to_vec()
            ]
        );
        let exact: Vec<Vec<u8>> = LinePrefixes::new(&b"abcd"[..], 4)
            .collect::<io::Result<_>>()
            .expect("reads");
        assert_eq!(exact, [b"abcd".to_vec()]);
    }

    #[test]
    fn a_user_prompt_after_a_huge_record_is_still_found() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("t.jsonl");
        let huge = "y".repeat(usize::try_from(LINE_PREFIX_BYTES).expect("fits") * 3);
        std::fs::write(&path, format!("{{\"type\":\"queue\",\"x\":\"{huge}\"}}\n"))
            .expect("writes");
        assert!(!has_user_prompt(&path).expect("reads"));
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("opens");
        std::io::Write::write_all(&mut file, br#"{"type":"user","message":{}}"#).expect("writes");
        assert!(has_user_prompt(&path).expect("reads"));
    }

    #[test]
    fn a_tail_says_whether_it_reaches_the_start() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "aaaa\nbbbb\n").expect("writes");
        let mut file = std::fs::File::open(&path).expect("opens");
        assert_eq!(
            read_tail(&mut file, 10, 100, 100).expect("reads"),
            Tail::FromStart("aaaa\nbbbb\n".into())
        );
        assert_eq!(
            read_tail(&mut file, 10, 5, 5).expect("reads"),
            Tail::Partial("bbbb\n".into())
        );
    }

    const PROMPT_LINE: &str = r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"Review the diff.\nBe harsh."}}"#;
    const CALL_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking."},{"type":"tool_use","id":"tu1","name":"Read","input":{"file_path":"/a/b.rs"}}]}}"#;
    const RESULT_LINE: &str = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"fn main() {}\nmore"}]}}"#;
    const LATER_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu2","name":"Bash","input":{"command":"cargo test"}}]}}"#;

    #[test]
    fn a_subagent_transcript_reads_as_a_prompt_and_a_log() {
        assert_eq!(
            serde_json::from_str(PROMPT_LINE)
                .ok()
                .and_then(|r| prompt_of(&r))
                .as_deref(),
            Some("Review the diff.\nBe harsh.")
        );
        let text =
            format!("{PROMPT_LINE}\n{CALL_LINE}\n{RESULT_LINE}\n{LATER_LINE}\n{{\"type\":\"assi");
        let log = transcript::parse(&text).log;
        assert_eq!(log.len(), 3, "a record cut off mid-write is skipped");
        assert!(matches!(&log[0], LogEntry::Said { text, .. } if text == "Looking."));
        assert!(
            matches!(&log[2], LogEntry::Called(c) if c.state == CallState::Running && c.tool == "Bash")
        );
    }

    #[test]
    fn the_log_keeps_only_the_newest_entries() {
        let text: String = (0..transcript::LOG_ENTRIES + 5)
            .map(|i| {
                format!(
                    r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"t{i}"}}]}}}}"#
                ) + "\n"
            })
            .collect();
        let parsed = transcript::parse(&text);
        assert_eq!(parsed.log.len(), transcript::LOG_ENTRIES);
        assert!(parsed.dropped);
        assert!(matches!(&parsed.log[0], LogEntry::Said { text, .. } if text == "t5"));
    }

    #[test]
    fn a_finished_subagent_reports_its_last_message_and_its_model_and_times() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("agent-a1.jsonl");
        let first = r#"{"type":"user","timestamp":"2026-11-08T10:00:00.000Z","message":{"content":"Do it."}}"#;
        let last = r#"{"type":"assistant","timestamp":"2026-11-08T10:06:02.000Z","message":{"content":[{"type":"text","text":"Done: all green."}]}}"#;
        std::fs::write(&path, format!("{first}\n{last}\n")).expect("writes");
        std::fs::write(
            dir.path().join("agent-a1.meta.json"),
            r#"{"finished":true,"model":"sonnet"}"#,
        )
        .expect("writes");
        let detail = read_subagent(&path).expect("reads");
        assert_eq!(
            detail.outcome,
            Outcome::Finished {
                report: Some("Done: all green.".into())
            }
        );
        assert_eq!(detail.log, [], "the report isn't also an entry");
        assert_eq!(detail.model.as_deref(), Some("sonnet"));
        assert!(!detail.truncated);
        let took = detail
            .last_heard
            .zip(detail.started)
            .map(|(end, start)| end.duration_since(start).expect("after").as_secs());
        assert_eq!(took, Some(362));

        std::fs::remove_file(dir.path().join("agent-a1.meta.json")).expect("removes");
        let running = read_subagent(&path).expect("reads");
        assert_eq!(running.outcome, Outcome::Running);
        assert_eq!(
            running.log.len(),
            1,
            "unfinished: its last words stay in the log"
        );
    }

    #[test]
    fn a_subagent_is_running_stopped_or_finished_and_never_two_at_once() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("agent-a1.jsonl");
        let meta = dir.path().join("agent-a1.meta.json");
        let first = r#"{"type":"user","timestamp":"2026-11-08T10:00:00.000Z","message":{"content":"Do it."}}"#;
        let error = r#"{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-11-08T10:00:05.000Z","message":{"content":[{"type":"text","text":"API Error: overloaded"}]}}"#;
        std::fs::write(&path, format!("{first}\n{error}\n")).expect("writes");
        assert_eq!(
            read_subagent(&path).expect("reads").outcome,
            Outcome::Stopped {
                reason: "API Error: overloaded".into()
            }
        );
        std::fs::write(&meta, r#"{"finished":true}"#).expect("writes");
        assert!(
            matches!(
                read_subagent(&path).expect("reads").outcome,
                Outcome::Finished { .. }
            ),
            "a finished subagent isn't stopped"
        );
    }

    #[test]
    fn subagents_are_found_by_id_in_any_session_and_missing_or_partial_files_dont_fail() {
        let home = tempfile::tempdir().expect("a temp dir");
        let work = tempfile::tempdir().expect("a temp dir");
        let claude = Claude::with_program(home.path(), "/usr/bin/true".into());
        let work_path = work.path().canonicalize().expect("exists");
        let id = AgentId::new("a1");
        assert_eq!(claude.subagent(&work_path, &id).expect("no project"), None);

        let dir = claude.project_dir(&work_path).join("sess1/subagents");
        std::fs::create_dir_all(&dir).expect("creates");
        assert_eq!(claude.subagent(&work_path, &id).expect("no file"), None);

        let transcript = dir.join("agent-a1.jsonl");
        std::fs::write(&transcript, format!("{PROMPT_LINE}\n{CALL_LINE}\n{{\"type"))
            .expect("writes");
        std::fs::write(dir.join("agent-a1.meta.json"), r#"{"finished":true}"#).expect("writes");
        let detail = claude
            .subagent(&work_path, &id)
            .expect("reads")
            .expect("found");
        assert_eq!(
            detail.prompt.as_deref(),
            Some("Review the diff.\nBe harsh.")
        );
        assert!(matches!(detail.outcome, Outcome::Finished { .. }));
        assert_eq!(detail.log.len(), 2);

        std::fs::write(&transcript, "").expect("writes");
        let empty = claude
            .subagent(&work_path, &id)
            .expect("reads")
            .expect("found");
        assert_eq!(empty.prompt, None);
        assert_eq!(empty.log, []);

        let hostile = AgentId::new("../x");
        assert_eq!(
            claude.subagent(&work_path, &hostile).expect("refused"),
            None
        );
    }

    #[test]
    fn the_tail_keeps_whole_records_widens_for_a_huge_one_and_keeps_one_on_the_edge() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let tail = |body: &str, window: u64, max: u64| {
            let path = dir.path().join("t.jsonl");
            std::fs::write(&path, body).expect("writes");
            let mut file = std::fs::File::open(&path).expect("opens");
            let tail = read_tail(&mut file, body.len() as u64, window, max).expect("reads");
            tail.text().to_string()
        };
        let body = "aaaa\nbbbb\ncccc\n";
        assert_eq!(tail(body, 100, 100), body, "the whole file");
        assert_eq!(
            tail(body, 7, 7),
            "cccc\n",
            "the partial record before is dropped"
        );
        assert_eq!(
            tail(body, 5, 5),
            "cccc\n",
            "a record starting on the edge is kept"
        );
        let huge = format!("{}\n{}\n", "x".repeat(40), "y".repeat(40));
        assert_eq!(
            tail(&huge, 10, 10),
            "",
            "one record bigger than the limit is skipped"
        );
        assert_eq!(
            tail(&huge, 10, 60),
            format!("{}\n", "y".repeat(40)),
            "the window widens until it holds one"
        );
    }

    #[test]
    fn the_newest_transcript_with_the_subagent_wins() {
        let home = tempfile::tempdir().expect("a temp dir");
        let work = tempfile::tempdir().expect("a temp dir");
        let claude = Claude::with_program(home.path(), "/usr/bin/true".into());
        let work_path = work.path().canonicalize().expect("exists");
        let project = claude.project_dir(&work_path);
        let write = |session: &str, secs: u64| {
            let dir = project.join(session).join("subagents");
            std::fs::create_dir_all(&dir).expect("creates");
            let path = dir.join("agent-a1.jsonl");
            std::fs::write(&path, PROMPT_LINE).expect("writes");
            std::fs::File::open(&path)
                .expect("opens")
                .set_modified(SystemTime::now() + std::time::Duration::from_secs(secs))
                .expect("sets mtime");
        };
        write("zzz-old", 1);
        write("aaa-new", 5);
        write("mmm-older", 0);
        let found = claude
            .subagent_file(&work_path, &AgentId::new("a1"))
            .expect("reads")
            .expect("found");
        assert!(found.to_string_lossy().contains("aaa-new"));
    }

    #[test]
    fn the_transcript_path_of_the_agent_is_kept_and_a_subagents_is_not() {
        let payload = |extra: &str| {
            format!(
                r#"{{"hook_event_name":"UserPromptSubmit","prompt":"hi","transcript_path":"/p/s.jsonl"{extra}}}"#
            )
        };
        let main = claude_event(&payload("")).expect("an event");
        assert_eq!(main.transcript, Some(TranscriptRef::new("/p/s.jsonl")));
        assert_eq!(
            claude_event(r#"{"hook_event_name":"SessionEnd","transcript_path":""}"#)
                .expect("an event")
                .transcript,
            None,
            "an empty path names nothing"
        );
        let tool = |extra: &str| {
            format!(
                r#"{{"hook_event_name":"PreToolUse","tool_use_id":"t","tool_name":"Bash","transcript_path":"/p/s.jsonl"{extra}}}"#
            )
        };
        assert_eq!(
            claude_event(&tool(r#","agent_id":"a1""#))
                .expect("an event")
                .transcript,
            None,
            "whichever file a subagent's hooks name, it isn't the agent's activity"
        );
        assert_eq!(
            claude_event(&tool("")).expect("an event").transcript,
            Some(TranscriptRef::new("/p/s.jsonl"))
        );
    }

    #[test]
    fn the_activity_is_read_from_the_named_transcript_or_else_the_newest_conversation() {
        let home = tempfile::tempdir().expect("a temp dir");
        let work = tempfile::tempdir().expect("a temp dir");
        let claude = Claude::with_program(home.path(), "/usr/bin/true".into());
        let dir = claude.project_dir(&work.path().canonicalize().expect("exists"));
        std::fs::create_dir_all(&dir).expect("creates");
        let prompt = |text: &str| {
            format!(
                r#"{{"type":"user","promptId":"p","timestamp":"2026-11-08T10:00:00.000Z","message":{{"role":"user","content":"{text}"}}}}"#
            )
        };
        let other = claude.projects.join("other-project");
        std::fs::create_dir_all(&other).expect("creates");
        let named = other.join("named.jsonl");
        std::fs::write(&named, prompt("from the hooks")).expect("writes");
        let outside = home.path().join("elsewhere.jsonl");
        std::fs::write(&outside, prompt("from outside")).expect("writes");
        let sneaky = format!("{}/../../elsewhere.jsonl", claude.projects.display());
        std::fs::write(dir.join("found.jsonl"), prompt("from the newest")).expect("writes");

        let detail = |at: Option<&TranscriptRef>| {
            claude
                .activity_detail(work.path(), at)
                .expect("reads")
                .and_then(|t| t.log.prompt)
        };
        let at = TranscriptRef::new(named.to_string_lossy());
        assert_eq!(detail(Some(&at)).as_deref(), Some("from the hooks"));
        assert_eq!(detail(None).as_deref(), Some("from the newest"));
        for refused in [
            "relative.jsonl",
            "/etc/passwd",
            "/nowhere/else.jsonl",
            &outside.to_string_lossy(),
            sneaky.as_str(),
        ] {
            assert_eq!(
                detail(Some(&TranscriptRef::new(refused))).as_deref(),
                Some("from the newest"),
                "{refused} isn't a transcript to read"
            );
        }
    }
}
