use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agent_core::activity::{About, AgentId, Background, Event, Input, Origin, Reported, ToolId};
use agent_core::names::Prompt;
use agent_core::ports::{
    Harness, HarnessError, HarnessId, Harnesses, LaunchCtx, PendingPrompt, Seen,
};
use agent_core::process::LaunchSpec;

const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
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
    permission_mode: String,
    add_dirs: Vec<PathBuf>,
}

impl Claude {
    pub fn new(home: &Path) -> Self {
        Self {
            id: "claude".parse().expect("valid id"),
            program: std::env::var_os("AGENT_CLAUDE").unwrap_or_else(|| "claude".into()),
            quick_model: "haiku".into(),
            projects: home.join(".claude/projects"),
            permission_mode: "auto".into(),
            add_dirs: vec![PathBuf::from("/tmp")],
        }
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
        recent.sort_by(|a, b| b.0.cmp(&a.0));
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
                    Input::User(snippet(prompt))
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
            summary: text("last_assistant_message").map(snippet),
            running: background_subagents(payload),
        },
        "StopFailure" => Event::Failed(text("error").unwrap_or("error").replace('_', " ")),
        "SessionEnd" => Event::Ended,
        _ => return None,
    };
    Some(Reported { origin, event })
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
        let claude = Claude::new(home.path());

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
}
