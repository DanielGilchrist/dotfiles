use std::num::NonZeroUsize;
use std::sync::atomic::Ordering;

use super::*;
use crate::model::Head;
use crate::ports::{SessionInfo, SessionStatus};
use crate::testing::{FakeHarnesses, FakeHost, FakeVcs, MemStore};

struct World {
    _dir: tempfile::TempDir,
    vcs: FakeVcs,
    host: FakeHost,
    store: MemStore,
    harnesses: FakeHarnesses,
    layout: Layout,
    exe: PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src/api");
        std::fs::create_dir_all(&root).unwrap();
        Self {
            vcs: FakeVcs::new(&root),
            layout: Layout::new(dir.path().join("w")).unwrap(),
            host: FakeHost::default(),
            store: MemStore::default(),
            harnesses: FakeHarnesses::default(),
            exe: "/bin/agent".into(),
            _dir: dir,
        }
    }

    fn svc(&self) -> Services<'_> {
        Services {
            vcs: &self.vcs,
            host: &self.host,
            store: &self.store,
            harnesses: &self.harnesses,
            layout: &self.layout,
            exe: &self.exe,
        }
    }

    fn harness(&self) -> &dyn Harness {
        &self.harnesses.0
    }

    fn add_session(&self, name: &str, status: SessionStatus) {
        self.host.sessions.lock().unwrap().push(SessionInfo { name: name.parse().unwrap(), status });
    }

    fn existing_worktree(&self, name: &str) -> AgentWorktree {
        let wt = self.layout.worktree(&self.vcs.root, name.parse().unwrap());
        self.vcs.add_worktree(&self.vcs.root, &wt, &WorktreeStart::DefaultBase).unwrap();
        wt
    }
}

fn agent(s: &str) -> AgentName {
    s.parse().unwrap()
}

fn session(s: &str) -> SessionName {
    s.parse().unwrap()
}

fn prompt(s: &str) -> Prompt {
    s.parse().unwrap()
}

fn branch(s: &str) -> BranchName {
    s.parse().unwrap()
}

#[test]
fn new_agents_get_a_worktree_a_record_and_a_queued_bootstrap() {
    let w = World::new();
    let repo = w.vcs.root.clone();
    let launch = attach(w.svc(), &agent("fix"), Some(&prompt("fix ci")), Some(&repo), w.harness()).unwrap();
    assert_eq!(launch.delivery, Delivery::Queued);
    assert!(w.layout.worktree(&repo, agent("fix")).path().join(".git").exists());
    let record = w.store.record(&session("fix")).unwrap().unwrap();
    assert!(record.pending.unwrap().prompt.as_str().ends_with("fix ci"));
    let created = w.host.created.lock().unwrap();
    assert_eq!(created[0].1.args, ["exec", "fix"]);
}

#[test]
fn queued_prompts_are_resent_until_the_agent_shows_activity() {
    let w = World::new();
    let repo = w.vcs.root.clone();
    attach(w.svc(), &agent("fix"), Some(&prompt("go")), Some(&repo), w.harness()).unwrap();
    let crashed = exec_spec(w.svc(), &session("fix"), Path::new("/x")).unwrap();
    let retried = exec_spec(w.svc(), &session("fix"), Path::new("/x")).unwrap();
    assert_eq!(crashed.arguments().len(), 1);
    assert_eq!(retried.arguments().len(), 1, "no activity yet, so the prompt is sent again");

    w.harnesses.0.active.store(true, Ordering::SeqCst);
    let resurrected = exec_spec(w.svc(), &session("fix"), Path::new("/x")).unwrap();
    assert_eq!(resurrected.arguments(), &[] as &[std::ffi::OsString]);
    assert!(w.store.record(&session("fix")).unwrap().unwrap().pending.is_none());
}

#[test]
fn restarts_keep_the_agents_recorded_harness() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    let record = AgentRecord { harness: "codex".parse().unwrap(), pending: None };
    w.store.put_record(&session("fix"), &record).unwrap();
    let result = start(w.svc(), &Startable::worktree(&wt), w.harness(), None);
    assert!(matches!(result, Err(Error::UnknownHarness(id)) if id.to_string() == "codex"));
}

#[test]
fn prompts_for_exited_sessions_are_refused_before_anything_happens() {
    let w = World::new();
    w.existing_worktree("fix");
    w.add_session("fix", SessionStatus::Exited);
    let result = attach(w.svc(), &agent("fix"), Some(&prompt("hi")), None, w.harness());
    assert!(matches!(result, Err(Error::NotRunning(_))));
    assert!(w.host.written.lock().unwrap().is_empty());
}

#[test]
fn prompts_for_live_sessions_are_typed_in_with_the_harness_encoding() {
    let w = World::new();
    w.existing_worktree("fix");
    w.add_session("fix", SessionStatus::Live { cwd: None });
    let launch = attach(w.svc(), &agent("fix"), Some(&prompt("hi")), None, w.harness()).unwrap();
    assert_eq!(launch.delivery, Delivery::Injected);
    assert_eq!(w.host.written.lock().unwrap()[0].1, b"hi\r");
}

#[test]
fn exited_repo_sessions_resurrect_instead_of_creating_worktrees() {
    let w = World::new();
    w.add_session("api", SessionStatus::Exited);
    let repo = w.vcs.root.clone();
    let launch = attach(w.svc(), &agent("api"), None, Some(&repo), w.harness()).unwrap();
    assert_eq!(launch.spec.program(), "attach");
    assert_eq!(w.layout.scan().unwrap(), []);
}

#[test]
fn vanished_worktrees_are_only_recreated_from_their_own_repo() {
    let w = World::new();
    w.add_session("fix", SessionStatus::Live { cwd: Some(w.layout.root().join("web/fix")) });
    let repo = w.vcs.root.clone();
    let result = attach(w.svc(), &agent("fix"), None, Some(&repo), w.harness());
    assert!(matches!(result, Err(Error::WorktreeGone { .. })));
}

#[test]
fn sessions_sitting_in_another_agents_worktree_just_attach() {
    let w = World::new();
    w.existing_worktree("other");
    w.add_session("fix", SessionStatus::Live { cwd: Some(w.layout.root().join("api/other/src")) });
    let launch = attach(w.svc(), &agent("fix"), None, None, w.harness()).unwrap();
    assert_eq!(launch.spec.program(), "attach");
}

#[test]
fn removal_refuses_unmerged_work_unless_forced() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    w.vcs.set_head(wt.path(), Head::Branch(branch("fix")));
    w.vcs.set_state(&branch("fix"), BranchState::Unmerged { ahead: NonZeroUsize::new(2).unwrap() });
    let plan = plan_removal(w.svc(), removable(w.svc(), &session("fix")).unwrap(), None).unwrap();
    let plan = plan.approve().unwrap_err();
    assert!(wt.path().exists());
    let removed = remove(w.svc(), plan.force(), None).unwrap();
    assert!(matches!(removed, Removed::Agent { ref warnings, .. } if warnings.is_empty()));
    assert!(!wt.path().exists());
    assert_eq!(*w.vcs.deleted.lock().unwrap(), [branch("fix")]);
}

#[test]
fn a_refused_removal_leaves_the_session_running() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    w.vcs.set_head(wt.path(), Head::Branch(branch("fix")));
    w.add_session("fix", SessionStatus::Live { cwd: None });
    let approved = plan_removal(w.svc(), removable(w.svc(), &session("fix")).unwrap(), None).unwrap().approve().unwrap();
    w.vcs.set_state(&branch("fix"), BranchState::Unmerged { ahead: NonZeroUsize::new(1).unwrap() });
    assert!(matches!(remove(w.svc(), approved, None), Err(Error::Hazards(_))));
    assert!(wt.path().exists());
    assert!(w.host.deleted.lock().unwrap().is_empty());
}

#[test]
fn uncommitted_files_and_detached_commits_block_removal() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    w.vcs.dirty.lock().unwrap().insert(wt.path().to_path_buf(), 3);
    w.vcs.detached_commits.lock().unwrap().insert(wt.path().to_path_buf(), 1);
    let plan = plan_removal(w.svc(), removable(w.svc(), &session("fix")).unwrap(), None).unwrap();
    assert!(matches!(plan.hazards(), [Hazard::Uncommitted { .. }, Hazard::Detached { .. }]));
    assert!(plan.approve().is_err());
}

#[test]
fn forced_removal_still_refuses_after_a_branch_switch() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    w.vcs.set_head(wt.path(), Head::Branch(branch("fix")));
    w.vcs.set_state(&branch("fix"), BranchState::Unmerged { ahead: NonZeroUsize::new(1).unwrap() });
    let forced = plan_removal(w.svc(), removable(w.svc(), &session("fix")).unwrap(), None).unwrap().approve().unwrap_err().force();
    w.vcs.set_head(wt.path(), Head::Branch(branch("other")));
    assert!(matches!(remove(w.svc(), forced, None), Err(Error::Changed(_))));
    assert!(w.vcs.deleted.lock().unwrap().is_empty());
}

#[test]
fn the_session_doing_the_removal_cant_remove_itself() {
    let w = World::new();
    w.existing_worktree("fix");
    let approved = plan_removal(w.svc(), removable(w.svc(), &session("fix")).unwrap(), None).unwrap().approve().unwrap();
    assert!(matches!(remove(w.svc(), approved, Some(&session("fix"))), Err(Error::OwnSession(_))));
}

#[test]
fn a_main_entry_never_removes_a_same_named_worktree() {
    let w = World::new();
    w.existing_worktree("api");
    w.add_session("api", SessionStatus::Live { cwd: Some(w.vcs.root.path().into()) });
    let plan = plan_removal(w.svc(), Removable::Session(session("api")), None).unwrap();
    assert_eq!(plan.deletes(), Deletes::Session);
}

#[test]
fn removal_notices_a_branch_switch() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    let approved = plan_removal(w.svc(), removable(w.svc(), &session("fix")).unwrap(), None).unwrap().approve().unwrap();
    w.vcs.set_head(wt.path(), Head::Branch(branch("new-work")));
    assert!(matches!(remove(w.svc(), approved, None), Err(Error::Changed(_))));
}

#[test]
fn removal_deletes_a_session_started_after_the_plan() {
    let w = World::new();
    w.existing_worktree("fix");
    let approved = plan_removal(w.svc(), removable(w.svc(), &session("fix")).unwrap(), None).unwrap().approve().unwrap();
    w.add_session("fix", SessionStatus::Live { cwd: None });
    remove(w.svc(), approved, None).unwrap();
    assert_eq!(*w.host.deleted.lock().unwrap(), [session("fix")]);
}

#[test]
fn sessions_without_worktrees_are_only_deleted() {
    let w = World::new();
    w.add_session("api", SessionStatus::Live { cwd: Some(w.vcs.root.path().into()) });
    let plan = plan_removal(w.svc(), removable(w.svc(), &session("api")).unwrap(), None).unwrap();
    assert_eq!(plan.deletes(), Deletes::Session);
    remove(w.svc(), plan.approve().unwrap(), None).unwrap();
    assert_eq!(w.host.deleted.lock().unwrap().len(), 1);
    assert!(w.vcs.deleted.lock().unwrap().is_empty());
}

#[test]
fn kill_is_resurrectable() {
    let w = World::new();
    w.add_session("fix", SessionStatus::Live { cwd: None });
    kill(w.svc(), &Running::new(session("fix"))).unwrap();
    assert_eq!(w.host.sessions.lock().unwrap()[0].status, SessionStatus::Exited);
}

#[test]
fn sweep_never_touches_live_worktrees() {
    let w = World::new();
    let live = w.existing_worktree("live");
    std::fs::write(live.path().join("code.rs"), "fn main() {}").unwrap();
    let gone = w.layout.worktree(&w.vcs.root, agent("gone"));
    std::fs::create_dir_all(gone.path().join("tmp/cache")).unwrap();
    std::fs::create_dir_all(w.layout.trash().join("old-1")).unwrap();

    sweep(&w.layout, &[live.clone(), gone.clone()]).unwrap();
    assert!(live.path().join("code.rs").exists());
    assert!(!gone.path().exists());
    assert!(std::fs::read_dir(w.layout.trash()).unwrap().next().is_none());
}
