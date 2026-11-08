use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agent_core::activity::{About, AgentId, Background, Event, Input, Origin, Reported, ToolId};
use agent_core::names::Prompt;
use agent_core::ports::{
    Harness, HarnessError, HarnessId, Harnesses, LaunchCtx, LogEntry, PendingPrompt, Seen,
    SubagentDetail,
};
use agent_core::process::LaunchSpec;

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
/// How much of what a subagent said a log entry keeps.
const SAID: usize = 600;
/// Log entries kept per subagent, the newest.
const LOG_ENTRIES: usize = 400;
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
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(["stop", id]).stdin(std::process::Stdio::null());
        for key in INHERITED_SESSION_ENV {
            cmd.env_remove(key);
        }
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

    /// The conversation to resume: the newest transcript written since the
    /// worktree was created that the user has spoken in. Older ones belong
    /// to a previous agent that used the same name. `/clear` starts a new
    /// transcript, so the newest is the one the user means, and a launch
    /// that died before any message leaves one with no user turn, which is
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
            if !has_user_turn(&path)? {
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
                // Reattaching keeps everything the session had, hooks
                // included, since this harness launched it.
                None => {
                    return Ok(LaunchSpec::new(&self.program)
                        .cwd(&workdir)
                        .env_remove(INHERITED_SESSION_ENV.iter().copied())
                        .args(["attach".to_string(), id.clone()])
                        .reattaching());
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
        claude_event(&serde_json::from_str(payload).ok()?)
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
        let mut cmd = std::process::Command::new(&self.program);
        cmd.env("LC_ALL", "C").stdin(std::process::Stdio::null());
        cmd.args(["-p", "--model", self.quick_model.as_str()])
            .arg(question.as_str());
        for key in INHERITED_SESSION_ENV {
            cmd.env_remove(key);
        }
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
    fn subagent(
        &self,
        workdir: &Path,
        id: &AgentId,
    ) -> Result<Option<SubagentDetail>, HarnessError> {
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

/// Whether the user has said anything in the transcript at `path`. The
/// first user turn comes early, so this reads little of a long file.
fn has_user_turn(path: &Path) -> io::Result<bool> {
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    for line in std::io::BufReader::new(file).lines() {
        if line?.contains(r#""type":"user""#) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A subagent's prompt, log and whether it finished, from its transcript
/// and the `meta.json` beside it. A transcript still being written is read
/// as far as it goes.
fn read_subagent(transcript: &Path) -> io::Result<SubagentDetail> {
    use std::io::{BufRead, Read};
    let mut file = std::fs::File::open(transcript)?;
    let len = file.metadata()?.len();
    let mut head = Vec::new();
    std::io::BufReader::new((&file).take(HEAD_BYTES)).read_until(b'\n', &mut head)?;
    let tail = read_tail(&mut file, len, TAIL_BYTES, TAIL_BYTES_MAX)?;
    let finished = std::fs::read_to_string(transcript.with_extension("meta.json"))
        .is_ok_and(|meta| parse_finished(&meta));
    Ok(SubagentDetail {
        prompt: parse_prompt(&String::from_utf8_lossy(&head)).unwrap_or_default(),
        finished,
        log: parse_log(&tail),
        transcript: transcript.to_path_buf(),
    })
}

/// The whole records in the last `window` bytes of a `len` byte file, widening
/// the window fourfold up to `max` while it holds none (one huge record).
/// The window reaches one byte back so a record starting exactly at its edge
/// is kept, and the partial one before it is dropped.
fn read_tail(file: &mut std::fs::File, len: u64, mut window: u64, max: u64) -> io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
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
                return Ok(whole.to_string());
            }
            None if window >= max => return Ok(String::new()),
            _ => window = (window * 4).min(max),
        }
    }
}

/// The prompt in a subagent transcript's first line.
fn parse_prompt(line: &str) -> Option<String> {
    let record: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
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

/// Whether a `meta.json` says the subagent is done.
fn parse_finished(meta: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(meta)
        .ok()
        .and_then(|meta| meta.get("finished").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

/// The steps in transcript lines, oldest first and at most
/// [`LOG_ENTRIES`] of them: what the subagent said and the tools it
/// called, each with the first line of its result once that's recorded.
/// Lines that aren't whole records are skipped.
fn parse_log(text: &str) -> Vec<LogEntry> {
    let mut log: Vec<LogEntry> = Vec::new();
    let mut calls: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for record in text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
    {
        let Some(parts) = record
            .pointer("/message/content")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for part in parts {
            let field = |key: &str| part.get(key).and_then(serde_json::Value::as_str);
            match (
                record.get("type").and_then(serde_json::Value::as_str),
                field("type"),
            ) {
                (Some("assistant"), Some("text")) => {
                    let said: String = field("text")
                        .unwrap_or_default()
                        .trim()
                        .chars()
                        .take(SAID)
                        .collect();
                    if !said.is_empty() {
                        log.push(LogEntry::Said(said));
                    }
                }
                (Some("assistant"), Some("tool_use")) => {
                    if let Some(id) = field("id") {
                        calls.insert(id.to_string(), log.len());
                    }
                    log.push(LogEntry::Called {
                        tool: field("name").unwrap_or("a tool").to_string(),
                        target: part.get("input").and_then(target),
                        result: None,
                    });
                }
                (Some("user"), Some("tool_result")) => {
                    let first = part.get("content").map(text_of).map(|t| snippet(&t));
                    let at = field("tool_use_id").and_then(|id| calls.get(id));
                    if let (Some(first), Some(&at)) = (first, at)
                        && let Some(LogEntry::Called { result, .. }) = log.get_mut(at)
                    {
                        *result = Some(first).filter(|r| !r.is_empty());
                    }
                }
                _ => {}
            }
        }
    }
    log.drain(..log.len().saturating_sub(LOG_ENTRIES));
    log
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

/// What a tool is acting on: a command, a file, a search or a URL.
fn target(input: &serde_json::Value) -> Option<String> {
    ["command", "file_path", "pattern", "url", "description"]
        .iter()
        .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
        .map(snippet)
}

/// Prompts Claude Code writes to tell the agent something, like a
/// background subagent finishing.
const NOTIFICATION_PREFIXES: &[&str] = &["<agent-message", "<task-notification"];

/// A hook payload as a [`Reported`] event.
fn claude_event(payload: &serde_json::Value) -> Option<Reported> {
    let text = |key: &str| payload.get(key).and_then(serde_json::Value::as_str);
    let at = |pointer: &str| payload.pointer(pointer).and_then(serde_json::Value::as_str);
    let tool = || text("tool_name").unwrap_or("a tool").to_string();
    let id = || ToolId::new(text("tool_use_id").unwrap_or_default());
    let input = payload.get("tool_input").cloned().unwrap_or_default();
    let kind = |value: Option<&str>| value.unwrap_or("subagent").to_string();
    let name = text("hook_event_name")?;
    let launched = at("/tool_response/agentId")
        .filter(|_| at("/tool_response/status") == Some("async_launched"));
    let origin = match launched.or_else(|| text("agent_id")) {
        Some(id) => Origin::Subagent(AgentId::new(id)),
        None => Origin::Main,
    };
    let event = match name {
        "SessionStart" if text("source") == Some("compact") => Event::Compacted,
        "SessionStart" => Event::Started,
        "UserPromptSubmit" => {
            let prompt = text("prompt").unwrap_or_default();
            Event::Prompted(
                if NOTIFICATION_PREFIXES
                    .iter()
                    .any(|p| prompt.trim_start().starts_with(p))
                {
                    Input::Notification
                } else {
                    Input::User(long(prompt))
                },
            )
        }
        "PreToolUse" if text("tool_name") == Some("AskUserQuestion") => {
            let question = input
                .pointer("/questions/0/question")
                .and_then(serde_json::Value::as_str);
            Event::Asked {
                question: snippet(question.unwrap_or("has a question")),
                about: About::Tool(id()),
            }
        }
        "PreToolUse" => Event::ToolStarted {
            id: id(),
            tool: tool(),
            target: target(&input),
        },
        "PostToolUse" if launched.is_some() => Event::SubagentLaunched {
            tool: id(),
            kind: kind(
                input
                    .get("subagent_type")
                    .and_then(serde_json::Value::as_str),
            ),
            description: snippet(
                at("/tool_response/description")
                    .or_else(|| input.get("description").and_then(serde_json::Value::as_str))
                    .unwrap_or_default(),
            ),
        },
        "PostToolUse" | "PostToolUseFailure" => Event::ToolFinished { id: id() },
        "PermissionDenied" if text("tool_use_id").is_some() => Event::ToolFinished { id: id() },
        "PermissionDenied" => Event::Declined,
        // Its question already arrived with its PreToolUse.
        "PermissionRequest" if text("tool_name") == Some("AskUserQuestion") => return None,
        "PermissionRequest" => {
            let target = target(&input);
            Event::Asked {
                question: match &target {
                    Some(target) => format!("{}: {target}", tool()),
                    None => tool(),
                },
                about: About::Call {
                    tool: tool(),
                    target,
                },
            }
        }
        "Notification" => Event::Asked {
            question: snippet(text("message").unwrap_or("has a question")),
            about: About::Unknown,
        },
        "SubagentStart" => Event::SubagentStarted {
            kind: kind(text("agent_type")),
        },
        "SubagentStop" => Event::SubagentStopped,
        "Stop" => Event::Finished {
            summary: text("last_assistant_message").map(long),
            running: background_subagents(payload),
        },
        "StopFailure" => Event::Failed(text("error").unwrap_or("error").replace('_', " ")),
        "SessionEnd" => Event::Ended,
        _ => return None,
    };
    Some(Reported {
        origin,
        event,
        lineage: None,
    })
}

/// The subagents a finished turn left running. Other background work
/// (shells, monitors) doesn't hand anything back to the agent.
fn background_subagents(payload: &serde_json::Value) -> Vec<Background> {
    let tasks = payload
        .get("background_tasks")
        .and_then(serde_json::Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    tasks
        .iter()
        .filter(|task| task.get("type").and_then(serde_json::Value::as_str) == Some("subagent"))
        .filter(|task| {
            matches!(
                task.get("status").and_then(serde_json::Value::as_str),
                None | Some("running" | "pending")
            )
        })
        .filter_map(|task| {
            let field = |key: &str| task.get(key).and_then(serde_json::Value::as_str);
            Some(Background {
                id: AgentId::new(field("id")?),
                kind: field("agent_type").unwrap_or("subagent").to_string(),
                description: field("description").map(snippet),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let event = |json: &str| claude_event(&serde_json::from_str(json).expect("json"));
        let main = |e: Event| Some(Reported::main(e));
        assert_eq!(
            event(
                r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"cargo test\n--all"}}"#
            ),
            main(Event::ToolStarted {
                id: ToolId::new("t1"),
                tool: "Bash".into(),
                target: Some("cargo test".into())
            })
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf tmp"}}"#
            ),
            main(Event::Asked {
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
            main(Event::Asked {
                question: "Which DB?".into(),
                about: About::Tool(ToolId::new("q1"))
            })
        );
        assert_eq!(
            event(r#"{"hook_event_name":"SessionStart","source":"compact"}"#),
            main(Event::Compacted)
        );
        assert_eq!(
            event(r#"{"hook_event_name":"SessionStart","source":"resume"}"#),
            main(Event::Started)
        );
        assert_eq!(
            event(r#"{"hook_event_name":"StopFailure","error":"rate_limit"}"#),
            main(Event::Failed("rate limit".into()))
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"UserPromptSubmit","prompt":"<agent-message from=\"a1\">done</agent-message>"}"#
            ),
            main(Event::Prompted(Input::Notification))
        );
    }

    #[test]
    fn subagent_payloads_say_which_subagent_they_came_from() {
        let event = |json: &str| claude_event(&serde_json::from_str(json).expect("json"));
        let a1 = |e: Event| Some(Reported::subagent(AgentId::new("a1"), e));
        assert_eq!(
            event(
                r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","agent_id":"a1","tool_input":{"command":"ls"}}"#
            ),
            a1(Event::Asked {
                question: "Bash: ls".into(),
                about: About::Call {
                    tool: "Bash".into(),
                    target: Some("ls".into())
                }
            })
        );
        assert_eq!(
            event(r#"{"hook_event_name":"SubagentStart","agent_id":"a1","agent_type":"Explore"}"#),
            a1(Event::SubagentStarted {
                kind: "Explore".into()
            })
        );
        assert_eq!(
            event(r#"{"hook_event_name":"SubagentStop","agent_id":"a1"}"#),
            a1(Event::SubagentStopped)
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"PostToolUse","tool_name":"Agent","agent_id":null,"tool_use_id":"t9","tool_input":{"subagent_type":"general-purpose","description":"review"},"tool_response":{"status":"async_launched","agentId":"a1","description":"Adversarial review"}}"#
            ),
            a1(Event::SubagentLaunched {
                tool: ToolId::new("t9"),
                kind: "general-purpose".into(),
                description: "Adversarial review".into()
            })
        );
        assert_eq!(
            event(
                r#"{"hook_event_name":"Stop","agent_id":null,"last_assistant_message":"Waiting.","background_tasks":[{"id":"a1","type":"subagent","status":"running","agent_type":"Explore","description":"look"},{"id":"s1","type":"shell","status":"running"},{"id":"a2","type":"subagent","status":"completed"}]}"#
            ),
            Some(Reported::main(Event::Finished {
                summary: Some("Waiting.".into()),
                running: vec![Background {
                    id: AgentId::new("a1"),
                    kind: "Explore".into(),
                    description: Some("look".into())
                }]
            }))
        );
    }

    const PROMPT_LINE: &str = r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"Review the diff.\nBe harsh."}}"#;
    const CALL_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking."},{"type":"tool_use","id":"tu1","name":"Read","input":{"file_path":"/a/b.rs"}}]}}"#;
    const RESULT_LINE: &str = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"fn main() {}\nmore"}]}}"#;
    const LATER_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu2","name":"Bash","input":{"command":"cargo test"}}]}}"#;

    #[test]
    fn a_subagent_transcript_reads_as_a_log_with_results_paired_to_calls() {
        assert_eq!(
            parse_prompt(PROMPT_LINE).as_deref(),
            Some("Review the diff.\nBe harsh.")
        );
        let text =
            format!("{PROMPT_LINE}\n{CALL_LINE}\n{RESULT_LINE}\n{LATER_LINE}\n{{\"type\":\"assi");
        assert_eq!(
            parse_log(&text),
            [
                LogEntry::Said("Looking.".into()),
                LogEntry::Called {
                    tool: "Read".into(),
                    target: Some("/a/b.rs".into()),
                    result: Some("fn main() {}".into()),
                },
                LogEntry::Called {
                    tool: "Bash".into(),
                    target: Some("cargo test".into()),
                    result: None,
                },
            ],
            "a record cut off mid-write is skipped"
        );
        assert!(parse_finished(r#"{"agentType":"Explore","finished":true}"#));
        assert!(!parse_finished(r#"{"agentType":"Explore"}"#));
        assert!(!parse_finished("not json"));
        assert_eq!(parse_prompt("{\"type\":"), None);
    }

    #[test]
    fn the_log_keeps_only_the_newest_entries() {
        let text: String = (0..LOG_ENTRIES + 5)
            .map(|i| {
                format!(
                    r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"t{i}"}}]}}}}"#
                ) + "\n"
            })
            .collect();
        let log = parse_log(&text);
        assert_eq!(log.len(), LOG_ENTRIES);
        assert_eq!(log[0], LogEntry::Said("t5".into()));
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
        assert_eq!(detail.prompt, "Review the diff.\nBe harsh.");
        assert!(detail.finished);
        assert_eq!(detail.log.len(), 2);

        std::fs::write(&transcript, "").expect("writes");
        let empty = claude
            .subagent(&work_path, &id)
            .expect("reads")
            .expect("found");
        assert_eq!(empty.prompt, "");
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
            read_tail(&mut file, body.len() as u64, window, max).expect("reads")
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
}
