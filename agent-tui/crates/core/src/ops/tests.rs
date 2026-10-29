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
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = dir.path().join("src/api");
        std::fs::create_dir_all(&root).expect("creates");
        Self {
            vcs: FakeVcs::new(&root),
            layout: Layout::new(dir.path().join("w")).expect("absolute"),
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
        self.host.sessions.lock().expect("lock").push(SessionInfo {
            name: name.parse().expect("parses"),
            status,
        });
    }

    fn existing_worktree(&self, name: &str) -> AgentWorktree {
        let wt = self
            .layout
            .worktree(&self.vcs.root, name.parse().expect("parses"));
        self.vcs
            .add_worktree(&self.vcs.root, &wt, &WorktreeStart::DefaultBase)
            .expect("succeeds");
        wt
    }
}

fn agent(s: &str) -> AgentName {
    s.parse().expect("parses")
}

fn session(s: &str) -> SessionName {
    s.parse().expect("parses")
}

fn prompt(s: &str) -> Prompt {
    s.parse().expect("parses")
}

fn branch(s: &str) -> BranchName {
    s.parse().expect("parses")
}

#[test]
fn new_agents_get_a_worktree_a_record_and_a_queued_bootstrap() {
    let w = World::new();
    let repo = w.vcs.root.clone();
    let launch = attach(
        w.svc(),
        &agent("fix"),
        Some(&prompt("fix ci")),
        Some(&repo),
        w.harness(),
    )
    .expect("succeeds");
    assert_eq!(launch.delivery, Delivery::Queued);
    assert!(
        w.layout
            .worktree(&repo, agent("fix"))
            .path()
            .join(".git")
            .exists()
    );
    let record = w
        .store
        .record(&session("fix"))
        .expect("succeeds")
        .expect("succeeds");
    assert!(
        record
            .pending
            .expect("succeeds")
            .prompt
            .as_str()
            .ends_with("fix ci")
    );
    let created = w.host.created.lock().expect("lock");
    assert_eq!(created[0].1.args, ["exec", "fix"]);
}

#[test]
fn queued_prompts_are_resent_until_the_agent_shows_activity() {
    let w = World::new();
    let repo = w.vcs.root.clone();
    attach(
        w.svc(),
        &agent("fix"),
        Some(&prompt("go")),
        Some(&repo),
        w.harness(),
    )
    .expect("succeeds");
    let crashed = exec_spec(w.svc(), &session("fix"), Path::new("/x")).expect("succeeds");
    let retried = exec_spec(w.svc(), &session("fix"), Path::new("/x")).expect("succeeds");
    assert_eq!(crashed.arguments().len(), 1);
    assert_eq!(
        retried.arguments().len(),
        1,
        "no activity yet, so the prompt is sent again"
    );

    w.harnesses.0.active.store(true, Ordering::SeqCst);
    let resurrected = exec_spec(w.svc(), &session("fix"), Path::new("/x")).expect("succeeds");
    assert_eq!(resurrected.arguments(), &[] as &[std::ffi::OsString]);
    assert!(
        w.store
            .record(&session("fix"))
            .expect("succeeds")
            .expect("succeeds")
            .pending
            .is_none()
    );
}

#[test]
fn restarts_keep_the_agents_recorded_harness() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    let record = AgentRecord {
        harness: "codex".parse().expect("parses"),
        pending: None,
    };
    w.store
        .put_record(&session("fix"), &record)
        .expect("succeeds");
    let result = start(w.svc(), &Startable::worktree(&wt), w.harness(), None);
    assert!(matches!(result, Err(Error::UnknownHarness(id)) if id.to_string() == "codex"));
}

#[test]
fn exited_agents_restart_through_agent_with_the_prompt_queued() {
    let w = World::new();
    w.existing_worktree("fix");
    w.add_session("fix", SessionStatus::Exited { cwd: None });
    let launch = attach(
        w.svc(),
        &agent("fix"),
        Some(&prompt("hi")),
        None,
        w.harness(),
    )
    .expect("succeeds");
    assert_eq!(launch.delivery, Delivery::Queued);
    assert_eq!(
        w.host.created.lock().expect("lock").len(),
        1,
        "a fresh session, not zellij's cached command"
    );
    assert!(w.host.written.lock().expect("lock").is_empty());
}

#[test]
fn prompts_for_live_sessions_are_typed_in_with_the_harness_encoding() {
    let w = World::new();
    w.existing_worktree("fix");
    w.add_session("fix", SessionStatus::Live { cwd: None });
    let launch = attach(
        w.svc(),
        &agent("fix"),
        Some(&prompt("hi")),
        None,
        w.harness(),
    )
    .expect("succeeds");
    assert_eq!(launch.delivery, Delivery::Injected);
    assert_eq!(w.host.written.lock().expect("lock")[0].1, b"hi\r");
}

#[test]
fn exited_repo_sessions_resurrect_instead_of_creating_worktrees() {
    let w = World::new();
    w.add_session("api", SessionStatus::Exited { cwd: None });
    let repo = w.vcs.root.clone();
    let launch = attach(w.svc(), &agent("api"), None, Some(&repo), w.harness()).expect("succeeds");
    assert_eq!(launch.spec.program(), "attach");
    assert_eq!(w.layout.scan().expect("succeeds"), []);
}

#[test]
fn vanished_worktrees_are_only_recreated_from_their_own_repo() {
    let w = World::new();
    w.add_session(
        "fix",
        SessionStatus::Live {
            cwd: Some(w.layout.root().join("web/fix")),
        },
    );
    let repo = w.vcs.root.clone();
    let result = attach(w.svc(), &agent("fix"), None, Some(&repo), w.harness());
    assert!(matches!(result, Err(Error::WorktreeGone { .. })));
}

#[test]
fn sessions_sitting_in_another_agents_worktree_just_attach() {
    let w = World::new();
    w.existing_worktree("other");
    w.add_session(
        "fix",
        SessionStatus::Live {
            cwd: Some(w.layout.root().join("api/other/src")),
        },
    );
    let launch = attach(w.svc(), &agent("fix"), None, None, w.harness()).expect("succeeds");
    assert_eq!(launch.spec.program(), "attach");
}

#[test]
fn removal_refuses_unmerged_work_unless_forced() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    w.vcs.set_head(wt.path(), Head::Branch(branch("fix")));
    w.vcs.set_state(
        &branch("fix"),
        BranchState::Unmerged {
            ahead: NonZeroUsize::new(2).expect("succeeds"),
        },
    );
    let plan = plan_removal(
        w.svc(),
        removable(w.svc(), &session("fix")).expect("succeeds"),
        None,
    )
    .expect("succeeds");
    let plan = plan.approve().expect_err("fails");
    assert!(wt.path().exists());
    let removed = remove(w.svc(), plan.force(), None).expect("succeeds");
    assert!(matches!(removed, Removed::Agent { ref warnings, .. } if warnings.is_empty()));
    assert!(!wt.path().exists());
    assert_eq!(*w.vcs.deleted.lock().expect("lock"), [branch("fix")]);
}

#[test]
fn a_refused_removal_leaves_the_session_running() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    w.vcs.set_head(wt.path(), Head::Branch(branch("fix")));
    w.add_session("fix", SessionStatus::Live { cwd: None });
    let approved = plan_removal(
        w.svc(),
        removable(w.svc(), &session("fix")).expect("succeeds"),
        None,
    )
    .expect("succeeds")
    .approve()
    .expect("succeeds");
    w.vcs.set_state(
        &branch("fix"),
        BranchState::Unmerged {
            ahead: NonZeroUsize::new(1).expect("succeeds"),
        },
    );
    assert!(matches!(
        remove(w.svc(), approved, None),
        Err(Error::Hazards(_))
    ));
    assert!(wt.path().exists());
    assert!(w.host.deleted.lock().expect("lock").is_empty());
}

#[test]
fn uncommitted_files_and_detached_commits_block_removal() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    w.vcs
        .dirty
        .lock()
        .expect("succeeds")
        .insert(wt.path().to_path_buf(), 3);
    w.vcs
        .detached_commits
        .lock()
        .expect("succeeds")
        .insert(wt.path().to_path_buf(), 1);
    let plan = plan_removal(
        w.svc(),
        removable(w.svc(), &session("fix")).expect("succeeds"),
        None,
    )
    .expect("succeeds");
    assert!(matches!(
        plan.hazards(),
        [Hazard::Uncommitted { .. }, Hazard::Detached { .. }]
    ));
    assert!(plan.approve().is_err());
}

#[test]
fn forced_removal_still_refuses_after_a_branch_switch() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    w.vcs.set_head(wt.path(), Head::Branch(branch("fix")));
    w.vcs.set_state(
        &branch("fix"),
        BranchState::Unmerged {
            ahead: NonZeroUsize::new(1).expect("succeeds"),
        },
    );
    let forced = plan_removal(
        w.svc(),
        removable(w.svc(), &session("fix")).expect("succeeds"),
        None,
    )
    .expect("succeeds")
    .approve()
    .expect_err("fails")
    .force();
    w.vcs.set_head(wt.path(), Head::Branch(branch("other")));
    assert!(matches!(
        remove(w.svc(), forced, None),
        Err(Error::Changed(_))
    ));
    assert!(w.vcs.deleted.lock().expect("lock").is_empty());
}

#[test]
fn the_session_doing_the_removal_cant_remove_itself() {
    let w = World::new();
    w.existing_worktree("fix");
    let approved = plan_removal(
        w.svc(),
        removable(w.svc(), &session("fix")).expect("succeeds"),
        None,
    )
    .expect("succeeds")
    .approve()
    .expect("succeeds");
    assert!(matches!(
        remove(w.svc(), approved, Some(&session("fix"))),
        Err(Error::OwnSession(_))
    ));
}

#[test]
fn a_main_entry_never_removes_a_same_named_worktree() {
    let w = World::new();
    w.existing_worktree("api");
    w.add_session(
        "api",
        SessionStatus::Live {
            cwd: Some(w.vcs.root.path().into()),
        },
    );
    let plan = plan_removal(w.svc(), Removable::Session(session("api")), None).expect("succeeds");
    assert_eq!(plan.deletes(), Deletes::Session);
}

#[test]
fn removal_notices_a_branch_switch() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    let approved = plan_removal(
        w.svc(),
        removable(w.svc(), &session("fix")).expect("succeeds"),
        None,
    )
    .expect("succeeds")
    .approve()
    .expect("succeeds");
    w.vcs.set_head(wt.path(), Head::Branch(branch("new-work")));
    assert!(matches!(
        remove(w.svc(), approved, None),
        Err(Error::Changed(_))
    ));
}

#[test]
fn removal_deletes_a_session_started_after_the_plan() {
    let w = World::new();
    w.existing_worktree("fix");
    let approved = plan_removal(
        w.svc(),
        removable(w.svc(), &session("fix")).expect("succeeds"),
        None,
    )
    .expect("succeeds")
    .approve()
    .expect("succeeds");
    w.add_session("fix", SessionStatus::Live { cwd: None });
    remove(w.svc(), approved, None).expect("succeeds");
    assert_eq!(*w.host.deleted.lock().expect("lock"), [session("fix")]);
}

#[test]
fn sessions_without_worktrees_are_only_deleted() {
    let w = World::new();
    w.add_session(
        "api",
        SessionStatus::Live {
            cwd: Some(w.vcs.root.path().into()),
        },
    );
    let plan = plan_removal(
        w.svc(),
        removable(w.svc(), &session("api")).expect("succeeds"),
        None,
    )
    .expect("succeeds");
    assert_eq!(plan.deletes(), Deletes::Session);
    remove(w.svc(), plan.approve().expect("succeeds"), None).expect("succeeds");
    assert_eq!(w.host.deleted.lock().expect("lock").len(), 1);
    assert!(w.vcs.deleted.lock().expect("lock").is_empty());
}

#[test]
fn kill_is_resurrectable() {
    let w = World::new();
    w.add_session("fix", SessionStatus::Live { cwd: None });
    kill(w.svc(), &Running::new(session("fix"))).expect("succeeds");
    assert_eq!(
        w.host.sessions.lock().expect("lock")[0].status,
        SessionStatus::Exited { cwd: None }
    );
}

#[test]
fn sweep_never_touches_live_worktrees() {
    let w = World::new();
    let live = w.existing_worktree("live");
    std::fs::write(live.path().join("code.rs"), "fn main() {}").expect("writes");
    let gone = w.layout.worktree(&w.vcs.root, agent("gone"));
    std::fs::create_dir_all(gone.path().join("tmp/cache")).expect("creates");
    std::fs::create_dir_all(w.layout.trash().join("old-1")).expect("creates");

    sweep(&w.layout, &[live.clone(), gone.clone()]).expect("succeeds");
    assert!(live.path().join("code.rs").exists());
    assert!(!gone.path().exists());
    assert!(
        std::fs::read_dir(w.layout.trash())
            .expect("succeeds")
            .next()
            .is_none()
    );
}

#[test]
fn a_listed_repo_shows_with_nothing_running_until_its_forgotten() {
    let world = World::new();
    let svc = world.svc();
    assert!(snapshot(svc).expect("succeeds").0.is_empty());
    let root = world.vcs.root.path().to_path_buf();
    let listed = list_repo(svc, &root.join("src")).expect("succeeds");
    assert_eq!(
        listed,
        Listed {
            repo: world.vcs.root.clone(),
            listing: Listing::Added
        }
    );
    assert_eq!(
        list_repo(svc, &root).expect("succeeds").listing,
        Listing::AlreadyListed
    );
    let (snap, warnings) = snapshot(svc).expect("succeeds");
    assert!(warnings.is_empty(), "{warnings:?}");
    let repo = &snap.repos()[0];
    assert_eq!(
        (
            repo.name().as_ref(),
            repo.entries().len(),
            repo.first().presence()
        ),
        ("api", 1, None)
    );
    assert_eq!(
        unlist_repo(svc, &root).expect("succeeds").unlisting,
        Unlisting::Dropped
    );
    assert_eq!(
        unlist_repo(svc, &root).expect("succeeds").unlisting,
        Unlisting::NotListed
    );
    assert!(snapshot(svc).expect("succeeds").0.is_empty());
}

#[test]
fn a_repo_whose_name_cant_be_a_session_isnt_listed() {
    let world = World::new();
    let svc = world.svc();
    let long = world.vcs.root.path().join("../a-very-long-repository-name");
    let long = RepoRoot::new(long).expect("absolute");
    world
        .vcs
        .other_repos
        .lock()
        .expect("succeeds")
        .insert(long.path().to_path_buf(), long.clone());
    assert!(matches!(list_repo(svc, long.path()), Err(Error::Name(_))));
    assert_eq!(world.store.repos().expect("store"), Vec::new());
}

#[test]
fn forgetting_a_listed_path_inside_another_repo_drops_that_path_not_the_repo() {
    let world = World::new();
    let svc = world.svc();
    let inner = RepoRoot::new(world.vcs.root.path().join("vendor/thing")).expect("absolute");
    world.store.list_repo(&world.vcs.root).expect("store");
    world.store.list_repo(&inner).expect("store");
    assert_eq!(
        unlist_repo(svc, inner.path()).expect("succeeds"),
        Unlisted {
            repo: inner,
            unlisting: Unlisting::Dropped
        }
    );
    assert_eq!(
        world.store.repos().expect("store"),
        vec![world.vcs.root.clone()]
    );
    assert_eq!(
        unlist_repo(svc, &world.vcs.root.path().join("src"))
            .expect("succeeds")
            .repo,
        world.vcs.root,
        "an unlisted path falls back to the repo holding it"
    );
}

#[test]
fn a_listed_path_that_stopped_being_a_repo_warns_and_can_be_forgotten() {
    let world = World::new();
    let svc = world.svc();
    let gone = world.vcs.root.path().join("../gone");
    let gone = RepoRoot::new(gone).expect("absolute");
    world.store.list_repo(&gone).expect("store");
    world
        .vcs
        .not_repos
        .lock()
        .expect("succeeds")
        .push(gone.path().to_path_buf());
    let (snap, warnings) = snapshot(svc).expect("succeeds");
    assert!(snap.is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].to_string().contains("isn't a git repo"));
    assert!(matches!(
        list_repo(svc, gone.path()),
        Err(Error::NotARepo(_))
    ));
    unlist_repo(svc, gone.path()).expect("succeeds");
    assert_eq!(world.store.repos().expect("store"), Vec::new());
}

#[test]
fn opening_a_repo_or_creating_an_agent_in_it_lists_it() {
    let world = World::new();
    let svc = world.svc();
    open_repo(svc, &world.vcs.root, world.harness()).expect("succeeds");
    assert_eq!(
        world.store.repos().expect("store"),
        vec![world.vcs.root.clone()]
    );
    world.store.unlist_repo(&world.vcs.root).expect("store");
    create(
        svc,
        &"fix".parse().expect("parses"),
        &world.vcs.root,
        None,
        world.harness(),
    )
    .expect("succeeds");
    assert_eq!(
        world.store.repos().expect("store"),
        vec![world.vcs.root.clone()]
    );
}

#[test]
fn attaching_here_starts_the_worktrees_agent_and_never_creates_one() {
    let world = World::new();
    let svc = world.svc();
    let fix = world.existing_worktree("fix");
    let launch = attach_here(svc, &fix, world.harness()).expect("succeeds");
    assert_eq!(launch.session.as_str(), "fix");
    let created = world.host.created.lock().expect("lock");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].1.cwd, fix.path());
    drop(created);

    let gone = world
        .layout
        .worktree(&world.vcs.root, "gone".parse().expect("parses"));
    std::fs::create_dir_all(gone.path()).expect("creates");
    assert!(matches!(
        attach_here(svc, &gone, world.harness()),
        Err(Error::WorktreeGone { .. })
    ));
}
