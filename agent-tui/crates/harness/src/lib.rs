use std::ffi::OsString;
use std::io::{self, BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use agent_core::activity::{
    About, AgentId, Background, CommandLine, CommandListing, Common, Input, ListedCommand,
    MainEvent, Reported, SubagentEvent, TaskId, ToolId, TranscriptRef,
};
use agent_core::names::Prompt;
use agent_core::ports::{
    ActivityDetail, Harness, HarnessError, HarnessId, Harnesses, LaunchCtx, Log, Lookup, Outcome,
    PendingPrompt, Probe, Seen, TranscriptStamp, ends_with_report,
};
use agent_core::process::{LaunchSpec, quiet};
use serde::Deserialize;

use crate::background::{Found, JobId, ProcStart, Survivor};
use crate::conversation::{Chains, Conversation, SessionId};
use crate::stopping::{Patience, Processes};

mod activity;
mod background;
mod conversation;
#[cfg(test)]
mod fixtures;
mod stopping;
mod transcript;

const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const PS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
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
/// session. They make Claude treat it as a child and stop writing its
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
    program: Arc<OsString>,
    /// The model for one-off questions.
    quick_model: String,
    projects: PathBuf,
    /// Where Claude Code registers its live sessions, one JSON per process.
    sessions: PathBuf,
    permission_mode: String,
    add_dirs: Vec<PathBuf>,
    chains: Chains,
    processes: Arc<dyn Processes>,
}

impl Claude {
    pub fn new(home: &Path) -> Self {
        Self::with_program(
            home,
            std::env::var_os("AGENT_CLAUDE").unwrap_or_else(|| "claude".into()),
        )
    }

    fn with_program(home: &Path, program: OsString) -> Self {
        let program = Arc::new(program);
        Self {
            id: "claude".parse().expect("valid id"),
            program: Arc::clone(&program),
            quick_model: "haiku".into(),
            projects: home.join(".claude/projects"),
            sessions: home.join(".claude/sessions"),
            permission_mode: "auto".into(),
            add_dirs: vec![PathBuf::from("/tmp")],
            chains: Chains::default(),
            processes: Arc::new(Machine { program }),
        }
    }

    /// A one-off `claude` invocation, outside any session this process is
    /// itself running in.
    fn command(&self) -> std::process::Command {
        command(&self.program)
    }

    /// The background session running `conversation`, if any. Registry
    /// entries of its sessions that can't be read are added to `notes`,
    /// since the worker may be among them. A process still in its terminal
    /// isn't one to attach to or stop.
    fn in_background(
        &self,
        conversation: &Conversation,
        workdir: &Path,
        notes: &mut Vec<String>,
    ) -> Result<Option<Joinable>, HarnessError> {
        let search = background::find(&self.sessions, std::slice::from_ref(conversation), workdir)?;
        if !search.unreadable.is_empty() {
            notes.push(unreadable_registry(&search.unreadable));
        }
        Ok(match search.found {
            Some(Found::Attachable(survivor)) => Some(Joinable::Attach(survivor)),
            Some(Found::Unaddressable { pid, why }) => Some(Joinable::Unaddressable { pid, why }),
            Some(Found::Pending(_) | Found::Elsewhere) | None => None,
        })
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

    /// The transcript of the conversation the transcript file `at` names
    /// belongs to, if that is under the projects directory: the one holding
    /// its messages, which is not `at`'s own when the conversation has moved
    /// on from it.
    fn named_transcript(&self, at: &TranscriptRef) -> io::Result<Option<PathBuf>> {
        Ok(match self.named(at)? {
            None => None,
            Some(Named::File(path)) => Some(path),
            Some(Named::Chain { dir, conversation }) => {
                Some(conversation.transcript_in(&dir)).filter(|live| live.is_file())
            }
        })
    }

    /// The conversation the transcript file `at` names belongs to, followed
    /// to where it has moved on to. Its session ids include the one a
    /// running process is registered under, which a `/clear` not yet
    /// followed by a message leaves out of what `launch` would resume.
    fn conversation_at(&self, at: &TranscriptRef) -> io::Result<Option<Conversation>> {
        Ok(match self.named(at)? {
            Some(Named::Chain { conversation, .. }) => Some(conversation),
            Some(Named::File(_)) | None => None,
        })
    }

    fn named(&self, at: &TranscriptRef) -> io::Result<Option<Named>> {
        let Some(path) = self.named_file(at) else {
            return Ok(None);
        };
        let Some(start) = session_of(&path) else {
            return Ok(Some(Named::File(path)));
        };
        let dir = path.parent().unwrap_or(&self.projects).to_path_buf();
        let conversation = self.chains.of(&dir, start)?;
        Ok(Some(Named::Chain { dir, conversation }))
    }

    /// The transcript file `at` names, if it is one under the projects
    /// directory.
    fn named_file(&self, at: &TranscriptRef) -> Option<PathBuf> {
        let projects = self.projects.canonicalize().ok()?;
        at.path()
            .canonicalize()
            .ok()
            .filter(|path| path.extension().is_some_and(|e| e == "jsonl") && path.is_file())
            .filter(|path| path.starts_with(&projects))
    }

    /// The transcript of the agent's current conversation: the one the hooks
    /// named, if it is a transcript file under the projects directory, and
    /// otherwise the one `launch` would resume.
    fn activity_file(
        &self,
        workdir: &Path,
        at: Option<&TranscriptRef>,
    ) -> io::Result<Option<PathBuf>> {
        if let Some(named) = at
            .map(|at| self.named_transcript(at))
            .transpose()?
            .flatten()
        {
            return Ok(Some(named));
        }
        let workdir = workdir.canonicalize()?;
        let dir = self.project_dir(&workdir);
        Ok(self
            .resumable(&workdir)?
            .map(|conversation| conversation.transcript_in(&dir)))
    }

    /// The conversation to resume: the newest transcript written since the
    /// worktree was created that the user has spoken in, followed to where
    /// the conversation has moved on to. Older ones belong to a previous
    /// agent that used the same name. `/clear` starts a new transcript, so
    /// the newest is the one the user means, and a launch that died before
    /// any message leaves one with no user prompt, which is skipped.
    fn resumable(&self, workdir: &Path) -> io::Result<Option<Conversation>> {
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
            if let Some(id) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(SessionId::parse)
            {
                return self.chains.of(&self.project_dir(workdir), id).map(Some);
            }
        }
        Ok(None)
    }
}

fn command(program: &std::ffi::OsStr) -> std::process::Command {
    let mut cmd = quiet(program);
    for key in INHERITED_SESSION_ENV {
        cmd.env_remove(key);
    }
    cmd
}

fn stop_command(program: &std::ffi::OsStr, job: &JobId) -> std::process::Command {
    let mut cmd = command(program);
    cmd.args(["stop", job.as_str()]);
    cmd
}

fn term_command(pid: agent_core::activity::Pid) -> std::process::Command {
    let mut cmd = quiet("kill");
    cmd.args(["-TERM", &pid.get().to_string()]);
    cmd
}

/// A background session a launch can do something about.
enum Joinable {
    Attach(Survivor),
    Unaddressable {
        pid: agent_core::activity::Pid,
        why: background::Unaddressed,
    },
}

/// What a transcript named by the hooks is.
enum Named {
    /// A transcript file whose name isn't a session id.
    File(PathBuf),
    Chain {
        dir: PathBuf,
        conversation: Conversation,
    },
}

fn unreadable_registry(files: &[PathBuf]) -> String {
    let files: Vec<_> = files.iter().map(|p| p.display().to_string()).collect();
    format!(
        "can't read Claude's registry entries {}, so a running background session may be missed",
        files.join(", ")
    )
}

fn session_of(transcript: &Path) -> Option<SessionId> {
    transcript
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(SessionId::parse)
}

/// The real processes: `claude stop`, `ps` and `kill`.
struct Machine {
    program: Arc<OsString>,
}

impl Processes for Machine {
    fn started(&self, pid: agent_core::activity::Pid) -> Option<ProcStart> {
        ProcStart::of(pid)
    }

    fn stop_job(&self, job: &JobId) -> io::Result<()> {
        let mut cmd = stop_command(&self.program, job);
        let out =
            agent_core::process::output_within(&mut cmd, STOP_TIMEOUT).map_err(io::Error::other)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    fn terminate(&self, pid: agent_core::activity::Pid) -> io::Result<()> {
        let mut cmd = term_command(pid);
        let out =
            agent_core::process::output_within(&mut cmd, PS_TIMEOUT).map_err(io::Error::other)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "kill: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }
}

/// Stops one background session through `claude stop`, then SIGTERM.
struct ClaudeStop {
    processes: Arc<dyn Processes>,
    survivor: Survivor,
}

impl agent_core::ports::Stop for ClaudeStop {
    fn stop(self: Box<Self>) -> Result<(), HarnessError> {
        stopping::stop(
            self.processes.as_ref(),
            &self.survivor,
            Patience::STOP,
            Patience::TERM,
        )
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

        let mut notes = Vec::new();
        if let Some(conversation) = &resume
            && let Some(found) = self.in_background(conversation, &workdir, &mut notes)?
        {
            let survivor = match found {
                Joinable::Attach(survivor) => survivor,
                Joinable::Unaddressable { pid, why } => {
                    return Err(HarnessError {
                        context: format!(
                            "the background session of this conversation (pid {}) has {why} in Claude's registry, so it can't be attached or stopped",
                            pid.get()
                        ),
                        source: io::Error::other("unaddressable background session"),
                    });
                }
            };
            match ctx.bootstrap {
                // Joining keeps everything the session had, hooks
                // included, since this harness launched it.
                None => {
                    let spec = LaunchSpec::new(self.program.as_os_str())
                        .cwd(&workdir)
                        .env_remove(INHERITED_SESSION_ENV.iter().copied())
                        .args(["attach", survivor.job.as_str()])
                        .untraced();
                    return Ok(notes.into_iter().fold(spec, LaunchSpec::noting));
                }
                Some(_) => stopping::stop(
                    self.processes.as_ref(),
                    &survivor,
                    Patience::STOP,
                    Patience::TERM,
                )?,
            }
        }

        let mut spec = LaunchSpec::new(self.program.as_os_str())
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
        if let Some(conversation) = resume {
            spec = spec.args(["--resume", conversation.resumable().as_str()]);
        }
        let spec = notes.into_iter().fold(spec, LaunchSpec::noting);
        Ok(match ctx.bootstrap {
            Some(prompt) => spec.arg("--").arg(prompt.as_str()),
            None => spec,
        })
    }

    fn survivor(&self, workdir: &Path, at: Option<&TranscriptRef>) -> Result<Lookup, HarnessError> {
        let read = |source| HarnessError {
            context: "looking for the conversation's background session".into(),
            source,
        };
        let workdir = match workdir.canonicalize() {
            Ok(workdir) => workdir,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Lookup::Gone),
            Err(e) => return Err(read(e)),
        };
        let named = at
            .map(|at| self.conversation_at(at))
            .transpose()
            .map_err(read)?
            .flatten();
        let conversations: Vec<Conversation> = named
            .into_iter()
            .chain(self.resumable(&workdir).map_err(read)?)
            .collect();
        let search = background::find(&self.sessions, &conversations, &workdir)?;
        match search.found {
            Some(Found::Attachable(survivor)) => {
                Ok(Lookup::Survivor(agent_core::ports::Survivor::new(
                    survivor.state,
                    Box::new(ClaudeStop {
                        processes: Arc::clone(&self.processes),
                        survivor,
                    }),
                )))
            }
            Some(Found::Pending(state)) => Ok(Lookup::NotYet(state)),
            Some(Found::Elsewhere) => Ok(Lookup::Elsewhere),
            Some(Found::Unaddressable { pid, why }) => Err(HarnessError {
                context: format!(
                    "the background session of this conversation (pid {}) has {why} in Claude's registry, so it can't be stopped",
                    pid.get()
                ),
                source: io::Error::other("unaddressable background session"),
            }),
            None if search.unreadable.is_empty() => Ok(Lookup::Gone),
            None => Err(HarnessError {
                context: "reading Claude's session registry".into(),
                source: io::Error::other(unreadable_registry(&search.unreadable)),
            }),
        }
    }

    fn event(&self, payload: &str) -> Option<Reported> {
        claude_event(payload).map(Reported::interrupted_in_transcript)
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
        Ok(latest.filter(|m| *m >= pending.since).map(Seen::at))
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

    fn interruption(
        &self,
        at: &TranscriptRef,
        after: SystemTime,
        seen: Option<&TranscriptStamp>,
    ) -> Result<Probe, HarnessError> {
        let context = |source| HarnessError {
            context: "checking the transcript for an interrupt".into(),
            source,
        };
        let Some(path) = self.named_transcript(at).map_err(context)? else {
            return Ok(Probe::NoTranscript);
        };
        let meta = std::fs::metadata(&path).map_err(context)?;
        let stamp = TranscriptStamp::new(meta.len(), meta.modified().ok());
        if seen == Some(&stamp) {
            return Ok(Probe::Unchanged);
        }
        let mut file = std::fs::File::open(&path).map_err(context)?;
        let mut window = PROBE_BYTES;
        loop {
            let tail = read_tail(&mut file, meta.len(), window, TAIL_BYTES).map_err(context)?;
            if transcript::unfinished(tail.text()) {
                return Ok(Probe::Unchanged);
            }
            match transcript::last_conversational(tail.text()) {
                Some(last) => return Ok(verdict(last, after, stamp)),
                None if tail.reaches_start() => return Ok(Probe::Older(stamp)),
                None if window >= TAIL_BYTES => return Ok(Probe::Unchanged),
                None => window = (window * 4).min(TAIL_BYTES),
            }
        }
    }

    /// Bracketed paste keeps a multi-line prompt as one message. Enter submits it.
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

/// What the newest conversational record says, if it is later than `after`.
fn verdict(last: transcript::Last, after: SystemTime, stamp: TranscriptStamp) -> Probe {
    match (last.beat, last.at) {
        (transcript::Beat::Interrupt, Some(at)) if at > after => Probe::Interrupted(stamp, at),
        (transcript::Beat::Work, Some(at)) if at > after => Probe::Going(stamp),
        _ => Probe::Older(stamp),
    }
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

fn running_tasks(
    tasks: &[Lenient<BackgroundTask>],
    kind: TaskKind,
) -> impl Iterator<Item = &BackgroundTask> {
    tasks
        .iter()
        .filter_map(Lenient::get)
        .filter(move |task| task.kind == Some(kind))
        .filter(|task| {
            matches!(
                task.status,
                None | Some(TaskStatus::Running | TaskStatus::Pending)
            )
        })
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

/// What the text of the record Claude Code writes for the user stopping a
/// turn starts with (`[Request interrupted by user]`, or `… for tool use]`).
pub(crate) const INTERRUPTED: &str = "[Request interrupted by user";

/// How much of the end of a transcript is read to see whether it ends in an
/// interrupt, widened while it holds no conversational record.
const PROBE_BYTES: u64 = 64 * 1024;

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

/// A background task's id. One with nothing visible in it can't be told
/// from another or shown, so a task carrying one is skipped.
struct VisibleId(String);

impl<'de> Deserialize<'de> for VisibleId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if !agent_core::visible::has_visible(&text) {
            return Err(serde::de::Error::custom("id with nothing visible"));
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

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TaskKind {
    Subagent,
    Shell,
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    Running,
    Pending,
    #[serde(other)]
    Settled,
}

#[derive(Deserialize)]
struct BackgroundTask {
    #[serde(default)]
    id: Option<VisibleId>,
    #[serde(default, rename = "type")]
    kind: Option<TaskKind>,
    #[serde(default)]
    status: Option<TaskStatus>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    command: Lenient<String>,
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
        } => {
            let commands = match background_tasks.get() {
                Some(tasks) => CommandListing::Listed(background_commands(tasks)),
                None => CommandListing::Unknown,
            };
            Routed::Main(MainEvent::Finished {
                summary: last_assistant_message.as_deref().map(long),
                running: background_subagents(
                    background_tasks.get().map_or(&[][..], Vec::as_slice),
                ),
                commands,
            })
        }
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
/// (monitors, workflows) isn't tracked. A task that can't be read is skipped
/// without hiding the others.
fn background_subagents(tasks: &[Lenient<BackgroundTask>]) -> Vec<Background> {
    running_tasks(tasks, TaskKind::Subagent)
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

/// The commands a finished activity left running. A task without a command
/// line is skipped like an unreadable one.
fn background_commands(tasks: &[Lenient<BackgroundTask>]) -> Vec<ListedCommand> {
    running_tasks(tasks, TaskKind::Shell)
        .filter_map(|task| {
            Some(ListedCommand {
                id: TaskId::try_from(task.id.as_ref()?.0.clone()).ok()?,
                command: task.command.text()?.parse::<CommandLine>().ok()?,
                description: task.description.clone(),
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
    }

    mod background_sessions {
        use super::*;
        use crate::background::{Found, JobField, ProcStart, Unaddressed};
        use crate::conversation::SessionId;
        use crate::fixtures::{continued, user_line, write_at};
        use agent_core::ports::SurvivorState;
        use std::sync::{Arc, Mutex};

        /// Processes that exit when told to stop and are never signalled,
        /// since a registered pid in these tests is the test's own.
        #[derive(Default)]
        struct Cooperative {
            stopped: Mutex<Vec<String>>,
        }

        impl Processes for Cooperative {
            fn started(&self, pid: agent_core::activity::Pid) -> Option<ProcStart> {
                let stopped = self.stopped.lock().expect("lock").is_empty();
                stopped.then(|| ProcStart::of(pid)).flatten()
            }

            fn stop_job(&self, job: &JobId) -> io::Result<()> {
                self.stopped
                    .lock()
                    .expect("lock")
                    .push(job.as_str().to_string());
                Ok(())
            }

            fn terminate(&self, _: agent_core::activity::Pid) -> io::Result<()> {
                panic!("a test never signals a process")
            }
        }

        struct Project {
            _home: tempfile::TempDir,
            work: tempfile::TempDir,
            claude: Claude,
            dir: PathBuf,
            processes: Arc<Cooperative>,
        }

        impl Project {
            fn new() -> Self {
                let home = tempfile::tempdir().expect("a temp dir");
                let work = tempfile::tempdir().expect("a temp dir");
                let mut claude = Claude::with_program(home.path(), "/usr/bin/true".into());
                let processes = Arc::new(Cooperative::default());
                claude.processes = processes.clone();
                let dir = claude.project_dir(&work.path().canonicalize().expect("exists"));
                std::fs::create_dir_all(&dir).expect("creates");
                std::fs::create_dir_all(&claude.sessions).expect("creates");
                Self {
                    _home: home,
                    work,
                    claude,
                    dir,
                    processes,
                }
            }

            fn transcript(&self, id: &str, lines: &[String], secs: u64) {
                write_at(&self.dir, id, lines, secs);
            }

            fn register(&self, file: &str, entry: Entry) {
                let pid = agent_core::activity::Pid::new(entry.pid).expect("a pid");
                let started = entry.started.unwrap_or_else(|| {
                    ProcStart::of(pid)
                        .map(|s| s.to_string())
                        .unwrap_or_default()
                });
                let mut json = serde_json::json!({
                    "pid": entry.pid,
                    "sessionId": entry.session,
                    "kind": entry.kind,
                    "cwd": entry.cwd.unwrap_or_else(|| self.work.path().to_path_buf()),
                    "procStart": started,
                });
                if let Some(job) = entry.job {
                    json["jobId"] = job.into();
                }
                if entry.spare {
                    json["spare"] = true.into();
                }
                if let Some(status) = entry.status {
                    json["status"] = status.into();
                }
                std::fs::write(self.claude.sessions.join(file), json.to_string()).expect("writes");
            }

            fn launched(&self, bootstrap: Option<&Prompt>) -> Result<LaunchSpec, HarnessError> {
                let session = "fix".parse().expect("parses");
                self.claude.launch(&LaunchCtx {
                    session: &session,
                    workdir: self.work.path(),
                    bootstrap,
                    report: Path::new("/bin/agent"),
                })
            }

            fn tail(&self, bootstrap: Option<&Prompt>, n: usize) -> Vec<String> {
                let spec = self.launched(bootstrap).expect("launches");
                let args: Vec<String> = spec
                    .arguments()
                    .iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                args[args.len().saturating_sub(n)..].to_vec()
            }

            fn search(&self, ids: &[&str]) -> crate::background::Search {
                let mut ids = ids.iter().map(|id| SessionId::parse(id).expect("an id"));
                let first = ids.next().expect("an id");
                let conversation =
                    crate::conversation::Conversation::of(&self.dir, first).expect("reads");
                crate::background::find(
                    &self.claude.sessions,
                    std::slice::from_ref(&conversation),
                    &self.work.path().canonicalize().expect("exists"),
                )
                .expect("reads")
            }
        }

        struct Entry<'a> {
            pid: u32,
            session: &'a str,
            kind: &'a str,
            job: Option<&'a str>,
            spare: bool,
            cwd: Option<PathBuf>,
            started: Option<String>,
            status: Option<&'a str>,
        }

        impl<'a> Entry<'a> {
            fn bg(session: &'a str, job: &'a str) -> Self {
                Self {
                    pid: std::process::id(),
                    session,
                    kind: "bg",
                    job: Some(job),
                    spare: false,
                    cwd: None,
                    started: None,
                    status: None,
                }
            }
        }

        fn jobs_of(found: Option<Found>) -> Option<String> {
            match found {
                Some(Found::Attachable(survivor)) => Some(survivor.job.as_str().to_string()),
                Some(Found::Unaddressable { .. } | Found::Pending(_) | Found::Elsewhere) | None => {
                    None
                }
            }
        }

        #[test]
        fn a_running_background_session_is_attached_to_by_its_job_id() {
            let project = Project::new();
            project.transcript("after-clear", &[user_line()], 1);
            assert_eq!(project.tail(None, 2), ["--resume", "after-clear"]);
            project.register("alive.json", Entry::bg("after-clear", "470bb73a"));
            let spec = project.launched(None).expect("launches");
            assert_eq!(spec.arguments(), ["attach", "470bb73a"]);
        }

        #[test]
        fn a_prompt_stops_the_background_session_and_resumes_the_conversation() {
            let project = Project::new();
            project.transcript("after-clear", &[user_line()], 1);
            project.register("alive.json", Entry::bg("after-clear", "470bb73a"));
            let go: Prompt = "--go".parse().expect("parses");
            assert_eq!(
                project.tail(Some(&go), 4),
                ["--resume", "after-clear", "--", "--go"]
            );
            assert_eq!(
                *project.processes.stopped.lock().expect("lock"),
                ["470bb73a"]
            );
        }

        #[test]
        fn a_transcript_the_conversation_left_with_the_newer_mtime_still_finds_its_fork() {
            let project = Project::new();
            project.transcript("old", &[user_line(), continued("old", "new")], 5);
            project.transcript("new", &[user_line(), user_line()], 1);
            assert_eq!(
                project.tail(None, 2),
                ["--resume", "new"],
                "the id that holds the messages, not the one named most recently"
            );
            project.register("alive.json", Entry::bg("new", "470bb73a"));
            let spec = project.launched(None).expect("launches");
            assert_eq!(spec.arguments(), ["attach", "470bb73a"]);
        }

        #[test]
        fn a_session_registered_under_any_id_of_the_chain_is_the_conversations() {
            let project = Project::new();
            project.transcript("a", &[user_line(), continued("a", "b")], 3);
            project.transcript("b", &[user_line(), continued("b", "c")], 2);
            project.transcript("c", &[user_line()], 1);
            for (i, id) in ["a", "b", "c"].into_iter().enumerate() {
                project.register("alive.json", Entry::bg(id, &format!("job{i}")));
                let spec = project.launched(None).expect("launches");
                assert_eq!(spec.arguments(), ["attach", format!("job{i}").as_str()]);
            }
        }

        #[test]
        fn a_fork_with_no_messages_is_attached_to_but_the_conversation_resumes_from_its_origin() {
            let project = Project::new();
            project.transcript("old", &[user_line(), continued("old", "stub")], 2);
            project.transcript("stub", &[r#"{"type":"summary"}"#.to_string()], 1);
            assert_eq!(project.tail(None, 2), ["--resume", "old"]);
            project.register("alive.json", Entry::bg("stub", "470bb73a"));
            let spec = project.launched(None).expect("launches");
            assert_eq!(spec.arguments(), ["attach", "470bb73a"]);
        }

        fn survivor_state(project: &Project, at: Option<&TranscriptRef>) -> Option<SurvivorState> {
            match project
                .claude
                .survivor(project.work.path(), at)
                .expect("looks")
            {
                Lookup::Survivor(survivor) => Some(survivor.state()),
                Lookup::NotYet(_) | Lookup::Elsewhere | Lookup::Gone => None,
            }
        }

        fn lookup(project: &Project) -> Lookup {
            project
                .claude
                .survivor(project.work.path(), None)
                .expect("looks")
        }

        #[test]
        fn stopping_runs_claude_stop_with_the_job_id_and_signalling_is_a_plain_sigterm() {
            let JobField::Valid(job) =
                serde_json::from_str::<JobField>(r#""470bb73a""#).expect("parses")
            else {
                panic!("a valid job id")
            };
            let stop = stop_command(std::ffi::OsStr::new("claude"), &job);
            assert_eq!(stop.get_program(), "claude");
            assert_eq!(stop.get_args().collect::<Vec<_>>(), ["stop", "470bb73a"]);
            let pid = agent_core::activity::Pid::new(4242).expect("a pid");
            let term = term_command(pid);
            assert_eq!(term.get_program(), "kill");
            assert_eq!(term.get_args().collect::<Vec<_>>(), ["-TERM", "4242"]);
        }

        #[test]
        fn a_process_still_registered_as_interactive_is_not_gone_but_not_stoppable_yet() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            project.register(
                "terminal.json",
                Entry {
                    kind: "interactive",
                    status: Some("busy"),
                    ..Entry::bg("mine", "470bb73a")
                },
            );
            assert!(matches!(
                lookup(&project),
                Lookup::NotYet(SurvivorState::Busy)
            ));
            project.register("terminal.json", Entry::bg("mine", "470bb73a"));
            assert!(matches!(lookup(&project), Lookup::Survivor(_)));
        }

        #[test]
        fn an_interactive_process_in_another_directory_is_open_elsewhere_not_pending() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            let elsewhere = tempfile::tempdir().expect("a temp dir");
            project.register(
                "terminal.json",
                Entry {
                    kind: "interactive",
                    cwd: Some(elsewhere.path().to_path_buf()),
                    ..Entry::bg("mine", "470bb73a")
                },
            );
            assert!(matches!(lookup(&project), Lookup::Elsewhere));
        }

        #[test]
        fn an_interactive_process_that_is_gone_a_spare_or_another_conversations_is_not_there() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            for (file, entry) in [
                (
                    "gone",
                    Entry {
                        kind: "interactive",
                        pid: 999_999_999,
                        ..Entry::bg("mine", "j1")
                    },
                ),
                (
                    "spare",
                    Entry {
                        kind: "interactive",
                        spare: true,
                        ..Entry::bg("mine", "j2")
                    },
                ),
                (
                    "other",
                    Entry {
                        kind: "interactive",
                        ..Entry::bg("someone-elses", "j3")
                    },
                ),
            ] {
                project.register(&format!("{file}.json"), entry);
            }
            assert!(matches!(lookup(&project), Lookup::Gone));
        }

        #[test]
        fn a_survivor_is_the_conversations_process_and_says_whether_it_is_mid_task() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            assert_eq!(survivor_state(&project, None), None);
            project.register("alive.json", Entry::bg("mine", "470bb73a"));
            assert_eq!(survivor_state(&project, None), Some(SurvivorState::Idle));
            project.register(
                "alive.json",
                Entry {
                    status: Some("busy"),
                    ..Entry::bg("mine", "470bb73a")
                },
            );
            assert_eq!(survivor_state(&project, None), Some(SurvivorState::Busy));
        }

        #[test]
        fn a_process_running_a_session_cleared_without_a_message_is_found_through_the_hooks_transcript()
         {
            let project = Project::new();
            project.transcript("before", &[user_line()], 1);
            project.transcript("cleared", &[r#"{"type":"summary"}"#.to_string()], 2);
            project.register("alive.json", Entry::bg("cleared", "470bb73a"));
            assert_eq!(
                survivor_state(&project, None),
                None,
                "nothing in the resumable chain names it"
            );
            let hooked = TranscriptRef::new(project.dir.join("cleared.jsonl").to_string_lossy());
            assert_eq!(
                survivor_state(&project, Some(&hooked)),
                Some(SurvivorState::Idle)
            );
        }

        #[test]
        fn a_clear_followed_by_a_message_resumes_the_new_session_and_one_without_resumes_the_old() {
            let project = Project::new();
            project.transcript("before", &[user_line()], 1);
            project.transcript("cleared", &[r#"{"type":"summary"}"#.to_string()], 2);
            assert_eq!(project.tail(None, 2), ["--resume", "before"]);
            project.transcript("cleared", &[user_line()], 2);
            assert_eq!(project.tail(None, 2), ["--resume", "cleared"]);
        }

        #[test]
        fn an_unreadable_registry_entry_of_the_conversation_is_an_error_for_a_stop() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            std::fs::write(
                project.claude.sessions.join("odd.json"),
                r#"{"pid":1,"sessionId":"mine","kind":"bg"}"#,
            )
            .expect("writes");
            assert!(project.claude.survivor(project.work.path(), None).is_err());
        }

        #[test]
        fn entries_that_are_not_this_conversations_running_worker_are_ignored() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            let ignored = [
                (
                    "spare",
                    Entry {
                        spare: true,
                        ..Entry::bg("mine", "j1")
                    },
                ),
                (
                    "spare-kind",
                    Entry {
                        kind: "bg-spare",
                        ..Entry::bg("mine", "j2")
                    },
                ),
                (
                    "interactive",
                    Entry {
                        kind: "interactive",
                        ..Entry::bg("mine", "j3")
                    },
                ),
                ("other-conversation", Entry::bg("someone-elses", "j4")),
                (
                    "gone",
                    Entry {
                        pid: 999_999_999,
                        ..Entry::bg("mine", "j6")
                    },
                ),
                (
                    "pid-reused",
                    Entry {
                        started: Some("Mon Jan  1 00:00:00 1990".into()),
                        ..Entry::bg("mine", "j7")
                    },
                ),
            ];
            for (file, entry) in ignored {
                project.register(&format!("{file}.json"), entry);
            }
            std::fs::write(project.claude.sessions.join("junk.json"), "{").expect("writes");
            std::fs::write(project.claude.sessions.join("binary.json"), [0xff, 0xfe])
                .expect("writes");
            assert_eq!(project.tail(None, 2), ["--resume", "mine"]);
        }

        #[test]
        fn a_worker_in_another_directory_still_counts_but_one_in_the_agents_is_preferred() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            let elsewhere = tempfile::tempdir().expect("a temp dir");
            project.register(
                "a.json",
                Entry {
                    cwd: Some(elsewhere.path().to_path_buf()),
                    ..Entry::bg("mine", "faraway")
                },
            );
            let found = project.search(&["mine"]).found;
            assert_eq!(jobs_of(found).as_deref(), Some("faraway"));
            project.register("b.json", Entry::bg("mine", "nearby"));
            let found = project.search(&["mine"]).found;
            assert_eq!(jobs_of(found).as_deref(), Some("nearby"));
        }

        #[test]
        fn a_worker_with_a_job_id_is_preferred_to_one_without() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            project.register(
                "a.json",
                Entry {
                    job: None,
                    ..Entry::bg("mine", "")
                },
            );
            project.register("b.json", Entry::bg("mine", "reachable"));
            let found = project.search(&["mine"]).found;
            assert_eq!(jobs_of(found).as_deref(), Some("reachable"));
        }

        #[test]
        fn a_running_session_without_a_usable_job_id_is_reported_with_why_and_its_pid() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            project.register(
                "alive.json",
                Entry {
                    job: None,
                    ..Entry::bg("mine", "")
                },
            );
            let error = project.launched(None).expect_err("can't be attached");
            assert!(
                error.context.contains("no job id")
                    && error.context.contains(&std::process::id().to_string()),
                "{}",
                error.context
            );
            assert!(
                project
                    .launched(Some(&"go".parse().expect("parses")))
                    .is_err(),
                "it can't be stopped either"
            );
        }

        #[test]
        fn a_job_id_that_could_pass_for_a_flag_is_malformed_and_said_so() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            project.register("alive.json", Entry::bg("mine", "--help"));
            let found = project.search(&["mine"]).found;
            assert!(
                matches!(
                    found,
                    Some(Found::Unaddressable {
                        why: Unaddressed::Malformed(ref raw),
                        ..
                    }) if raw == "--help"
                ),
                "{found:?}"
            );
            let error = project.launched(None).expect_err("refused");
            assert!(error.context.contains("--help"), "{}", error.context);
        }

        #[test]
        fn job_ids_read_as_missing_malformed_or_valid() {
            let field = |json: &str| serde_json::from_str::<JobField>(json).expect("parses");
            assert_eq!(field("null"), JobField::Missing);
            assert!(matches!(field("\"470bb73a\""), JobField::Valid(_)));
            assert_eq!(field("\"-x\""), JobField::Malformed("-x".into()));
            assert_eq!(field("12"), JobField::Malformed("12".into()));
        }

        #[test]
        fn unreadable_entries_reach_the_launch_as_notes_not_as_output() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            std::fs::write(
                project.claude.sessions.join("odd.json"),
                r#"{"pid":1,"sessionId":"mine","kind":"bg"}"#,
            )
            .expect("writes");
            let spec = project.launched(None).expect("launches");
            assert!(
                spec.notes().iter().any(|n| n.contains("odd.json")),
                "{:?}",
                spec.notes()
            );
        }

        #[test]
        fn an_entry_of_the_conversation_that_cannot_be_read_is_reported_not_skipped_silently() {
            let project = Project::new();
            project.transcript("mine", &[user_line()], 1);
            std::fs::write(
                project.claude.sessions.join("odd.json"),
                r#"{"pid":1,"sessionId":"mine","kind":"bg"}"#,
            )
            .expect("writes");
            std::fs::write(
                project.claude.sessions.join("other.json"),
                r#"{"pid":1,"sessionId":"theirs"}"#,
            )
            .expect("writes");
            let search = project.search(&["mine"]);
            assert_eq!(search.found, None);
            assert_eq!(
                search.unreadable,
                [project.claude.sessions.join("odd.json")]
            );
        }

        #[test]
        fn a_conversation_is_looked_up_again_only_when_a_transcript_of_its_chain_changed() {
            let project = Project::new();
            project.transcript("a", &[user_line(), continued("a", "b")], 2);
            project.transcript("b", &[user_line()], 1);
            let id = |raw: &str| SessionId::parse(raw).expect("an id");
            let first = project
                .claude
                .chains
                .of(&project.dir, id("a"))
                .expect("reads");
            assert_eq!(first.resumable().as_str(), "b");
            std::fs::remove_file(project.dir.join("b.jsonl")).expect("removes");
            let again = project
                .claude
                .chains
                .of(&project.dir, id("a"))
                .expect("reads");
            assert_eq!(
                again.resumable().as_str(),
                "a",
                "the fork's transcript changed, so the chain is read again"
            );
        }

        fn prompted(text: &str) -> String {
            crate::fixtures::typed("p", "2026-11-08T10:00:00.000Z", text)
        }

        #[test]
        fn the_activity_is_read_from_the_conversations_live_transcript_not_a_superseded_one() {
            let project = Project::new();
            project.transcript(
                "old",
                &[prompted("from the old"), continued("old", "new")],
                5,
            );
            project.transcript("new", &[prompted("from the fork")], 1);

            let read = |at: Option<&TranscriptRef>| {
                project
                    .claude
                    .activity_detail(project.work.path(), at)
                    .expect("reads")
                    .and_then(|t| t.log.prompt)
            };
            let hooked = TranscriptRef::new(project.dir.join("old.jsonl").to_string_lossy());
            assert_eq!(read(Some(&hooked)).as_deref(), Some("from the fork"));
            assert_eq!(read(None).as_deref(), Some("from the fork"));
        }

        #[test]
        fn an_interrupt_is_looked_for_in_the_live_transcript_of_the_conversation() {
            let project = Project::new();
            project.transcript("old", &[prompted("go"), continued("old", "new")], 5);
            project.transcript(
                "new",
                &[
                    prompted("go"),
                    crate::fixtures::stopped(
                        "p",
                        "2026-11-08T10:00:05.000Z",
                        "[Request interrupted by user]",
                    ),
                ],
                1,
            );
            let hooked = TranscriptRef::new(project.dir.join("old.jsonl").to_string_lossy());
            let after = transcript::parse_time("2026-11-08T10:00:00.000Z").expect("a time");
            let probe = project
                .claude
                .interruption(&hooked, after, None)
                .expect("reads");
            assert!(matches!(probe, Probe::Interrupted(..)), "{probe:?}");
        }
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
                r#"{"hook_event_name":"Stop","agent_id":null,"last_assistant_message":"Waiting.","background_tasks":[{"id":"a1","type":"subagent","status":"running","agent_type":"Explore","description":"look"},{"id":"s1","type":"shell","status":"running","command":"cargo test","description":"tests"},{"id":"a2","type":"subagent","status":"completed"}]}"#
            ),
            Some(Reported::main(MainEvent::Finished {
                summary: Some("Waiting.".into()),
                running: vec![Background {
                    id: AgentId::new("a1"),
                    kind: "Explore".into(),
                    description: Some("look".into())
                }],
                commands: CommandListing::Listed(vec![shell("s1", "cargo test", Some("tests"))])
            }))
        );
    }

    fn shell(id: &str, command: &str, description: Option<&str>) -> ListedCommand {
        ListedCommand {
            id: id.parse().expect("shows"),
            command: command.parse().expect("not blank"),
            description: description.map(Into::into),
        }
    }

    fn stop_listing(commands: CommandListing) -> Reported {
        Reported::main(MainEvent::Finished {
            summary: None,
            running: Vec::new(),
            commands,
        })
    }

    #[test]
    fn a_stop_lists_only_shells_that_are_still_going() {
        assert_eq!(
            claude_event(
                r#"{"hook_event_name":"Stop","background_tasks":[{"id":"s1","type":"shell","status":"completed","command":"ls"},{"id":"s2","type":"shell","command":"make"},{"id":"s3","type":"shell","status":"pending","command":"npm start"},{"id":"s4","type":"shell","status":null,"command":"top"},{"id":"m1","type":"monitor","status":"running","command":"tail"}]}"#
            ),
            Some(stop_listing(CommandListing::Listed(vec![
                shell("s2", "make", None),
                shell("s3", "npm start", None),
                shell("s4", "top", None),
            ])))
        );
    }

    #[test]
    fn an_unreadable_shell_does_not_hide_the_others() {
        assert_eq!(
            claude_event(
                r#"{"hook_event_name":"Stop","background_tasks":[7,{"id":"","type":"shell","command":"a"},{"id":"s0","type":"shell"},{"id":"s1","type":"shell","command":7},{"id":"s5","type":"shell","command":" \n "},{"id":"s2","type":"shell","command":"b"}]}"#
            ),
            Some(stop_listing(CommandListing::Listed(vec![shell(
                "s2", "b", None
            )])))
        );
    }

    #[test]
    fn a_shell_whose_id_shows_nothing_is_skipped_like_one_with_no_id() {
        for blank in ["", " ", "\\u200b", "\\u0007", "\\ufe0f"] {
            let payload = format!(
                r#"{{"hook_event_name":"Stop","background_tasks":[{{"id":"{blank}","type":"shell","command":"a"}},{{"id":"s2","type":"shell","command":"b"}}]}}"#
            );
            assert_eq!(
                claude_event(&payload),
                Some(stop_listing(CommandListing::Listed(vec![shell(
                    "s2", "b", None
                )]))),
                "{blank}"
            );
        }
    }

    #[test]
    fn a_shells_command_and_description_are_kept_whole() {
        let long = "x".repeat(SNIPPET + 40);
        let payload = format!(
            r#"{{"hook_event_name":"Stop","background_tasks":[{{"id":"s1","type":"shell","command":"{long}\nsecond","description":"first\nsecond"}}]}}"#
        );
        assert_eq!(
            claude_event(&payload),
            Some(stop_listing(CommandListing::Listed(vec![shell(
                "s1",
                &format!("{long}\nsecond"),
                Some("first\nsecond")
            )])))
        );
    }

    #[test]
    fn a_stop_without_a_readable_task_list_says_the_commands_are_unknown() {
        for payload in [
            r#"{"hook_event_name":"Stop"}"#,
            r#"{"hook_event_name":"Stop","background_tasks":null}"#,
            r#"{"hook_event_name":"Stop","background_tasks":"none"}"#,
            r#"{"hook_event_name":"Stop","background_tasks":{"id":"s1"}}"#,
        ] {
            assert_eq!(
                claude_event(payload),
                Some(stop_listing(CommandListing::Unknown)),
                "{payload}"
            );
        }
    }

    #[test]
    fn a_non_string_command_does_not_drop_a_running_subagent_and_skips_a_shell() {
        assert_eq!(
            claude_event(
                r#"{"hook_event_name":"Stop","background_tasks":[{"id":"a1","type":"subagent","status":"running","command":["x"]},{"id":"s1","type":"shell","command":["x"]}]}"#
            ),
            Some(Reported::main(MainEvent::Finished {
                summary: None,
                running: vec![Background {
                    id: AgentId::new("a1"),
                    kind: "subagent".into(),
                    description: None
                }],
                commands: CommandListing::Listed(Vec::new())
            }))
        );
    }

    #[test]
    fn a_stop_with_an_empty_task_list_lists_no_commands() {
        assert_eq!(
            claude_event(r#"{"hook_event_name":"Stop","background_tasks":[]}"#),
            Some(stop_listing(CommandListing::Listed(Vec::new())))
        );
    }

    #[test]
    fn a_subagent_stop_carrying_shells_reports_only_the_subagent_stopping() {
        assert_eq!(
            claude_event(
                r#"{"hook_event_name":"SubagentStop","agent_id":"a1","background_tasks":[{"id":"s1","type":"shell","command":"make"}]}"#
            ),
            Some(Reported::subagent(
                AgentId::new("a1"),
                SubagentEvent::Stopped
            ))
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
    fn claude_tells_of_an_interrupt_only_in_its_transcript() {
        let reported = Claude::with_program(Path::new("/h"), "/usr/bin/true".into())
            .event(r#"{"hook_event_name":"Stop"}"#)
            .expect("an event");
        assert_eq!(
            reported.interrupts,
            agent_core::activity::InterruptSignal::Transcript
        );
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
                }],
                commands: CommandListing::Listed(Vec::new())
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

    mod interruption {
        use super::*;
        use crate::fixtures::{said, stopped, typed, user, write};

        const BEFORE: &str = "2026-11-08T10:00:00.000Z";
        const MARKER: &str = "2026-11-08T10:00:05.000Z";
        const AFTER: &str = "2026-11-08T10:00:09.000Z";

        struct Setup {
            _home: tempfile::TempDir,
            claude: Claude,
            file: PathBuf,
        }

        fn setup(lines: &[String]) -> Setup {
            let home = tempfile::tempdir().expect("a temp dir");
            let claude = Claude::with_program(home.path(), "/usr/bin/true".into());
            let dir = claude.projects.join("proj");
            std::fs::create_dir_all(&dir).expect("creates");
            let file = write(&dir, lines);
            Setup {
                _home: home,
                claude,
                file,
            }
        }

        fn since(stamp: &str) -> SystemTime {
            transcript::parse_time(stamp).expect("a time")
        }

        fn asked(s: &Setup, after: &str, seen: Option<&TranscriptStamp>) -> Probe {
            let at = TranscriptRef::new(s.file.to_string_lossy());
            s.claude
                .interruption(&at, since(after), seen)
                .expect("reads")
        }

        fn marker() -> Vec<String> {
            vec![
                typed("p", BEFORE, "go"),
                said("2026-11-08T10:00:01.000Z", "On it."),
                stopped("p", MARKER, "[Request interrupted by user]"),
            ]
        }

        #[test]
        fn a_marker_later_than_the_activity_began_is_an_interrupt_whatever_bookkeeping_follows() {
            let mut lines = marker();
            lines.extend(["last-prompt", "cost-state"].map(crate::fixtures::bookkeeping));
            let s = setup(&lines);
            assert!(matches!(asked(&s, BEFORE, None), Probe::Interrupted(..)));
        }

        #[test]
        fn a_marker_from_before_the_activity_began_is_not() {
            let s = setup(&marker());
            assert!(matches!(asked(&s, AFTER, None), Probe::Older(_)));
        }

        #[test]
        fn work_after_the_marker_means_it_carried_on() {
            let mut lines = marker();
            lines.push(typed("q", AFTER, "try again"));
            let s = setup(&lines);
            assert!(matches!(asked(&s, MARKER, None), Probe::Going(_)));
        }

        #[test]
        fn the_marker_only_counts_by_its_message_id_or_as_a_lone_text_block() {
            let plain = user("p", MARKER, r#""[Request interrupted by user]""#, "");
            let with_more = user(
                "p",
                MARKER,
                r#"[{"type":"text","text":"[Request interrupted by user]"},{"type":"text","text":"and then"}]"#,
                "",
            );
            let named = user(
                "p",
                MARKER,
                r#"[{"type":"text","text":"stopped"}]"#,
                r#","interruptedMessageId":"msg_1""#,
            );
            for (record, interrupted) in [(plain, false), (with_more, false), (named, true)] {
                let s = setup(&[record]);
                assert_eq!(
                    matches!(asked(&s, BEFORE, None), Probe::Interrupted(..)),
                    interrupted
                );
            }
        }

        #[test]
        fn a_half_written_last_line_leaves_the_file_inconclusive() {
            let s = setup(&marker());
            let mut text = std::fs::read_to_string(&s.file).expect("reads");
            text.push_str(r#"{"type":"assistant","timest"#);
            std::fs::write(&s.file, &text).expect("writes");
            assert_eq!(asked(&s, BEFORE, None), Probe::Unchanged);
            std::fs::write(&s.file, text.trim_end_matches(|c| c != '\n')).expect("writes");
            assert!(matches!(asked(&s, BEFORE, None), Probe::Interrupted(..)));
        }

        #[test]
        fn more_bookkeeping_than_the_window_still_finds_the_marker_and_saves_no_stamp_without_one()
        {
            let mut lines = marker();
            let filler = format!(r#"{{"type":"attachment","pad":"{}"}}"#, "x".repeat(4096));
            lines.extend(std::iter::repeat_n(
                filler,
                (usize::try_from(PROBE_BYTES).expect("fits") / 4096) * 2,
            ));
            let s = setup(&lines);
            assert!(matches!(asked(&s, BEFORE, None), Probe::Interrupted(..)));

            let none = setup(&[
                format!(
                    r#"{{"type":"attachment","pad":"{}"}}"#,
                    "x".repeat(usize::try_from(TAIL_BYTES).expect("fits") * 2)
                ),
                crate::fixtures::bookkeeping("last-prompt"),
            ]);
            assert_eq!(asked(&none, BEFORE, None), Probe::Unchanged);
        }

        #[test]
        fn a_file_unchanged_since_its_stamp_is_not_read_again() {
            let s = setup(&[typed("p", BEFORE, "go")]);
            let Probe::Going(stamp) = asked(&s, "2026-11-08T09:00:00.000Z", None) else {
                panic!("it went on");
            };
            assert_eq!(asked(&s, BEFORE, Some(&stamp)), Probe::Unchanged);
            let mut lines = marker();
            lines.insert(0, typed("o", "2026-11-08T09:59:00.000Z", "earlier"));
            std::fs::write(&s.file, lines.join("\n") + "\n").expect("writes");
            assert!(matches!(
                asked(&s, BEFORE, Some(&stamp)),
                Probe::Interrupted(..)
            ));
        }

        #[test]
        fn a_path_outside_the_projects_directory_is_no_transcript() {
            let s = setup(&marker());
            let outside = s
                .file
                .parent()
                .expect("a dir")
                .join("../../elsewhere.jsonl");
            std::fs::write(&outside, "").expect("writes");
            let at = TranscriptRef::new(outside.to_string_lossy());
            assert_eq!(
                s.claude
                    .interruption(&at, since(BEFORE), None)
                    .expect("refused quietly"),
                Probe::NoTranscript
            );
        }
    }
}
