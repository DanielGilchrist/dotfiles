//! In-memory ports for exercising use cases without git, zellij or a disk store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use crate::activity::{Activity, Reported};
use crate::model::{Head, Running, Worktree};
use crate::names::{BranchName, Prompt, SessionName};
use crate::paths::{AgentWorktree, RepoRoot};
use crate::ports::{
    AgentRecord, BranchState, ClientControl, Harness, HarnessError, HarnessId, Harnesses,
    HostError, LaunchCtx, LocalBranch, PaneCommand, PendingPrompt, Progress, RemoteBranch,
    RepoWorktrees, Seen, SessionHost, SessionInfo, SessionStatus, Store, StoreError, Vcs,
    WorktreeStart,
};
use crate::process::{CommandError, LaunchSpec};

/// One repo whose agent worktrees are real directories (so `Layout::scan`
/// finds them) with heads and branch states set by the test.
pub struct FakeVcs {
    pub root: RepoRoot,
    pub heads: Mutex<HashMap<PathBuf, Head>>,
    pub states: Mutex<HashMap<BranchName, BranchState>>,
    pub dirty: Mutex<HashMap<PathBuf, usize>>,
    pub detached_commits: Mutex<HashMap<PathBuf, usize>>,
    pub deleted: Mutex<Vec<BranchName>>,
}

impl FakeVcs {
    pub fn new(root: &Path) -> Self {
        Self {
            root: RepoRoot::new(root.to_path_buf()).unwrap(),
            heads: Mutex::default(),
            states: Mutex::default(),
            dirty: Mutex::default(),
            detached_commits: Mutex::default(),
            deleted: Mutex::default(),
        }
    }

    pub fn set_head(&self, path: &Path, head: Head) {
        self.heads.lock().unwrap().insert(path.to_path_buf(), head);
    }

    pub fn set_state(&self, branch: &BranchName, state: BranchState) {
        self.states.lock().unwrap().insert(branch.clone(), state);
    }
}

impl Vcs for FakeVcs {
    fn repo_of(&self, _: &Path) -> Result<Option<RepoRoot>, CommandError> {
        Ok(Some(self.root.clone()))
    }

    fn worktrees(&self, _: &RepoRoot) -> Result<RepoWorktrees, CommandError> {
        let heads = self.heads.lock().unwrap();
        let linked = heads
            .iter()
            .filter(|(p, _)| p.as_path() != self.root.path())
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
            .unwrap()
            .get(dir)
            .cloned()
            .unwrap_or(Head::Detached))
    }

    fn add_worktree(
        &self,
        _: &RepoRoot,
        at: &AgentWorktree,
        _: &WorktreeStart,
    ) -> Result<(), CommandError> {
        std::fs::create_dir_all(at.path()).unwrap();
        std::fs::write(at.path().join(".git"), "gitdir: fake").unwrap();
        self.set_head(at.path(), Head::Detached);
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
        let deleted = self.deleted.lock().unwrap().contains(branch);
        Ok((!deleted).then(|| LocalBranch::found(branch.clone())))
    }

    fn remote_branch(
        &self,
        _: &RepoRoot,
        _: &BranchName,
    ) -> Result<Option<RemoteBranch>, CommandError> {
        Ok(None)
    }

    fn branch_state(&self, _: &RepoRoot, branch: &BranchName) -> Result<BranchState, CommandError> {
        Ok(self
            .states
            .lock()
            .unwrap()
            .get(branch)
            .copied()
            .unwrap_or(BranchState::Safe))
    }

    fn dirty_files(&self, worktree: &Path) -> Result<usize, CommandError> {
        Ok(self
            .dirty
            .lock()
            .unwrap()
            .get(worktree)
            .copied()
            .unwrap_or(0))
    }

    fn unreachable_commits(&self, worktree: &Path) -> Result<usize, CommandError> {
        Ok(self
            .detached_commits
            .lock()
            .unwrap()
            .get(worktree)
            .copied()
            .unwrap_or(0))
    }

    fn progress(&self, _: &Path) -> Result<Progress, CommandError> {
        Ok(Progress::default())
    }

    fn delete_branch(&self, _: &RepoRoot, branch: LocalBranch) -> Result<(), CommandError> {
        self.deleted.lock().unwrap().push(branch.name().clone());
        Ok(())
    }
}

#[derive(Default)]
pub struct FakeHost {
    pub sessions: Mutex<Vec<SessionInfo>>,
    pub created: Mutex<Vec<(SessionName, PaneCommand)>>,
    pub detached: Mutex<Vec<SessionName>>,
    pub killed: Mutex<Vec<SessionName>>,
    pub deleted: Mutex<Vec<SessionName>>,
    pub written: Mutex<Vec<(SessionName, Vec<u8>)>>,
}

impl SessionHost for FakeHost {
    fn sessions(&self) -> Result<Vec<SessionInfo>, HostError> {
        Ok(self.sessions.lock().unwrap().clone())
    }

    fn attach(&self, name: &SessionName) -> LaunchSpec {
        LaunchSpec::new("attach").arg(name.as_str())
    }

    fn create(&self, name: &SessionName, pane: &PaneCommand) -> Result<LaunchSpec, HostError> {
        self.created
            .lock()
            .unwrap()
            .push((name.clone(), pane.clone()));
        Ok(LaunchSpec::new("create").arg(name.as_str()))
    }

    fn start_detached(&self, name: &SessionName) -> Result<(), HostError> {
        self.detached.lock().unwrap().push(name.clone());
        Ok(())
    }

    fn kill(&self, session: &Running) -> Result<(), HostError> {
        let name = session.name();
        self.killed.lock().unwrap().push(name.clone());
        for s in self
            .sessions
            .lock()
            .unwrap()
            .iter_mut()
            .filter(|s| &s.name == name)
        {
            let cwd = s.cwd().map(Path::to_path_buf);
            s.status = SessionStatus::Exited { cwd };
        }
        Ok(())
    }

    fn delete(&self, name: &SessionName) -> Result<(), HostError> {
        self.deleted.lock().unwrap().push(name.clone());
        self.sessions.lock().unwrap().retain(|s| &s.name != name);
        Ok(())
    }

    fn write(&self, name: &SessionName, bytes: &[u8]) -> Result<(), HostError> {
        self.written
            .lock()
            .unwrap()
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
}

impl Store for MemStore {
    fn pinned(&self) -> Result<Vec<SessionName>, StoreError> {
        Ok(self.pins.lock().unwrap().clone())
    }

    fn pin(&self, name: &SessionName) -> Result<(), StoreError> {
        let mut pins = self.pins.lock().unwrap();
        if !pins.contains(name) {
            pins.push(name.clone());
        }
        Ok(())
    }

    fn unpin(&self, name: &SessionName) -> Result<(), StoreError> {
        self.pins.lock().unwrap().retain(|p| p != name);
        Ok(())
    }

    fn record(&self, name: &SessionName) -> Result<Option<AgentRecord>, StoreError> {
        Ok(self.records.lock().unwrap().get(name).cloned())
    }

    fn put_record(&self, name: &SessionName, record: &AgentRecord) -> Result<(), StoreError> {
        self.records
            .lock()
            .unwrap()
            .insert(name.clone(), record.clone());
        Ok(())
    }

    fn forget(&self, name: &SessionName) -> Result<(), StoreError> {
        self.unpin(name)?;
        self.records.lock().unwrap().remove(name);
        self.activities.lock().unwrap().remove(name);
        Ok(())
    }

    fn report(&self, name: &SessionName, event: Reported) -> Result<(), StoreError> {
        let mut activities = self.activities.lock().unwrap();
        let next = Activity::next(activities.remove(name), event, SystemTime::now());
        activities.insert(name.clone(), next);
        Ok(())
    }

    fn acknowledge(&self, name: &SessionName) -> Result<(), StoreError> {
        let mut activities = self.activities.lock().unwrap();
        if let Some(activity) = activities.remove(name) {
            activities.insert(name.clone(), activity.acknowledged(SystemTime::now()));
        }
        Ok(())
    }

    fn activities(&self) -> Result<HashMap<SessionName, Activity>, StoreError> {
        Ok(self.activities.lock().unwrap().clone())
    }

    fn last_selected(&self) -> Result<Option<SessionName>, StoreError> {
        Ok(self.last_selected.lock().unwrap().clone())
    }

    fn set_last_selected(&self, name: &SessionName) -> Result<(), StoreError> {
        *self.last_selected.lock().unwrap() = Some(name.clone());
        Ok(())
    }
}

/// Launches `fake [bootstrap]`; submits a prompt followed by a carriage
/// return; reports activity when told to.
pub struct FakeHarness {
    id: HarnessId,
    pub active: AtomicBool,
}

impl Default for FakeHarness {
    fn default() -> Self {
        Self {
            id: "fake".parse().unwrap(),
            active: AtomicBool::new(false),
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
