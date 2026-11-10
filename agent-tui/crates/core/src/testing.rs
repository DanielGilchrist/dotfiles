//! In-memory ports for exercising use cases without git, zellij or a disk store.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::activity::{Activity, Going, Input, MainEvent, Reported, Stopped, TranscriptRef};
use crate::model::{Head, HostedWorktree, Running, Startable, Worktree};
use crate::names::{BranchName, HostName, NameLimit, Prompt, PullRequest, SessionName};
use crate::ops::Services;
use crate::paths::{AgentWorktree, Layout, RepoRoot};
use crate::ports::{
    AgentRecord, BranchLog, BranchState, Change, Checkout, ClientControl, Compared, DiffBase,
    Fetch, FileDiff, Forge, Harness, HarnessError, HarnessId, Harnesses, HostError, LaunchCtx,
    Listing, LocalBranch, Lookup, Observation, PaneCommand, PendingPrompt, Probe, PullRequestHead,
    RemoteBranch, RepoWorktrees, Seen, SessionHost, SessionInfo, SessionStatus, Stop, Store,
    StoreError, Survivor, SurvivorState, TranscriptStamp, Unlisting, Vcs, WorktreeFacts,
    WorktreeStart,
};
use crate::process::{CommandError, LaunchSpec};

/// What the fakes did to the world, in order, so a test can assert that one
/// port was used before another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Kill(SessionName),
    Stop(PathBuf),
    Report(SessionName),
    Create(SessionName),
    Detach(SessionName),
}

/// Shared by the fakes of one test.
#[derive(Debug, Clone, Default)]
pub struct Journal(Arc<Mutex<Vec<Step>>>);

impl Journal {
    pub fn push(&self, step: Step) {
        self.0.lock().expect("lock").push(step);
    }

    pub fn steps(&self) -> Vec<Step> {
        self.0.lock().expect("lock").clone()
    }
}

/// One repo whose agent worktrees are real directories (so `Layout::scan`
/// finds them) with heads and branch states set by the test.
pub struct FakeVcs {
    pub root: RepoRoot,
    pub heads: Mutex<HashMap<PathBuf, Head>>,
    pub states: Mutex<HashMap<BranchName, BranchState>>,
    pub dirty: Mutex<HashMap<PathBuf, usize>>,
    pub detached_commits: Mutex<HashMap<PathBuf, usize>>,
    pub deleted: Mutex<Vec<BranchName>>,
    /// Paths that aren't inside any repo.
    pub not_repos: Mutex<Vec<PathBuf>>,
    /// Paths inside some other repo than `root`.
    pub other_repos: Mutex<HashMap<PathBuf, RepoRoot>>,
    /// Branches on origin that a fetch would bring in.
    pub on_origin: Mutex<Vec<BranchName>>,
    pub fetched: Mutex<Vec<BranchName>>,
    /// Pull request numbers origin has a `refs/pull/N/head` for.
    pub pull_requests: Mutex<Vec<u64>>,
    /// Every fetch fails, as with no network.
    pub offline: Mutex<bool>,
}

impl FakeVcs {
    pub fn new(root: &Path) -> Self {
        Self {
            root: RepoRoot::new(root.to_path_buf()).expect("absolute"),
            heads: Mutex::default(),
            states: Mutex::default(),
            dirty: Mutex::default(),
            detached_commits: Mutex::default(),
            deleted: Mutex::default(),
            not_repos: Mutex::default(),
            other_repos: Mutex::default(),
            on_origin: Mutex::default(),
            fetched: Mutex::default(),
            pull_requests: Mutex::default(),
            offline: Mutex::default(),
        }
    }

    pub fn set_head(&self, path: &Path, head: Head) {
        self.heads
            .lock()
            .expect("lock")
            .insert(path.to_path_buf(), head);
    }

    pub fn set_state(&self, branch: &BranchName, state: BranchState) {
        self.states
            .lock()
            .expect("lock")
            .insert(branch.clone(), state);
    }
}

impl Vcs for FakeVcs {
    fn repo_of(&self, dir: &Path) -> Result<Option<RepoRoot>, CommandError> {
        if self
            .not_repos
            .lock()
            .expect("lock")
            .iter()
            .any(|p| p == dir)
        {
            return Ok(None);
        }
        if let Some(other) = self.other_repos.lock().expect("lock").get(dir) {
            return Ok(Some(other.clone()));
        }
        Ok(Some(self.root.clone()))
    }

    fn worktrees(&self, repo: &RepoRoot) -> Result<RepoWorktrees, CommandError> {
        let heads = self.heads.lock().expect("lock");
        let others = self.other_repos.lock().expect("lock");
        let linked = heads
            .iter()
            .filter(|(p, _)| p.as_path() != self.root.path())
            .filter(|(p, _)| others.get(p.as_path()).unwrap_or(&self.root) == repo)
            .map(|(path, head)| Worktree {
                path: path.clone(),
                head: head.clone(),
            })
            .collect();
        Ok(RepoWorktrees {
            main: Worktree {
                path: self.root.path().into(),
                head: Head::Detached,
            },
            linked,
        })
    }

    fn head(&self, dir: &Path) -> Result<Head, CommandError> {
        Ok(self
            .heads
            .lock()
            .expect("succeeds")
            .get(dir)
            .cloned()
            .unwrap_or(Head::Detached))
    }

    fn add_worktree(
        &self,
        _: &RepoRoot,
        at: &AgentWorktree,
        start: &WorktreeStart,
    ) -> Result<(), CommandError> {
        std::fs::create_dir_all(at.path()).expect("creates");
        let admin = self
            .root
            .path()
            .join(".git/worktrees")
            .join(at.name().as_str());
        std::fs::create_dir_all(&admin).expect("creates");
        std::fs::write(
            at.path().join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .expect("writes");
        let head = match start {
            WorktreeStart::DefaultBase => Head::Detached,
            WorktreeStart::Branch(local) => Head::Branch(local.name().clone()),
            WorktreeStart::TrackRemote(remote) | WorktreeStart::AtRemote(remote) => {
                Head::Branch(remote.name().clone())
            }
        };
        self.set_head(at.path(), head);
        Ok(())
    }

    fn prune_worktrees(&self, _: &RepoRoot) -> Result<(), CommandError> {
        Ok(())
    }

    fn local_branch(
        &self,
        _: &RepoRoot,
        branch: &BranchName,
    ) -> Result<Option<LocalBranch>, CommandError> {
        let deleted = self.deleted.lock().expect("lock").contains(branch);
        Ok((!deleted).then(|| LocalBranch::found(branch.clone())))
    }

    fn remote_branch(
        &self,
        _: &RepoRoot,
        branch: &BranchName,
    ) -> Result<Option<RemoteBranch>, CommandError> {
        let fetched = self.fetched.lock().expect("lock").contains(branch);
        Ok(fetched.then(|| RemoteBranch::found(branch.clone())))
    }

    fn fetch(&self, _: &RepoRoot, what: &Fetch) -> Result<(), CommandError> {
        if *self.offline.lock().expect("lock") {
            return Err(CommandError::Failed {
                command: "git fetch".into(),
                status: "exit status: 128".into(),
                stderr: "fatal: unable to access origin: Could not resolve host".into(),
            });
        }
        let known = match what {
            Fetch::Branch(branch) => self.on_origin.lock().expect("lock").contains(branch),
            Fetch::PullRequest(number) => self.pull_requests.lock().expect("lock").contains(number),
        };
        if known {
            self.fetched.lock().expect("lock").push(what.branch());
        }
        Ok(())
    }

    fn branch_state(&self, _: &RepoRoot, branch: &BranchName) -> Result<BranchState, CommandError> {
        Ok(self
            .states
            .lock()
            .expect("succeeds")
            .get(branch)
            .copied()
            .unwrap_or(BranchState::Safe))
    }

    fn dirty_files(&self, worktree: &Path) -> Result<usize, CommandError> {
        Ok(self
            .dirty
            .lock()
            .expect("succeeds")
            .get(worktree)
            .copied()
            .unwrap_or(0))
    }

    fn unreachable_commits(&self, worktree: &Path) -> Result<usize, CommandError> {
        Ok(self
            .detached_commits
            .lock()
            .expect("succeeds")
            .get(worktree)
            .copied()
            .unwrap_or(0))
    }

    fn facts(&self, _: &Path) -> Result<WorktreeFacts, CommandError> {
        Ok(WorktreeFacts::unchanged(Compared::NoBase {
            checkout: Checkout::Detached("0000000".into()),
        }))
    }

    fn branch_log(&self, _: &Path) -> Result<BranchLog, CommandError> {
        Ok(BranchLog::default())
    }

    fn file_diff(&self, _: &Path, _: &Change, _: DiffBase) -> Result<FileDiff, CommandError> {
        Ok(FileDiff::Text {
            lines: Vec::new(),
            more: 0,
        })
    }

    fn delete_branch(&self, _: &RepoRoot, branch: LocalBranch) -> Result<(), CommandError> {
        self.deleted
            .lock()
            .expect("lock")
            .push(branch.name().clone());
        Ok(())
    }
}

#[derive(Default)]
/// Pull requests and the heads behind them.
pub struct FakeForge {
    pub heads: Mutex<HashMap<PullRequest, PullRequestHead>>,
}

impl Forge for FakeForge {
    fn pull_request_head(&self, pr: &PullRequest) -> Result<PullRequestHead, CommandError> {
        self.heads
            .lock()
            .expect("lock")
            .get(pr)
            .cloned()
            .ok_or_else(|| CommandError::Failed {
                command: "gh pr view".into(),
                status: "exit status: 1".into(),
                stderr: format!("no pull request {pr}"),
            })
    }
}

#[derive(Default)]
pub struct FakeHost {
    pub sessions: Mutex<Vec<SessionInfo>>,
    /// The panes sessions were created with. The journal says when.
    pub created: Mutex<Vec<(SessionName, PaneCommand)>>,
    pub deleted: Mutex<Vec<SessionName>>,
    pub written: Mutex<Vec<(SessionName, Vec<u8>)>>,
    /// Creating a session fails, as when the host can't be reached.
    pub create_fails: Mutex<bool>,
    /// Killing a session fails.
    pub kill_fails: Mutex<bool>,
    /// Starting a session detached fails.
    pub start_fails: Mutex<bool>,
    /// How many listings still show a killed session running.
    pub kill_lag: Mutex<usize>,
    /// How many listings go by before a session started detached shows.
    pub start_lag: Mutex<usize>,
    lagging: Mutex<Vec<Lag>>,
    pub journal: Journal,
}

/// A session the host lists as it was for a few more listings.
struct Lag {
    name: SessionName,
    listings: usize,
    shown: Option<SessionStatus>,
}

impl FakeHost {
    pub fn with(journal: Journal) -> Self {
        Self {
            journal,
            ..Self::default()
        }
    }

    /// Sessions killed, in order.
    pub fn killed(&self) -> Vec<SessionName> {
        self.named(|step| match step {
            Step::Kill(name) => Some(name),
            _ => None,
        })
    }

    /// Sessions started detached, in order.
    pub fn detached(&self) -> Vec<SessionName> {
        self.named(|step| match step {
            Step::Detach(name) => Some(name),
            _ => None,
        })
    }

    fn named(&self, pick: impl Fn(Step) -> Option<SessionName>) -> Vec<SessionName> {
        self.journal.steps().into_iter().filter_map(pick).collect()
    }
}

pub const NAME_LIMIT: NameLimit = NameLimit::new(NonZeroUsize::new(20).expect("nonzero"));

impl SessionHost for FakeHost {
    fn name_limit(&self) -> NameLimit {
        NAME_LIMIT
    }

    fn sessions(&self) -> Result<Vec<SessionInfo>, HostError> {
        let mut listed = self.sessions.lock().expect("lock").clone();
        let mut lagging = self.lagging.lock().expect("lock");
        for lag in lagging.iter_mut().filter(|lag| lag.listings > 0) {
            lag.listings -= 1;
            listed.retain(|s| s.name != lag.name);
            if let Some(status) = lag.shown.clone() {
                listed.push(SessionInfo {
                    name: lag.name.clone(),
                    status,
                });
            }
        }
        lagging.retain(|lag| lag.listings > 0);
        Ok(listed)
    }

    fn attach(&self, name: &SessionName) -> LaunchSpec {
        LaunchSpec::new("attach").arg(name.as_str())
    }

    fn observe(&self, name: &SessionName) -> Result<Observation, HostError> {
        Ok(Observation {
            spec: LaunchSpec::new("observe").arg(name.as_str()),
            cols: std::num::NonZeroU16::new(80).expect("nonzero"),
            rows: std::num::NonZeroU16::new(24).expect("nonzero"),
        })
    }

    fn create(&self, name: &HostName, pane: &PaneCommand) -> Result<LaunchSpec, HostError> {
        name.within(NAME_LIMIT)?;
        if *self.create_fails.lock().expect("lock") {
            return Err(HostError::Io {
                context: "creating a session".into(),
                source: std::io::Error::other("no host"),
            });
        }
        let name = name.session();
        self.journal.push(Step::Create(name.clone()));
        self.created
            .lock()
            .expect("succeeds")
            .push((name.clone(), pane.clone()));
        Ok(LaunchSpec::new("create").arg(name.as_str()))
    }

    fn start_detached(&self, name: &SessionName) -> Result<(), HostError> {
        self.journal.push(Step::Detach(name.clone()));
        if *self.start_fails.lock().expect("lock") {
            return Err(HostError::Io {
                context: format!("starting {name} detached"),
                source: std::io::Error::other("no host"),
            });
        }
        let mut sessions = self.sessions.lock().expect("lock");
        sessions.retain(|s| &s.name != name);
        sessions.push(SessionInfo {
            name: name.clone(),
            status: SessionStatus::Live { cwd: None },
        });
        let listings = std::mem::take(&mut *self.start_lag.lock().expect("lock"));
        if listings > 0 {
            self.lagging.lock().expect("lock").push(Lag {
                name: name.clone(),
                listings,
                shown: None,
            });
        }
        Ok(())
    }

    fn kill(&self, session: &Running) -> Result<(), HostError> {
        let name = session.name();
        self.journal.push(Step::Kill(name.clone()));
        if *self.kill_fails.lock().expect("lock") {
            return Err(HostError::Io {
                context: format!("killing {name}"),
                source: std::io::Error::other("no host"),
            });
        }
        let listings = std::mem::take(&mut *self.kill_lag.lock().expect("lock"));
        if listings > 0 {
            let cwd = self
                .sessions
                .lock()
                .expect("lock")
                .iter()
                .find(|s| &s.name == name)
                .and_then(|s| s.cwd().map(Path::to_path_buf));
            self.lagging.lock().expect("lock").push(Lag {
                name: name.clone(),
                listings,
                shown: Some(SessionStatus::Live { cwd }),
            });
        }
        for s in self
            .sessions
            .lock()
            .expect("succeeds")
            .iter_mut()
            .filter(|s| &s.name == name)
        {
            let cwd = s.cwd().map(Path::to_path_buf);
            s.status = SessionStatus::Exited { cwd };
        }
        Ok(())
    }

    fn delete(&self, name: &SessionName) -> Result<(), HostError> {
        self.deleted.lock().expect("lock").push(name.clone());
        self.sessions
            .lock()
            .expect("lock")
            .retain(|s| &s.name != name);
        Ok(())
    }

    fn write(&self, name: &SessionName, bytes: &[u8]) -> Result<(), HostError> {
        self.written
            .lock()
            .expect("succeeds")
            .push((name.clone(), bytes.to_vec()));
        Ok(())
    }

    fn screen(&self, _: &SessionName) -> Result<String, HostError> {
        Ok(String::new())
    }

    fn clients(&self) -> Option<&dyn ClientControl> {
        None
    }

    fn nesting_env(&self) -> &'static [&'static str] {
        &[]
    }
}

#[derive(Default)]
pub struct MemStore {
    pub pins: Mutex<Vec<SessionName>>,
    pub records: Mutex<HashMap<SessionName, AgentRecord>>,
    pub last_selected: Mutex<Option<SessionName>>,
    pub activities: Mutex<HashMap<SessionName, Activity>>,
    pub repos: Mutex<Vec<RepoRoot>>,
    /// Applying a report fails, as when the state directory can't be written.
    pub report_fails: Mutex<bool>,
    pub notes: Mutex<Vec<String>>,
    pub journal: Journal,
}

impl MemStore {
    pub fn with(journal: Journal) -> Self {
        Self {
            journal,
            ..Self::default()
        }
    }
}

impl Store for MemStore {
    fn note(&self, message: &str) {
        self.notes.lock().expect("lock").push(message.to_string());
    }

    fn pinned(&self) -> Result<Vec<SessionName>, StoreError> {
        Ok(self.pins.lock().expect("lock").clone())
    }

    fn pin(&self, name: &SessionName) -> Result<(), StoreError> {
        let mut pins = self.pins.lock().expect("lock");
        if !pins.contains(name) {
            pins.push(name.clone());
        }
        Ok(())
    }

    fn unpin(&self, name: &SessionName) -> Result<(), StoreError> {
        self.pins.lock().expect("lock").retain(|p| p != name);
        Ok(())
    }

    fn record(&self, name: &SessionName) -> Result<Option<AgentRecord>, StoreError> {
        Ok(self.records.lock().expect("lock").get(name).cloned())
    }

    fn put_record(&self, name: &SessionName, record: &AgentRecord) -> Result<(), StoreError> {
        self.records
            .lock()
            .expect("succeeds")
            .insert(name.clone(), record.clone());
        Ok(())
    }

    fn forget(&self, name: &SessionName) -> Result<(), StoreError> {
        self.unpin(name)?;
        self.records.lock().expect("lock").remove(name);
        self.activities.lock().expect("lock").remove(name);
        Ok(())
    }

    fn report(&self, name: &SessionName, event: Reported) -> Result<(), StoreError> {
        self.journal.push(Step::Report(name.clone()));
        if *self.report_fails.lock().expect("lock") {
            return Err(StoreError::Io {
                path: std::path::PathBuf::from("/state"),
                source: std::io::Error::other("read-only"),
            });
        }
        let mut activities = self.activities.lock().expect("lock");
        let next = Activity::next(activities.remove(name), event, SystemTime::now());
        activities.insert(name.clone(), next);
        Ok(())
    }

    fn acknowledge(&self, name: &SessionName) -> Result<(), StoreError> {
        let mut activities = self.activities.lock().expect("lock");
        if let Some(activity) = activities.remove(name) {
            activities.insert(name.clone(), activity.acknowledged(SystemTime::now()));
        }
        Ok(())
    }

    fn interrupt(
        &self,
        name: &SessionName,
        proof: Going,
        at: SystemTime,
    ) -> Result<(), StoreError> {
        if let Some(activity) = self.activities.lock().expect("lock").get_mut(name) {
            *activity = activity.clone().interrupted(proof, at);
        }
        Ok(())
    }

    fn resume(&self, name: &SessionName, proof: Stopped) -> Result<(), StoreError> {
        if let Some(activity) = self.activities.lock().expect("lock").get_mut(name) {
            *activity = activity.clone().resumed(proof, SystemTime::now());
        }
        Ok(())
    }

    fn activities(&self) -> Result<HashMap<SessionName, Activity>, StoreError> {
        Ok(self.activities.lock().expect("lock").clone())
    }

    fn repos(&self) -> Result<Vec<RepoRoot>, StoreError> {
        Ok(self.repos.lock().expect("lock").clone())
    }

    fn list_repo(&self, repo: &RepoRoot) -> Result<Listing, StoreError> {
        let mut repos = self.repos.lock().expect("lock");
        if repos.contains(repo) {
            return Ok(Listing::AlreadyListed);
        }
        repos.push(repo.clone());
        Ok(Listing::Added)
    }

    fn unlist_repo(&self, repo: &RepoRoot) -> Result<Unlisting, StoreError> {
        let mut repos = self.repos.lock().expect("lock");
        let before = repos.len();
        repos.retain(|r| r != repo);
        Ok(if repos.len() < before {
            Unlisting::Dropped
        } else {
            Unlisting::NotListed
        })
    }

    fn last_selected(&self) -> Result<Option<SessionName>, StoreError> {
        Ok(self.last_selected.lock().expect("lock").clone())
    }

    fn set_last_selected(&self, name: &SessionName) -> Result<(), StoreError> {
        *self.last_selected.lock().expect("lock") = Some(name.clone());
        Ok(())
    }
}

/// What stopping a fake's survivor does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopping {
    Works,
    /// It reports an error and the process keeps running.
    Fails,
    /// It reports success and the process keeps running.
    Ignored,
}

/// A process a fake harness left running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lurking {
    /// Stoppable at once.
    Survivor(SurvivorState),
    /// Alive but not stoppable for this many more lookups.
    Appears { after: usize, state: SurvivorState },
    /// Alive and never stoppable, like a process still in its terminal.
    Interactive(SurvivorState),
    /// Alive in a terminal in another directory.
    Elsewhere,
}

type Lurkers = Arc<Mutex<HashMap<PathBuf, Lurking>>>;

/// Launches `fake [bootstrap]`. Submits a prompt followed by a carriage
/// return. Reports activity when told to.
pub struct FakeHarness {
    id: HarnessId,
    pub active: AtomicBool,
    /// What every probe of a transcript finds.
    pub probe: Mutex<Probe>,
    /// Processes left running after their session was killed, by workdir.
    pub survivors: Lurkers,
    pub stop: Arc<Mutex<Stopping>>,
    /// Looking for a survivor fails, as when its registry can't be read.
    pub lookup_fails: AtomicBool,
    pub journal: Journal,
}

impl Default for FakeHarness {
    fn default() -> Self {
        Self::with(Journal::default())
    }
}

impl FakeHarness {
    pub fn with(journal: Journal) -> Self {
        Self {
            id: "fake".parse().expect("parses"),
            active: AtomicBool::new(false),
            probe: Mutex::new(Probe::NoTranscript),
            survivors: Lurkers::default(),
            stop: Arc::new(Mutex::new(Stopping::Works)),
            lookup_fails: AtomicBool::new(false),
            journal,
        }
    }

    pub fn lurk(&self, workdir: &Path, lurking: Lurking) {
        self.survivors
            .lock()
            .expect("lock")
            .insert(workdir.to_path_buf(), lurking);
    }
}

struct FakeProcess {
    workdir: PathBuf,
    survivors: Lurkers,
    stop: Arc<Mutex<Stopping>>,
    journal: Journal,
}

impl Stop for FakeProcess {
    fn stop(self: Box<Self>) -> Result<(), HarnessError> {
        self.journal.push(Step::Stop(self.workdir.clone()));
        let stopping = *self.stop.lock().expect("lock");
        match stopping {
            Stopping::Works => {
                self.survivors.lock().expect("lock").remove(&self.workdir);
                Ok(())
            }
            Stopping::Fails => Err(HarnessError {
                context: "stopping the fake".into(),
                source: std::io::Error::other("still running"),
            }),
            Stopping::Ignored => Ok(()),
        }
    }
}

impl Harness for FakeHarness {
    fn id(&self) -> &HarnessId {
        &self.id
    }

    fn acknowledged(&self, _: &Path, _: &PendingPrompt) -> Result<Option<Seen>, HarnessError> {
        Ok(self
            .active
            .load(Ordering::SeqCst)
            .then(|| Seen::at(SystemTime::now())))
    }

    fn label(&self) -> &'static str {
        "fake"
    }

    fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchSpec, HarnessError> {
        Ok(LaunchSpec::new("fake").args(ctx.bootstrap.map(Prompt::as_str)))
    }

    fn submit(&self, prompt: &Prompt) -> Vec<u8> {
        format!("{prompt}\r").into_bytes()
    }

    fn survivor(&self, workdir: &Path, _: Option<&TranscriptRef>) -> Result<Lookup, HarnessError> {
        if self.lookup_fails.load(Ordering::SeqCst) {
            return Err(HarnessError {
                context: "reading the registry".into(),
                source: std::io::Error::other("unreadable"),
            });
        }
        let mut lurkers = self.survivors.lock().expect("lock");
        let process = || {
            Box::new(FakeProcess {
                workdir: workdir.to_path_buf(),
                survivors: Arc::clone(&self.survivors),
                stop: Arc::clone(&self.stop),
                journal: self.journal.clone(),
            })
        };
        Ok(match lurkers.get_mut(workdir) {
            None => Lookup::Gone,
            Some(Lurking::Survivor(state)) => Lookup::Survivor(Survivor::new(*state, process())),
            Some(Lurking::Appears { after, state }) if *after == 0 => {
                Lookup::Survivor(Survivor::new(*state, process()))
            }
            Some(Lurking::Appears { after, state }) => {
                *after -= 1;
                Lookup::NotYet(*state)
            }
            Some(Lurking::Interactive(state)) => Lookup::NotYet(*state),
            Some(Lurking::Elsewhere) => Lookup::Elsewhere,
        })
    }

    fn interruption(
        &self,
        _: &TranscriptRef,
        _: SystemTime,
        _: Option<&TranscriptStamp>,
    ) -> Result<Probe, HarnessError> {
        Ok(self.probe.lock().expect("lock").clone())
    }
}

#[derive(Default)]
pub struct FakeHarnesses(pub FakeHarness);

impl Harnesses for FakeHarnesses {
    fn get(&self, id: &HarnessId) -> Option<&dyn Harness> {
        (id == self.0.id()).then_some(&self.0 as &dyn Harness)
    }

    fn default_harness(&self) -> &dyn Harness {
        &self.0
    }

    fn all(&self) -> Vec<&dyn Harness> {
        vec![&self.0]
    }
}

/// What an agent in a [`World`] is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doing {
    Idle,
    Working,
}

/// Every in-memory port, wired to one [`Journal`], with a temp directory for
/// the agent worktrees.
pub struct World {
    _dir: tempfile::TempDir,
    pub vcs: FakeVcs,
    pub forge: FakeForge,
    pub host: FakeHost,
    pub store: MemStore,
    pub harnesses: FakeHarnesses,
    pub layout: Layout,
    pub exe: PathBuf,
    pub own: Option<SessionName>,
}

impl World {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = dir.path().join("src/api");
        std::fs::create_dir_all(&root).expect("creates");
        let journal = Journal::default();
        Self {
            host: FakeHost::with(journal.clone()),
            store: MemStore::with(journal.clone()),
            harnesses: FakeHarnesses(FakeHarness::with(journal)),
            vcs: FakeVcs::new(&root),
            forge: FakeForge::default(),
            layout: Layout::new(dir.path().join("w")).expect("absolute"),
            exe: "/bin/agent".into(),
            own: None,
            _dir: dir,
        }
    }

    pub fn svc(&self) -> Services<'_> {
        Services {
            vcs: &self.vcs,
            forge: &self.forge,
            host: &self.host,
            store: &self.store,
            harnesses: &self.harnesses,
            layout: &self.layout,
            exe: &self.exe,
            own: self.own.as_ref(),
        }
    }

    pub fn harness(&self) -> &dyn Harness {
        &self.harnesses.0
    }

    pub fn journal(&self) -> &Journal {
        &self.host.journal
    }

    pub fn add_session(&self, name: &str, status: SessionStatus) {
        self.host.sessions.lock().expect("lock").push(SessionInfo {
            name: name.parse().expect("parses"),
            status,
        });
    }

    pub fn existing_worktree(&self, name: &str) -> AgentWorktree {
        let wt = self
            .layout
            .worktree(&self.vcs.root, name.parse().expect("parses"));
        self.vcs
            .add_worktree(&self.vcs.root, &wt, &WorktreeStart::DefaultBase)
            .expect("succeeds");
        wt
    }

    /// An agent with a record, a live session and an activity.
    pub fn agent(&self, name: &str, doing: Doing) -> Startable {
        let wt = self.existing_worktree(name);
        let session: SessionName = name.parse().expect("parses");
        self.store
            .put_record(
                &session,
                &AgentRecord::new(self.harness().id().clone(), None),
            )
            .expect("stores");
        self.add_session(name, SessionStatus::Live { cwd: None });
        let event = match doing {
            Doing::Idle => MainEvent::Finished {
                summary: None,
                running: Vec::new(),
            },
            Doing::Working => MainEvent::Prompted(Input::User("go".into())),
        };
        self.store.activities.lock().expect("lock").insert(
            session,
            Activity::next(None, Reported::main(event), SystemTime::UNIX_EPOCH),
        );
        Startable::worktree(HostedWorktree::admit(NAME_LIMIT, wt).expect("admitted"))
    }

    /// An agent whose session exited.
    pub fn exited(&self, name: &str) -> Startable {
        let at = self.agent(name, Doing::Idle);
        let session: SessionName = name.parse().expect("parses");
        self.host
            .sessions
            .lock()
            .expect("lock")
            .retain(|s| s.name != session);
        self.add_session(name, SessionStatus::Exited { cwd: None });
        at
    }

    /// An agent whose session exited, leaving a harness process running.
    pub fn ghost(&self, name: &str, lurking: Lurking) -> Startable {
        let at = self.exited(name);
        self.harnesses.0.lurk(at.cwd(), lurking);
        at
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}
