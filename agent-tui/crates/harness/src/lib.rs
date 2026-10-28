use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agent_core::names::Prompt;
use agent_core::ports::{Harness, HarnessError, HarnessId, Harnesses, LaunchCtx, PendingPrompt, Seen};
use agent_core::process::LaunchSpec;

const ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
        let slug: String =
            workdir.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        self.projects.join(slug)
    }

    /// The conversation to resume: the largest transcript written since the
    /// worktree was created. Older ones belong to a previous agent that used
    /// the same name; the largest beats the newest because a botched restart
    /// leaves a tiny new transcript behind.
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

    fn resumable(&self, workdir: &Path) -> io::Result<Option<String>> {
        let born = std::fs::metadata(workdir)?.created().unwrap_or(SystemTime::UNIX_EPOCH);
        let mut best: Option<(u64, String)> = None;
        for (path, meta) in self.transcripts(workdir)? {
            if meta.modified()? < born {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
            if best.as_ref().is_none_or(|(len, _)| meta.len() > *len) {
                best = Some((meta.len(), id.to_string()));
            }
        }
        Ok(best.map(|(_, id)| id))
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
        let workdir = ctx
            .workdir
            .canonicalize()
            .map_err(|source| HarnessError { context: format!("resolving {}", ctx.workdir.display()), source })?;
        let resume = self
            .resumable(&workdir)
            .map_err(|source| HarnessError { context: "looking for a conversation to resume".into(), source })?;

        let mut spec = LaunchSpec::new(&self.program)
            .cwd(&workdir)
            .env_remove(INHERITED_SESSION_ENV.iter().copied())
            .env("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE", "1");
        for dir in &self.add_dirs {
            spec = spec.arg("--add-dir").arg(dir);
        }
        spec = spec.args(["--permission-mode", self.permission_mode.as_str()]);
        if let Some(id) = resume {
            spec = spec.args(["--resume".to_string(), id]);
        }
        Ok(match ctx.bootstrap {
            Some(prompt) => spec.arg("--").arg(prompt.as_str()),
            None => spec,
        })
    }

    fn acknowledged(&self, workdir: &Path, pending: &PendingPrompt) -> Result<Option<Seen>, HarnessError> {
        let context = || format!("reading transcripts for {}", workdir.display());
        let workdir = workdir.canonicalize().map_err(|source| HarnessError { context: context(), source })?;
        let transcripts = self.transcripts(&workdir).map_err(|source| HarnessError { context: context(), source })?;
        let latest = transcripts.iter().filter_map(|(_, meta)| meta.modified().ok()).max();
        Ok(latest.filter(|m| *m >= pending.queued).map(Seen::at))
    }

    /// `claude -p` with the quick model: print one answer and exit.
    fn ask(&self, question: &Prompt) -> Result<Option<String>, HarnessError> {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.env("LC_ALL", "C").stdin(std::process::Stdio::null());
        cmd.args(["-p", "--model", self.quick_model.as_str()]).arg(question.as_str());
        for key in INHERITED_SESSION_ENV {
            cmd.env_remove(key);
        }
        let out = agent_core::process::output_within(&mut cmd, ASK_TIMEOUT)
            .map_err(|e| HarnessError { context: "asking claude".into(), source: io::Error::other(e) })?;
        Ok(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
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
        Self { claude: Claude::new(home) }
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
    fn launches_fresh_with_the_bootstrap_then_resumes_the_largest_transcript() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let claude = Claude::new(home.path());

        let go: Prompt = "--go".parse().unwrap();
        let session = "fix".parse().unwrap();
        let ctx = |bootstrap| LaunchCtx { session: &session, workdir: work.path(), bootstrap };
        let fresh = claude.launch(&ctx(Some(&go))).unwrap();
        let tail: Vec<_> = fresh.arguments().iter().rev().take(2).collect();
        assert_eq!(tail, ["--go", "--"], "the prompt can't be mistaken for a flag");
        assert!(fresh.removed_environment().iter().any(|k| k == "CLAUDECODE"));

        let dir = claude.project_dir(&work.path().canonicalize().unwrap());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("small.jsonl"), "x").unwrap();
        std::fs::write(dir.join("big.jsonl"), "xxxxxxxx").unwrap();
        let resumed = claude.launch(&ctx(None)).unwrap();
        let args = resumed.arguments();
        assert_eq!(args[args.len() - 2..], ["--resume", "big"]);
    }
}
