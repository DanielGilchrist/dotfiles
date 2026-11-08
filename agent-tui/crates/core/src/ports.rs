use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::activity::{Activity, AgentId, Reported, ToolId, TranscriptRef};
use crate::model::{Head, Presence, Running, Worktree};
use crate::names::{BranchName, NameError, Prompt, PullRequest, SessionName};
use crate::paths::{AgentWorktree, RepoRoot};
use crate::process::{CommandError, LaunchSpec};

/// A repository's worktrees: the main checkout and the linked ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoWorktrees {
    pub main: Worktree,
    pub linked: Vec<Worktree>,
}

/// A worktree's work since it forked from the default branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeFacts {
    /// Commits on top of the fork point.
    pub ahead: usize,
    /// Commits on the default branch since the fork point.
    pub behind: usize,
    /// Committed, uncommitted and untracked changes since the fork point.
    pub changed: Vec<Change>,
    /// The files with uncommitted state, counted against the last commit.
    pub pending: Vec<Change>,
    /// What the fork point is measured against, and from where.
    pub compared: Compared,
}

/// What a worktree has checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checkout {
    Branch(BranchName),
    /// A detached HEAD, as its short sha.
    Detached(String),
}

impl Checkout {
    /// The branch or short sha.
    pub fn head(&self) -> &str {
        match self {
            Self::Branch(branch) => branch.as_str(),
            Self::Detached(sha) => sha,
        }
    }
}

/// A remote-tracking ref such as `origin/main`: a branch name behind a
/// remote's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRef(BranchName);

impl RemoteRef {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Whether this is `origin`'s copy of `branch`.
    pub fn tracks(&self, branch: &BranchName) -> bool {
        self.as_str().strip_prefix("origin/") == Some(branch.as_str())
    }
}

impl FromStr for RemoteRef {
    type Err = NameError;

    fn from_str(s: &str) -> Result<Self, NameError> {
        if !s.contains('/') {
            return Err(NameError::InvalidBranch(s.into()));
        }
        s.parse().map(Self)
    }
}

impl fmt::Display for RemoteRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a checkout stands against origin's default branch. `base` is the
/// remote ref, `origin/main`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compared {
    /// The local branch the base is a remote copy of: `main` on `origin/main`.
    Trunk {
        branch: BranchName,
        base: RemoteRef,
    },
    Branch {
        branch: BranchName,
        base: RemoteRef,
    },
    Detached {
        sha: String,
        base: RemoteRef,
    },
    /// No default branch to compare with.
    NoBase {
        checkout: Checkout,
    },
}

impl Compared {
    pub fn of(checkout: Checkout, base: Option<RemoteRef>) -> Self {
        match (checkout, base) {
            (checkout, None) => Self::NoBase { checkout },
            (Checkout::Detached(sha), Some(base)) => Self::Detached { sha, base },
            (Checkout::Branch(branch), Some(base)) => {
                if base.tracks(&branch) {
                    Self::Trunk { branch, base }
                } else {
                    Self::Branch { branch, base }
                }
            }
        }
    }

    /// The branch or short sha the checkout is at.
    pub fn head(&self) -> &str {
        match self {
            Self::Trunk { branch, .. } | Self::Branch { branch, .. } => branch.as_str(),
            Self::Detached { sha, .. } => sha,
            Self::NoBase { checkout } => checkout.head(),
        }
    }

    pub fn base(&self) -> Option<&RemoteRef> {
        match self {
            Self::Trunk { base, .. } | Self::Branch { base, .. } | Self::Detached { base, .. } => {
                Some(base)
            }
            Self::NoBase { .. } => None,
        }
    }
}

/// A file changed since the fork point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

/// A file git tracks has line counts and a state. One it doesn't has
/// neither, since there is no diff to count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Untracked,
    Tracked { lines: Lines, state: TrackedState },
}

impl Change {
    pub fn tracked(path: impl Into<PathBuf>, lines: Lines, state: TrackedState) -> Self {
        Self {
            path: path.into(),
            kind: ChangeKind::Tracked { lines, state },
        }
    }

    pub fn untracked(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            kind: ChangeKind::Untracked,
        }
    }

    /// A file that `git status` lists, before any diff was counted.
    pub fn listed(file: &Uncommitted) -> Self {
        match file.state.tracked() {
            Some(state) => Self::tracked(file.path.clone(), Lines::Unknown, state),
            None => Self::untracked(file.path.clone()),
        }
    }

    pub fn state(&self) -> FileState {
        match self.kind {
            ChangeKind::Untracked => FileState::Untracked,
            ChangeKind::Tracked { state, .. } => state.into(),
        }
    }

    /// `None` for an untracked file.
    pub fn lines(&self) -> Option<Lines> {
        match self.kind {
            ChangeKind::Untracked => None,
            ChangeKind::Tracked { lines, .. } => Some(lines),
        }
    }
}

/// What was done to a file in the index or the working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Modified,
    Added,
    Deleted,
}

/// Where a file git tracks stands against the last commit. A file only in
/// the fork diff and clean now is `Committed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackedState {
    Committed,
    Staged(Edit),
    Unstaged(Edit),
    Both {
        staged: Edit,
        unstaged: Edit,
    },
    /// Unmerged: both sides changed it, or one deleted what the other kept.
    Conflicted,
}

/// Where a changed file stands against the last commit, as `git status`
/// reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Committed,
    Untracked,
    Staged(Edit),
    Unstaged(Edit),
    Both { staged: Edit, unstaged: Edit },
    Conflicted,
}

impl From<TrackedState> for FileState {
    fn from(state: TrackedState) -> Self {
        match state {
            TrackedState::Committed => Self::Committed,
            TrackedState::Staged(edit) => Self::Staged(edit),
            TrackedState::Unstaged(edit) => Self::Unstaged(edit),
            TrackedState::Both { staged, unstaged } => Self::Both { staged, unstaged },
            TrackedState::Conflicted => Self::Conflicted,
        }
    }
}

impl FileState {
    /// `None` for a file git doesn't track.
    pub fn tracked(self) -> Option<TrackedState> {
        match self {
            Self::Committed => Some(TrackedState::Committed),
            Self::Untracked => None,
            Self::Staged(edit) => Some(TrackedState::Staged(edit)),
            Self::Unstaged(edit) => Some(TrackedState::Unstaged(edit)),
            Self::Both { staged, unstaged } => Some(TrackedState::Both { staged, unstaged }),
            Self::Conflicted => Some(TrackedState::Conflicted),
        }
    }

    pub fn is_committed(self) -> bool {
        self == Self::Committed
    }

    pub fn staged(self) -> Option<Edit> {
        match self {
            Self::Staged(edit) | Self::Both { staged: edit, .. } => Some(edit),
            Self::Committed | Self::Untracked | Self::Unstaged(_) | Self::Conflicted => None,
        }
    }

    pub fn unstaged(self) -> Option<Edit> {
        match self {
            Self::Unstaged(edit) | Self::Both { unstaged: edit, .. } => Some(edit),
            Self::Committed | Self::Untracked | Self::Staged(_) | Self::Conflicted => None,
        }
    }
}

/// A changed file as a diff, for reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDiff {
    /// The hunks, one entry per line, without git's header. `more` lines
    /// beyond what's kept were left out.
    Text { lines: Vec<String>, more: usize },
    /// Not text: its size on disk now, when it still exists.
    Binary { bytes: Option<u64> },
}

/// What a worktree has done since it forked from the default branch, for
/// reading: its commits and its uncommitted files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BranchLog {
    /// One line per commit on top of the fork point, newest first.
    pub commits: Vec<String>,
    /// Modified, staged and untracked files.
    pub uncommitted: Vec<Uncommitted>,
}

/// A file with uncommitted state, as `git status` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uncommitted {
    pub path: PathBuf,
    pub state: FileState,
}

/// How a changed file's lines changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    Counted {
        added: usize,
        removed: usize,
    },
    Binary,
    /// Listed by git status with nothing to count against `HEAD`, such as a
    /// conflict or a staged edit reverted in the worktree.
    Unknown,
}

impl WorktreeFacts {
    /// A worktree with no commits of its own and nothing uncommitted.
    pub fn unchanged(compared: Compared) -> Self {
        Self {
            ahead: 0,
            behind: 0,
            changed: Vec::new(),
            pending: Vec::new(),
            compared,
        }
    }

    /// Modified, staged and untracked files.
    pub fn dirty(&self) -> usize {
        self.pending.len()
    }

    /// Totals since the fork point.
    pub fn diff(&self) -> DiffStat {
        DiffStat::of(&self.changed)
    }

    /// Totals of the uncommitted files, against the last commit.
    pub fn pending_diff(&self) -> DiffStat {
        DiffStat::of(&self.pending)
    }

    /// How close the work is to shipping. Uncommitted files come first,
    /// since nothing else is final until they're committed.
    pub fn readiness(&self) -> Readiness {
        match (
            NonZeroUsize::new(self.dirty()),
            NonZeroUsize::new(self.ahead),
            NonZeroUsize::new(self.behind),
        ) {
            (Some(dirty), ..) => Readiness::Dirty {
                dirty,
                ahead: self.ahead,
            },
            (None, None, _) => Readiness::NothingAhead,
            (None, Some(ahead), Some(behind)) => Readiness::Behind { behind, ahead },
            (None, Some(ahead), None) => Readiness::Ready { ahead },
        }
    }
}

/// Whether a worktree's work could be shipped as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// Files not committed yet, on top of `ahead` commits.
    Dirty { dirty: NonZeroUsize, ahead: usize },
    /// Committed, but the default branch has moved on since the fork.
    Behind {
        behind: NonZeroUsize,
        ahead: NonZeroUsize,
    },
    /// Committed and up to date with the default branch.
    Ready { ahead: NonZeroUsize },
    /// Clean, with no commits of its own.
    NothingAhead,
}

/// Lines changed across a worktree's files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStat {
    pub files: usize,
    pub added: usize,
    pub removed: usize,
}

impl DiffStat {
    pub fn of(changes: &[Change]) -> Self {
        let counted = || {
            changes.iter().filter_map(|c| match c.kind {
                ChangeKind::Tracked {
                    lines: Lines::Counted { added, removed },
                    ..
                } => Some((added, removed)),
                ChangeKind::Tracked {
                    lines: Lines::Binary | Lines::Unknown,
                    ..
                }
                | ChangeKind::Untracked => None,
            })
        };
        Self {
            files: changes.len(),
            added: counted().map(|(added, _)| added).sum(),
            removed: counted().map(|(_, removed)| removed).sum(),
        }
    }
}

/// What a file's diff is read against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffBase {
    /// The last commit: what is uncommitted.
    Head,
    /// The fork point: committed and uncommitted together.
    Fork,
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
    /// A local branch at origin's, tracking nothing: the remote-tracking
    /// branch is a fetched pull request head, which no push could update.
    /// An old local branch of that name is reset to it.
    AtRemote(RemoteBranch),
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
    /// Fetches one ref from origin into its remote-tracking branch, so a
    /// branch pushed since the last fetch, or a pull request's head, can
    /// be found. A ref origin lacks is nothing fetched, not a failure.
    fn fetch(&self, repo: &RepoRoot, what: &Fetch) -> Result<(), CommandError>;
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
    /// The worktree's commits since it forked, and its uncommitted files.
    /// Slow on a big repo: call it off the UI thread.
    fn branch_log(&self, worktree: &Path) -> Result<BranchLog, CommandError>;
    /// The files commit `sha` changed, each with its diff against its
    /// parent. Slow on big commits: call it off the UI thread.
    fn commit_diff(
        &self,
        worktree: &Path,
        sha: &str,
    ) -> Result<Vec<(Change, FileDiff)>, CommandError> {
        let _ = (worktree, sha);
        Ok(Vec::new())
    }
    /// `change`'s file against `base`: the last commit, or the fork point
    /// from the default branch (committed and uncommitted together). A new
    /// untracked file is shown whole. Slow on big diffs: call it off the UI
    /// thread.
    fn file_diff(
        &self,
        worktree: &Path,
        change: &Change,
        base: DiffBase,
    ) -> Result<FileDiff, CommandError>;
}

/// What to fetch from origin, and the remote-tracking branch it lands in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    /// The branch itself, into `origin/<branch>`.
    Branch(BranchName),
    /// A pull request's head, by the `refs/pull/N/head` ref GitHub keeps
    /// on the base repo, into `origin/pr/N`. A fork's branch has no name
    /// of its own on origin, and one it shares with a base branch would
    /// be the wrong code.
    PullRequest(u64),
}

impl Fetch {
    /// The remote-tracking branch the fetch fills.
    pub fn branch(&self) -> BranchName {
        match self {
            Self::Branch(branch) => branch.clone(),
            Self::PullRequest(number) => pull_request_branch(*number),
        }
    }
}

/// `pr/N`: the local name a pull request's head is reviewed under.
pub fn pull_request_branch(number: u64) -> BranchName {
    format!("pr/{number}")
        .parse()
        .unwrap_or_else(|_| unreachable!("pr/<digits> is a valid branch name"))
}

/// Where a pull request's commits live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestHead {
    pub branch: BranchName,
    /// Opened from a fork, so `branch` names nothing on origin.
    pub cross_repository: bool,
}

/// The code host, for what git alone can't answer.
pub trait Forge: Send + Sync {
    /// The branch a pull request was opened from, and whether from a fork.
    fn pull_request_head(&self, pr: &PullRequest) -> Result<PullRequestHead, CommandError>;
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
    /// What subagent `id` of the agent working in `workdir` was told and has
    /// done, or `None` when the harness keeps no record of it (yet). Reads
    /// files: call it off the UI thread.
    fn subagent(&self, workdir: &Path, id: &AgentId) -> Result<Option<Log>, HarnessError> {
        let _ = (workdir, id);
        Ok(None)
    }
    /// The agent's current activity, cut at its last prompt, or `None` when the
    /// harness keeps no record of one. `at` is where the hooks last said the
    /// transcript is. Reads files: call it off the UI thread.
    fn activity_detail(
        &self,
        workdir: &Path,
        at: Option<&TranscriptRef>,
    ) -> Result<Option<ActivityDetail>, HarnessError> {
        let _ = (workdir, at);
        Ok(None)
    }
}

/// Whether a log ends with the report `said` is the whole of: the agent's
/// last step was saying it, not an older line followed by calls.
pub fn ends_with_report(log: &[LogEntry], said: &str) -> bool {
    matches!(log.last(), Some(LogEntry::Said { text, .. }) if said.starts_with(text.as_str()))
}

/// Which activity a [`ActivityDetail`] is, as the harness tells activities apart. A new
/// prompt makes a new mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityMark(String);

impl ActivityMark {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// An agent's current activity: its log, and what only the agent itself has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityDetail {
    pub log: Log,
    /// Which activity it is, when the harness could tell.
    pub mark: Option<ActivityMark>,
    pub progress: Progress,
}

/// How far an activity got, with what only that stage has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Still going, and waiting on the user's answer if it asked.
    Running { asked: Option<Asked> },
    /// The harness says it ended, and how long it took.
    Finished { took: Option<Duration> },
    /// The transcript ends because the API gave up on it.
    Stopped,
}

/// An unanswered question call and what it asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub call: ToolId,
    pub questions: Vec<Question>,
}

/// One question an agent put to the user, with the answers it offered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub header: String,
    pub text: String,
    pub options: Vec<Choice>,
    /// Whether several options can be picked.
    pub multi: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    pub description: String,
}

/// What an agent (a subagent, or the main agent's current activity) was told and
/// what it has done since, for a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Log {
    /// What it was told to do, when the record of it is in reach.
    pub prompt: Option<String>,
    /// The most recent entries, oldest first.
    pub log: Vec<LogEntry>,
    /// Where the harness keeps the whole transcript.
    pub transcript: TranscriptRef,
    pub outcome: Outcome,
    /// The model it runs, as the harness names it.
    pub model: Option<String>,
    /// The first record's time.
    pub started: Option<SystemTime>,
    /// The last record's time.
    pub last_heard: Option<SystemTime>,
    /// Whether the log starts after the transcript's start, so counts of
    /// calls are lower bounds.
    pub truncated: bool,
}

/// How far a subagent got, as its transcript records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Running,
    /// Done. The report is what it handed back, or its last message.
    Finished {
        report: Option<String>,
    },
    /// The transcript ends because the API gave up on it.
    Stopped {
        reason: String,
    },
}

/// One step of an agent's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEntry {
    Said {
        text: String,
        at: Option<SystemTime>,
    },
    Called(Box<Call>),
    /// A message the user sent while the agent was working.
    Queued {
        text: String,
        at: Option<SystemTime>,
    },
    /// The harness compacted the conversation here.
    Compacted,
}

/// What a tool call does, as far as a view treats calls differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallKind {
    /// Reads or searches, which repeats a lot and folds.
    Explore,
    /// Changes a file.
    Edit,
    /// Hands work to a subagent, which its result names once it has one.
    Delegate {
        spawned: Option<AgentId>,
    },
    /// Puts a question to the user.
    Ask,
    Other,
}

/// A tool call, from the request to what came back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub id: ToolId,
    pub tool: String,
    pub kind: CallKind,
    pub target: Target,
    pub state: CallState,
    pub started: Option<SystemTime>,
    /// The whole input and result, cut, for expanding.
    pub input: String,
    pub output: String,
}

impl Call {
    /// The subagent a call that hands work off started.
    pub fn spawned(&self) -> Option<&AgentId> {
        match &self.kind {
            CallKind::Delegate { spawned } => spawned.as_ref(),
            CallKind::Explore | CallKind::Edit | CallKind::Ask | CallKind::Other => None,
        }
    }

    /// The file a call that changes one is aimed at, unless it failed.
    pub fn touched(&self) -> Option<&str> {
        match (&self.kind, &self.target, &self.state) {
            (CallKind::Edit, Target::Path { path, .. }, state)
                if !matches!(state, CallState::Failed { .. }) =>
            {
                Some(path)
            }
            _ => None,
        }
    }
}

/// What a tool is acting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    None,
    Command(String),
    Path {
        path: String,
        range: Option<(u64, Option<u64>)>,
    },
    Search {
        pattern: String,
        path: Option<String>,
    },
    Pattern(String),
    Url(String),
    Query(String),
    Task(String),
    Todos {
        done: usize,
        total: usize,
        current: Option<String>,
    },
    Other(String),
}

/// Where a call stands. A running call has no outcome, and only a finished
/// one can have failed or taken any time. `took` is known when both the
/// request and the result carry a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallState {
    Running,
    Done {
        summary: Summary,
        took: Option<Duration>,
    },
    Failed {
        code: Option<i32>,
        excerpt: Excerpt,
        took: Option<Duration>,
    },
    /// Succeeded, but its output carries a failure line.
    Flagged {
        summary: Summary,
        excerpt: Excerpt,
        took: Option<Duration>,
    },
}

impl CallState {
    pub fn took(&self) -> Option<Duration> {
        match self {
            Self::Running => None,
            Self::Done { took, .. } | Self::Failed { took, .. } | Self::Flagged { took, .. } => {
                *took
            }
        }
    }
}

/// What a finished call came to, in a few words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Summary {
    Lines(usize),
    Diff { added: usize, removed: usize },
    NewFile { lines: usize },
    Matches(usize),
    Files(usize),
    Text(String),
    NoOutput,
    Image,
}

/// The lines worth showing under a call, and how many more there were.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Excerpt {
    pub lines: Vec<String>,
    pub more: usize,
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
    /// The user has looked at `name`: a finished activity stops being news.
    fn acknowledge(&self, name: &SessionName) -> Result<(), StoreError>;
    /// Every agent's latest activity.
    fn activities(&self) -> Result<HashMap<SessionName, Activity>, StoreError>;
    /// The agent last selected in the TUI, to start on next time.
    fn last_selected(&self) -> Result<Option<SessionName>, StoreError>;
    fn set_last_selected(&self, name: &SessionName) -> Result<(), StoreError>;
    /// Repos listed in the TUI whether or not anything runs in them, in
    /// the order they were listed.
    fn repos(&self) -> Result<Vec<RepoRoot>, StoreError>;
    /// Lists `repo` last.
    fn list_repo(&self, repo: &RepoRoot) -> Result<Listing, StoreError>;
    fn unlist_repo(&self, repo: &RepoRoot) -> Result<Unlisting, StoreError>;
}

/// What listing a repo did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    Added,
    AlreadyListed,
}

/// What unlisting a repo did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unlisting {
    Dropped,
    NotListed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(dirty: usize, ahead: usize, behind: usize) -> WorktreeFacts {
        let file = |n: usize| Change::untracked(format!("f{n}"));
        WorktreeFacts {
            ahead,
            behind,
            pending: (0..dirty).map(file).collect(),
            ..WorktreeFacts::unchanged(Compared::NoBase {
                checkout: Checkout::Detached("abc1234".into()),
            })
        }
    }

    fn n(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).expect("succeeds")
    }

    #[test]
    fn uncommitted_work_outranks_how_far_ahead_or_behind_it_is() {
        assert_eq!(
            facts(2, 3, 4).readiness(),
            Readiness::Dirty {
                dirty: n(2),
                ahead: 3
            }
        );
        assert_eq!(
            facts(0, 3, 4).readiness(),
            Readiness::Behind {
                behind: n(4),
                ahead: n(3)
            }
        );
        assert_eq!(facts(0, 3, 0).readiness(), Readiness::Ready { ahead: n(3) });
        assert_eq!(
            facts(0, 0, 4).readiness(),
            Readiness::NothingAhead,
            "behind with nothing to ship is nothing to ship"
        );
    }

    fn branch(name: &str) -> BranchName {
        name.parse().expect("parses")
    }

    fn remote(name: &str) -> RemoteRef {
        name.parse().expect("parses")
    }

    #[test]
    fn a_checkout_is_compared_to_the_base_it_tracks_or_to_nothing() {
        let origin_main = || Some(remote("origin/main"));
        assert_eq!(
            Compared::of(Checkout::Branch(branch("main")), origin_main()),
            Compared::Trunk {
                branch: branch("main"),
                base: remote("origin/main")
            }
        );
        assert_eq!(
            Compared::of(Checkout::Branch(branch("fix")), origin_main()),
            Compared::Branch {
                branch: branch("fix"),
                base: remote("origin/main")
            }
        );
        assert_eq!(
            Compared::of(Checkout::Detached("abc1234".into()), origin_main()),
            Compared::Detached {
                sha: "abc1234".into(),
                base: remote("origin/main")
            }
        );
        let unbased = Compared::of(Checkout::Branch(branch("wip")), None);
        assert_eq!((unbased.head(), unbased.base()), ("wip", None));
    }

    #[test]
    fn a_conflict_is_uncommitted_work_that_blocks_readiness() {
        let mut facts = facts(0, 3, 0);
        facts.pending.push(Change::tracked(
            "a.rs",
            Lines::Counted {
                added: 0,
                removed: 0,
            },
            TrackedState::Conflicted,
        ));
        assert_eq!(
            facts.readiness(),
            Readiness::Dirty {
                dirty: n(1),
                ahead: 3
            }
        );
    }

    #[test]
    fn a_remote_ref_needs_a_remote_and_a_valid_branch() {
        assert!("origin/main".parse::<RemoteRef>().is_ok());
        assert!("main".parse::<RemoteRef>().is_err());
        assert!("origin/a..b".parse::<RemoteRef>().is_err());
    }

    #[test]
    fn a_call_has_taken_no_time_until_it_finishes() {
        let took = Some(Duration::from_secs(2));
        assert_eq!(CallState::Running.took(), None);
        assert_eq!(
            CallState::Done {
                summary: Summary::NoOutput,
                took
            }
            .took(),
            took
        );
    }

    #[test]
    fn a_file_only_git_status_lists_has_no_counts_to_add() {
        let listed = Change::listed(&Uncommitted {
            path: "f.rs".into(),
            state: FileState::Conflicted,
        });
        assert_eq!(listed.lines(), Some(Lines::Unknown));
        assert_eq!(
            DiffStat::of(&[listed]),
            DiffStat {
                files: 1,
                added: 0,
                removed: 0
            }
        );
    }
}
