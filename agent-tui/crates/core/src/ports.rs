use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::activity::{Activity, Reported};
use crate::model::{Head, Presence, Running, Worktree};
use crate::names::{BranchName, Prompt, SessionName};
use crate::paths::{AgentWorktree, RepoRoot};
use crate::process::{CommandError, LaunchSpec};

/// A repository's worktrees: the main checkout and the linked ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoWorktrees {
    pub main: Worktree,
    pub linked: Vec<Worktree>,
}

/// A worktree's work since it forked from the default branch.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorktreeFacts {
    /// Commits on top of the fork point.
    pub ahead: usize,
    /// Commits on the default branch since the fork point.
    pub behind: usize,
    /// Committed, uncommitted and untracked changes since the fork point.
    pub changed: Vec<Change>,
    /// Modified, staged and untracked files.
    pub dirty: usize,
}

/// A file changed since the fork point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub lines: Lines,
}

/// How a changed file's lines changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    Counted {
        added: usize,
        removed: usize,
    },
    Binary,
    /// A new file git isn't tracking yet.
    Untracked,
}

impl WorktreeFacts {
    pub fn diff(&self) -> DiffStat {
        let counted = || {
            self.changed.iter().filter_map(|c| match c.lines {
                Lines::Counted { added, removed } => Some((added, removed)),
                Lines::Binary | Lines::Untracked => None,
            })
        };
        DiffStat {
            files: self.changed.len(),
            added: counted().map(|(added, _)| added).sum(),
            removed: counted().map(|(_, removed)| removed).sum(),
        }
    }
}

/// Lines changed across a worktree's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStat {
    pub files: usize,
    pub added: usize,
    pub removed: usize,
}

/// A branch a [`Vcs`] found in the repo's local refs. Only `Vcs`
/// implementations should construct one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBranch(BranchName);

impl LocalBranch {
    pub fn found(name: BranchName) -> Self {
        Self(name)
    }

    pub fn name(&self) -> &BranchName {
        &self.0
    }
}

/// A branch a [`Vcs`] found on origin. Only `Vcs` implementations should
/// construct one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBranch(BranchName);

impl RemoteBranch {
    pub fn found(name: BranchName) -> Self {
        Self(name)
    }

    pub fn name(&self) -> &BranchName {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeStart {
    /// Detached at origin's default branch, or at HEAD if there is no origin.
    DefaultBase,
    Branch(LocalBranch),
    /// A new local branch tracking origin's.
    TrackRemote(RemoteBranch),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchState {
    Missing,
    /// No commits ahead of the default base, or pushed to its own remote
    /// branch which has since been deleted (merged with auto-delete).
    Safe,
    Unmerged {
        ahead: NonZeroUsize,
    },
    /// Its upstream branch was deleted (often: merged with auto-delete), but
    /// commits after the last push can't be ruled out.
    UpstreamGone {
        ahead: NonZeroUsize,
    },
    /// There's no default base to compare against.
    Unknown,
}

pub trait Vcs: Send + Sync {
    /// The repo containing `dir`, or `None` when it isn't in one.
    fn repo_of(&self, dir: &Path) -> Result<Option<RepoRoot>, CommandError>;
    fn worktrees(&self, repo: &RepoRoot) -> Result<RepoWorktrees, CommandError>;
    fn head(&self, dir: &Path) -> Result<Head, CommandError>;
    fn add_worktree(
        &self,
        repo: &RepoRoot,
        at: &AgentWorktree,
        start: &WorktreeStart,
    ) -> Result<(), CommandError>;
    fn prune_worktrees(&self, repo: &RepoRoot) -> Result<(), CommandError>;
    fn local_branch(
        &self,
        repo: &RepoRoot,
        branch: &BranchName,
    ) -> Result<Option<LocalBranch>, CommandError>;
    fn remote_branch(
        &self,
        repo: &RepoRoot,
        branch: &BranchName,
    ) -> Result<Option<RemoteBranch>, CommandError>;
    fn branch_state(
        &self,
        repo: &RepoRoot,
        branch: &BranchName,
    ) -> Result<BranchState, CommandError>;
    /// Modified, staged and untracked files in a worktree.
    fn dirty_files(&self, worktree: &Path) -> Result<usize, CommandError>;
    /// Commits reachable from the worktree's HEAD but from no branch or
    /// remote ref (work on a detached HEAD).
    fn unreachable_commits(&self, worktree: &Path) -> Result<usize, CommandError>;
    fn delete_branch(&self, repo: &RepoRoot, branch: LocalBranch) -> Result<(), CommandError>;
    /// What a worktree has done since it forked from the default branch,
    /// without taking locks the agent working in it might need.
    fn facts(&self, worktree: &Path) -> Result<WorktreeFacts, CommandError>;
}

/// Where a live session's terminal is, when the host could tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    Live {
        cwd: Option<PathBuf>,
    },
    /// Dead but resurrectable. `cwd` is where it last ran, when the host
    /// remembers.
    Exited {
        cwd: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub name: SessionName,
    pub status: SessionStatus,
}

impl SessionInfo {
    pub fn cwd(&self) -> Option<&Path> {
        match &self.status {
            SessionStatus::Live { cwd } | SessionStatus::Exited { cwd } => cwd.as_deref(),
        }
    }
}

impl SessionStatus {
    pub fn presence(&self) -> Presence {
        match self {
            Self::Live { .. } => Presence::Live,
            Self::Exited { .. } => Presence::Exited,
        }
    }
}

/// A terminal client attached to a session (a process id for zellij).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientId(u32);

impl ClientId {
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("couldn't detach client {client}: {reason}")]
    Detach { client: ClientId, reason: String },
}

/// Hosts whose sessions accept several clients can report and detach them.
pub trait ClientControl: Send + Sync {
    /// Clients attached to `name`, excluding `own` and anything it spawned.
    fn other_clients(
        &self,
        name: &SessionName,
        own: Option<ClientId>,
    ) -> Result<Vec<ClientId>, HostError>;
    fn detach(&self, clients: &[ClientId]) -> Result<(), HostError>;
}

/// The command a new session's pane runs. Hosts store it to re-run when a
/// session is resurrected, so it carries no environment of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCommand {
    pub program: OsString,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// Where agent sessions persist (zellij today).
pub trait SessionHost: Send + Sync {
    fn sessions(&self) -> Result<Vec<SessionInfo>, HostError>;
    /// Attach to an existing (live or resurrectable) session in the foreground.
    fn attach(&self, name: &SessionName) -> LaunchSpec;
    /// Create `name` running `pane`, replacing a dead session of the same
    /// name, and attach in the foreground.
    fn create(&self, name: &SessionName, pane: &PaneCommand) -> Result<LaunchSpec, HostError>;
    /// Run `name` with no client attached. A live session is left alone;
    /// otherwise it starts with the pane its last `create` set up.
    fn start_detached(&self, name: &SessionName) -> Result<(), HostError>;
    /// Stop a session; it can be resurrected later.
    fn kill(&self, session: &Running) -> Result<(), HostError>;
    /// Stop a session and forget it.
    fn delete(&self, name: &SessionName) -> Result<(), HostError>;
    /// Raw input for the session's focused pane.
    fn write(&self, name: &SessionName, bytes: &[u8]) -> Result<(), HostError>;
    fn screen(&self, name: &SessionName) -> Result<String, HostError>;
    fn clients(&self) -> Option<&dyn ClientControl>;
    /// Environment variables to drop so a client can run inside the host.
    fn nesting_env(&self) -> &'static [&'static str];
}

/// Names a harness in the registry and in agent records.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HarnessId(String);

#[derive(Debug, Clone, thiserror::Error)]
#[error("'{0}' is not a harness id (lowercase letters, digits and '-')")]
pub struct InvalidHarnessId(String);

impl FromStr for HarnessId {
    type Err = InvalidHarnessId;

    fn from_str(s: &str) -> Result<Self, InvalidHarnessId> {
        let valid = !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if valid {
            Ok(Self(s.into()))
        } else {
            Err(InvalidHarnessId(s.into()))
        }
    }
}

impl TryFrom<String> for HarnessId {
    type Error = InvalidHarnessId;

    fn try_from(s: String) -> Result<Self, InvalidHarnessId> {
        s.parse()
    }
}

impl From<HarnessId> for String {
    fn from(id: HarnessId) -> String {
        id.0
    }
}

impl fmt::Display for HarnessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub struct LaunchCtx<'a> {
    pub session: &'a SessionName,
    pub workdir: &'a Path,
    pub bootstrap: Option<&'a Prompt>,
    /// The agent binary. Harnesses with hooks report through it with
    /// `<report> event <session>`, the hook's JSON payload on stdin.
    pub report: &'a Path,
}

#[derive(Debug, thiserror::Error)]
#[error("{context}: {source}")]
pub struct HarnessError {
    pub context: String,
    #[source]
    pub source: io::Error,
}

/// A coding agent (Claude Code, …): how to start or resume it in a worktree
/// and how to type into it.
pub trait Harness: Send + Sync {
    fn id(&self) -> &HarnessId;
    /// A short human name, e.g. "claude".
    fn label(&self) -> &'static str;
    /// Resume the worktree's conversation if there is one, otherwise start
    /// fresh; either way `bootstrap`, if any, is sent as the next prompt.
    fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchSpec, HarnessError>;
    /// What a hook payload reported through `agent event` means, if it's
    /// anything the TUI shows.
    fn event(&self, _payload: &str) -> Option<Reported> {
        None
    }
    /// Whether the agent has recorded any activity in `workdir` since `since`,
    /// i.e. a prompt queued then has been received.
    /// Evidence the agent was active after `pending` was queued, so it has
    /// seen the prompt.
    fn acknowledged(
        &self,
        workdir: &Path,
        pending: &PendingPrompt,
    ) -> Result<Option<Seen>, HarnessError>;
    /// The bytes that type `prompt` into a running agent and submit it.
    fn submit(&self, prompt: &Prompt) -> Vec<u8>;
    /// A one-shot, non-interactive answer to `question` from the harness's
    /// quickest model (`claude -p`, `codex exec`, …), or `None` if it can't
    /// answer one-off questions. May take seconds: call it off the UI thread.
    fn ask(&self, question: &Prompt) -> Result<Option<String>, HarnessError> {
        let _ = question;
        Ok(None)
    }
}

pub trait Harnesses: Send + Sync {
    fn get(&self, id: &HarnessId) -> Option<&dyn Harness>;
    fn default_harness(&self) -> &dyn Harness;
    fn all(&self) -> Vec<&dyn Harness>;
}

/// A prompt waiting for the agent's next launch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPrompt {
    pub prompt: Prompt,
    pub queued: SystemTime,
}

impl PendingPrompt {
    pub fn now(prompt: Prompt) -> Self {
        Self {
            prompt,
            queued: SystemTime::now(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub harness: HarnessId,
    /// Sent on launches until the agent shows activity after it was queued.
    #[serde(default)]
    pub pending: Option<PendingPrompt>,
}

impl AgentRecord {
    /// The agent has seen its pending prompt, so it's no longer sent.
    pub fn acknowledge(&mut self, _: Seen) {
        self.pending = None;
    }
}

/// Activity a harness observed at or after a prompt was queued. Only
/// harnesses should construct one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Seen(SystemTime);

impl Seen {
    pub fn at(activity: SystemTime) -> Self {
        Self(activity)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}: {detail}")]
    Corrupt { path: PathBuf, detail: String },
}

pub trait Store: Send + Sync {
    /// Pinned sessions in pin order.
    fn pinned(&self) -> Result<Vec<SessionName>, StoreError>;
    /// Pins `name` last; already pinned is fine.
    fn pin(&self, name: &SessionName) -> Result<(), StoreError>;
    /// Unpinned already is fine.
    fn unpin(&self, name: &SessionName) -> Result<(), StoreError>;
    fn record(&self, name: &SessionName) -> Result<Option<AgentRecord>, StoreError>;
    fn put_record(&self, name: &SessionName, record: &AgentRecord) -> Result<(), StoreError>;
    fn forget(&self, name: &SessionName) -> Result<(), StoreError>;
    /// Applies what `name`'s harness reported to its activity.
    fn report(&self, name: &SessionName, event: Reported) -> Result<(), StoreError>;
    /// The user has looked at `name`: a finished turn stops being news.
    fn acknowledge(&self, name: &SessionName) -> Result<(), StoreError>;
    /// Every agent's latest activity.
    fn activities(&self) -> Result<HashMap<SessionName, Activity>, StoreError>;
    /// The agent last selected in the TUI, to start on next time.
    fn last_selected(&self) -> Result<Option<SessionName>, StoreError>;
    fn set_last_selected(&self, name: &SessionName) -> Result<(), StoreError>;
}
