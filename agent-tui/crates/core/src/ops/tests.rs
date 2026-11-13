use std::num::NonZeroUsize;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

use super::*;
use crate::activity::{Activity, Input, Move, Standing, State, TranscriptRef};
use crate::model::Head;
use crate::names::PullRequest;
use crate::ports::{LocalBranch, Probe, PullRequestHead, SessionStatus, TranscriptStamp};
use crate::testing::World;

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
fn checkout_fetches_a_branch_it_doesnt_know_and_refuses_one_it_cant_find() {
    let w = World::new();
    let repo = w.vcs.root.clone();
    w.vcs.deleted.lock().expect("lock").push(branch("pushed"));
    w.vcs.deleted.lock().expect("lock").push(branch("nowhere"));
    w.vcs.on_origin.lock().expect("lock").push(branch("pushed"));
    let name = checkout(w.svc(), &repo, &branch("pushed"), None).expect("succeeds");
    assert_eq!(name, agent("pushed"));
    assert_eq!(
        w.vcs
            .head(w.layout.worktree(&repo, name).path())
            .expect("succeeds"),
        Head::Branch(branch("pushed"))
    );
    assert!(matches!(
        checkout(w.svc(), &repo, &branch("nowhere"), None),
        Err(Error::BranchNotFound(b)) if b == branch("nowhere")
    ));
}

#[test]
fn checkout_of_an_already_fetched_branch_works_offline() {
    let w = World::new();
    let repo = w.vcs.root.clone();
    w.vcs.deleted.lock().expect("lock").push(branch("pushed"));
    w.vcs.fetched.lock().expect("lock").push(branch("pushed"));
    *w.vcs.offline.lock().expect("lock") = true;
    let name = checkout(w.svc(), &repo, &branch("pushed"), None).expect("origin/pushed is local");
    assert_eq!(name, agent("pushed"));
}

#[test]
fn checkout_cuts_long_branches_to_a_name_and_refuses_a_name_on_another_branch() {
    let w = World::new();
    let repo = w.vcs.root.clone();
    let long = branch("axel-chargebee-webhook-handler-classes");
    assert_eq!(
        checkout(w.svc(), &repo, &long, None).expect("succeeds"),
        agent("axel-chargebee")
    );
    assert_eq!(
        checkout(w.svc(), &repo, &long, None).expect("succeeds"),
        agent("axel-chargebee"),
        "a branch already checked out attaches to its worktree"
    );
    let other = branch("axel-chargebee-refund-flow");
    assert!(matches!(
        checkout(w.svc(), &repo, &other, None),
        Err(Error::NameTaken { name, on: Some(on) }) if name == agent("axel-chargebee") && on == long
    ));
}

fn pull_request(link: &str) -> (Checkout, PullRequest) {
    let target: Checkout = link.parse().expect("parses");
    let Checkout::PullRequest(request) = &target else {
        panic!("a pull request");
    };
    (target.clone(), request.clone())
}

#[test]
fn a_pull_request_from_the_repo_is_checked_out_by_its_branch() {
    let w = World::new();
    let repo = w.vcs.root.clone();
    let (target, request) = pull_request("https://github.com/acme/api/pull/482");
    w.vcs
        .deleted
        .lock()
        .expect("lock")
        .push(branch("feature/x"));
    w.vcs
        .on_origin
        .lock()
        .expect("lock")
        .push(branch("feature/x"));
    w.forge.heads.lock().expect("lock").insert(
        request,
        PullRequestHead {
            branch: branch("feature/x"),
            cross_repository: false,
        },
    );
    let launch = checkout_launch(w.svc(), &repo, &target, Some(agent("pr-482")), w.harness())
        .expect("succeeds");
    assert_eq!(launch.session, session("pr-482"));
    assert_eq!(
        w.vcs
            .head(w.layout.worktree(&repo, agent("pr-482")).path())
            .expect("succeeds"),
        Head::Branch(branch("feature/x"))
    );
    let (unknown, _) = pull_request("https://github.com/acme/api/pull/1");
    assert!(checkout_launch(w.svc(), &repo, &unknown, None, w.harness()).is_err());
}

#[test]
fn a_fork_pull_request_is_fetched_by_its_pull_ref_whatever_origin_calls_its_branch() {
    let w = World::new();
    let repo = w.vcs.root.clone();
    let (target, request) = pull_request("https://github.com/acme/api/pull/7");
    w.vcs.deleted.lock().expect("lock").push(branch("main"));
    w.vcs.fetched.lock().expect("lock").push(branch("main"));
    w.vcs.pull_requests.lock().expect("lock").push(7);
    w.forge.heads.lock().expect("lock").insert(
        request,
        PullRequestHead {
            branch: branch("main"),
            cross_repository: true,
        },
    );
    let launch = checkout_launch(w.svc(), &repo, &target, None, w.harness()).expect("succeeds");
    assert_eq!(launch.session, session("pr-7"));
    assert_eq!(
        w.vcs
            .head(w.layout.worktree(&repo, agent("pr-7")).path())
            .expect("succeeds"),
        Head::Branch(branch("pr/7")),
        "origin's main is the base repo's, not the fork's"
    );
    assert_eq!(
        *w.vcs.fetched.lock().expect("lock"),
        [branch("main"), branch("pr/7")]
    );
}

#[test]
fn a_branch_checked_out_by_another_repos_agent_is_not_this_repos() {
    let w = World::new();
    let other = RepoRoot::new(w.layout.root().join("../src/web")).expect("absolute");
    let wt = w.layout.worktree(&other, agent("develop"));
    w.vcs
        .add_worktree(
            &other,
            &wt,
            &WorktreeStart::Branch(LocalBranch::found(branch("develop"))),
        )
        .expect("succeeds");
    w.vcs
        .other_repos
        .lock()
        .expect("lock")
        .insert(wt.path().to_path_buf(), other);
    let result = checkout(w.svc(), &w.vcs.root, &branch("develop"), None);
    assert!(
        matches!(result, Err(Error::NameTaken { .. })),
        "web's develop is not api's develop: {result:?}"
    );
}

#[test]
fn new_agents_get_a_worktree_a_record_and_a_bootstrap_for_launch() {
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
    assert_eq!(launch.delivery, Delivery::AtLaunch);
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
            .pending()
            .expect("succeeds")
            .prompt
            .as_str()
            .ends_with("fix ci")
    );
    let created = w.host.created.lock().expect("lock");
    assert_eq!(created[0].1.args, ["exec", "fix"]);
}

#[test]
fn pending_prompts_are_resent_until_the_agent_shows_activity() {
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
            .pending()
            .is_none()
    );
}

#[test]
fn restarting_without_a_prompt_keeps_the_undelivered_first_prompt() {
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
    let before = w
        .store
        .record(&session("fix"))
        .expect("succeeds")
        .expect("recorded");
    w.add_session("fix", SessionStatus::Exited { cwd: None });
    attach(w.svc(), &agent("fix"), None, None, w.harness()).expect("succeeds");
    let after = w
        .store
        .record(&session("fix"))
        .expect("succeeds")
        .expect("recorded");
    assert_eq!(after.pending(), before.pending());
    assert!(after.pending().is_some());
    let spec = exec_spec(w.svc(), &session("fix"), Path::new("/x")).expect("succeeds");
    assert_eq!(spec.arguments().len(), 1);
}

#[test]
fn restarting_with_a_prompt_replaces_the_pending_one() {
    let w = World::new();
    let repo = w.vcs.root.clone();
    attach(
        w.svc(),
        &agent("fix"),
        Some(&prompt("first")),
        Some(&repo),
        w.harness(),
    )
    .expect("succeeds");
    w.add_session("fix", SessionStatus::Exited { cwd: None });
    attach(
        w.svc(),
        &agent("fix"),
        Some(&prompt("second")),
        None,
        w.harness(),
    )
    .expect("succeeds");
    let record = w
        .store
        .record(&session("fix"))
        .expect("succeeds")
        .expect("recorded");
    assert!(
        record
            .pending()
            .expect("pending")
            .prompt
            .as_str()
            .ends_with("second")
    );
}

#[test]
fn restarts_keep_the_agents_recorded_harness() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    let record = AgentRecord::new("codex".parse().expect("parses"), None);
    w.store
        .put_record(&session("fix"), &record)
        .expect("succeeds");
    let result = start(
        w.svc(),
        &Startable::worktree(
            HostedWorktree::admit(crate::testing::NAME_LIMIT, wt.clone()).expect("admitted"),
        ),
        w.harness(),
        None,
    );
    assert!(matches!(result, Err(Error::UnknownHarness(id)) if id.to_string() == "codex"));
}

#[test]
fn exited_agents_restart_through_agent_with_the_prompt_passed_at_launch() {
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
    assert_eq!(launch.delivery, Delivery::AtLaunch);
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
    working_agent(&w, "fix");
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

fn live_fix(w: &World) {
    w.existing_worktree("fix");
    w.add_session("fix", SessionStatus::Live { cwd: None });
}

fn ask_in_fix(w: &World) {
    let asking = Activity::next(
        Some(working_agent(w, "fix")),
        Reported::main(MainEvent::Common(crate::activity::Common::Asked {
            question: "Bash: ls".into(),
            about: crate::activity::About::Unknown,
        })),
        SystemTime::UNIX_EPOCH,
    );
    w.store
        .activities
        .lock()
        .expect("lock")
        .insert(session("fix"), asking);
}

fn nothing_typed(w: &World) -> bool {
    w.host.written.lock().expect("lock").is_empty()
}

#[test]
fn a_prompt_for_an_agent_that_is_asking_is_refused_and_nothing_is_typed() {
    let w = World::new();
    live_fix(&w);
    ask_in_fix(&w);
    let result = attach(
        w.svc(),
        &agent("fix"),
        Some(&prompt("hi")),
        None,
        w.harness(),
    );
    let Err(
        error @ Error::Held {
            hold: Hold::Asking, ..
        },
    ) = result
    else {
        panic!("{result:?}");
    };
    assert_eq!(
        error.to_string(),
        "nothing sent: fix is asking · attach to answer"
    );
    assert!(nothing_typed(&w));
}

#[test]
fn a_prompt_to_a_vanished_worktree_is_refused_before_the_worktree_is_made() {
    let w = World::new();
    let gone = w.layout.worktree(&w.vcs.root, agent("fix"));
    w.add_session(
        "fix",
        SessionStatus::Live {
            cwd: Some(gone.path().to_path_buf()),
        },
    );
    ask_in_fix(&w);
    let repo = w.vcs.root.clone();
    let result = attach(
        w.svc(),
        &agent("fix"),
        Some(&prompt("hi")),
        Some(&repo),
        w.harness(),
    );
    assert!(matches!(result, Err(Error::Held { .. })), "{result:?}");
    assert_eq!(
        w.layout.scan().expect("succeeds"),
        [],
        "no worktree was made"
    );
    assert!(nothing_typed(&w));
}

#[test]
fn a_session_with_no_activity_record_is_typed_into() {
    let w = World::new();
    live_fix(&w);
    inject(w.svc(), &session("fix"), &prompt("hi")).expect("types");
    assert_eq!(w.host.written.lock().expect("lock")[0].1, b"hi\r");
}

#[test]
fn a_launched_session_that_has_not_reported_is_booting() {
    let w = World::new();
    live_fix(&w);
    let launched = Activity::next(
        None,
        Reported::main(MainEvent::Launched(crate::activity::Join::Untraced)),
        SystemTime::now(),
    );
    w.store
        .activities
        .lock()
        .expect("lock")
        .insert(session("fix"), launched);
    let result = inject(w.svc(), &session("fix"), &prompt("hi"));
    assert!(matches!(
        result,
        Err(Error::Held {
            hold: Hold::Booting,
            ..
        })
    ));
    assert!(nothing_typed(&w));
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
    )
    .expect("succeeds")
    .elsewhere()
    .expect("succeeds");
    let plan = plan.approve().expect_err("fails");
    assert!(wt.path().exists());
    let removed = remove(w.svc(), plan.force()).expect("succeeds");
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
    )
    .expect("succeeds")
    .elsewhere()
    .expect("succeeds")
    .approve()
    .expect("succeeds");
    w.vcs.set_state(
        &branch("fix"),
        BranchState::Unmerged {
            ahead: NonZeroUsize::new(1).expect("succeeds"),
        },
    );
    assert!(matches!(remove(w.svc(), approved), Err(Error::Hazards(_))));
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
    )
    .expect("succeeds")
    .elsewhere()
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
    )
    .expect("succeeds")
    .elsewhere()
    .expect("succeeds")
    .approve()
    .expect_err("fails")
    .force();
    w.vcs.set_head(wt.path(), Head::Branch(branch("other")));
    assert!(matches!(remove(w.svc(), forced), Err(Error::Changed(_))));
    assert!(w.vcs.deleted.lock().expect("lock").is_empty());
}

#[test]
fn a_removal_of_the_callers_own_session_cant_be_approved() {
    let mut w = World::new();
    w.own = Some(session("fix"));
    w.existing_worktree("fix");
    let planned = plan_removal(
        w.svc(),
        removable(w.svc(), &session("fix")).expect("succeeds"),
    )
    .expect("succeeds");
    assert!(matches!(planned, Planned::Own(ref own) if own.name() == session("fix")));
    assert!(matches!(planned.elsewhere(), Err(Error::OwnSession(_))));
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
    let plan = plan_removal(w.svc(), Removable::Session(session("api")))
        .expect("succeeds")
        .elsewhere()
        .expect("succeeds");
    assert_eq!(plan.deletes(), Deletes::Session);
}

#[test]
fn removal_notices_a_branch_switch() {
    let w = World::new();
    let wt = w.existing_worktree("fix");
    let approved = plan_removal(
        w.svc(),
        removable(w.svc(), &session("fix")).expect("succeeds"),
    )
    .expect("succeeds")
    .elsewhere()
    .expect("succeeds")
    .approve()
    .expect("succeeds");
    w.vcs.set_head(wt.path(), Head::Branch(branch("new-work")));
    assert!(matches!(remove(w.svc(), approved), Err(Error::Changed(_))));
}

#[test]
fn removal_deletes_a_session_started_after_the_plan() {
    let w = World::new();
    w.existing_worktree("fix");
    let approved = plan_removal(
        w.svc(),
        removable(w.svc(), &session("fix")).expect("succeeds"),
    )
    .expect("succeeds")
    .elsewhere()
    .expect("succeeds")
    .approve()
    .expect("succeeds");
    w.add_session("fix", SessionStatus::Live { cwd: None });
    remove(w.svc(), approved).expect("succeeds");
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
    )
    .expect("succeeds")
    .elsewhere()
    .expect("succeeds");
    assert_eq!(plan.deletes(), Deletes::Session);
    remove(w.svc(), plan.approve().expect("succeeds")).expect("succeeds");
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
fn an_agent_name_over_the_hosts_limit_is_refused_before_any_worktree_is_made() {
    let world = World::new();
    let svc = world.svc();
    let long: AgentName = "an-agent-name-over-the-limit".parse().expect("parses");
    assert!(matches!(
        create(svc, &long, &world.vcs.root, None, world.harness()),
        Err(Error::Name(NameError::TooLong { limit: 20, .. }))
    ));
    assert!(matches!(
        checkout(svc, &world.vcs.root, &branch("main-2"), Some(long.clone())),
        Err(Error::Name(NameError::TooLong { .. }))
    ));
    assert!(!world.layout.worktree(&world.vcs.root, long).path().exists());
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

#[test]
fn launching_marks_a_previously_finished_agent_as_booting() {
    let w = World::new();
    let repo = w.vcs.root.clone();
    attach(w.svc(), &agent("fix"), None, Some(&repo), w.harness()).expect("succeeds");
    let long_ago = std::time::SystemTime::UNIX_EPOCH;
    let done = Activity::next(
        None,
        Reported::main(MainEvent::Finished {
            summary: None,
            running: Vec::new(),
            commands: crate::activity::CommandListing::Unknown,
        }),
        long_ago,
    );
    assert!(matches!(done.state, State::Done { .. }));
    w.store
        .activities
        .lock()
        .expect("lock")
        .insert(session("fix"), done);

    exec_spec(w.svc(), &session("fix"), Path::new("/x")).expect("succeeds");
    let activity = w
        .store
        .activities
        .lock()
        .expect("lock")
        .get(&session("fix"))
        .cloned()
        .expect("recorded");
    assert!(matches!(activity.state, State::Idle) && activity.session.is_booting());
    assert!(!activity.state.is_going(), "a launch isn't work");
    assert!(matches!(
        activity.session,
        crate::activity::Session::Launched {
            join: crate::activity::Join::Fresh(_),
            ..
        }
    ));
}

fn finished_long_ago() -> Activity {
    Activity::next(
        None,
        Reported::main(MainEvent::Finished {
            summary: None,
            running: Vec::new(),
            commands: crate::activity::CommandListing::Unknown,
        }),
        SystemTime::UNIX_EPOCH,
    )
}

fn hold_of_fix(w: &World) -> Option<Hold> {
    w.store
        .activities
        .lock()
        .expect("lock")
        .get(&session("fix"))
        .expect("recorded")
        .hold(SystemTime::now())
}

#[test]
fn a_record_from_an_earlier_run_reads_as_booting_as_soon_as_the_session_starts() {
    let w = World::new();
    w.existing_worktree("fix");
    w.store
        .activities
        .lock()
        .expect("lock")
        .insert(session("fix"), finished_long_ago());
    assert_eq!(hold_of_fix(&w), None, "the stale record says it is idle");
    attach(w.svc(), &agent("fix"), None, None, w.harness()).expect("succeeds");
    assert_eq!(hold_of_fix(&w), Some(Hold::Booting));
}

fn record_for_fix(w: &World) {
    w.store
        .put_record(
            &session("fix"),
            &AgentRecord::new(w.harness().id().clone(), None),
        )
        .expect("stores");
}

#[test]
fn a_session_agent_did_not_start_is_not_marked_when_it_resurrects() {
    let w = World::new();
    w.existing_worktree("fix");
    w.add_session("fix", SessionStatus::Exited { cwd: None });
    resurrect(w.svc(), &session("fix")).expect("succeeds");
    assert!(w.store.activities.lock().expect("lock").is_empty());
}

#[test]
fn a_session_that_cannot_be_created_ends_instead_of_booting_for_good() {
    let w = World::new();
    w.existing_worktree("fix");
    *w.host.create_fails.lock().expect("lock") = true;
    let result = attach(w.svc(), &agent("fix"), None, None, w.harness());
    assert!(matches!(result, Err(Error::Host(_))), "{result:?}");
    assert_eq!(hold_of_fix(&w), Some(Hold::Ended));
}

#[test]
fn a_launch_mark_that_cannot_be_written_does_not_stop_the_launch() {
    let w = World::new();
    record_for_fix(&w);
    *w.store.report_fails.lock().expect("lock") = true;
    let spec = exec_spec(w.svc(), &session("fix"), Path::new("/x"));
    assert!(spec.is_ok(), "{spec:?}");
}

#[test]
fn a_launch_that_cannot_start_the_harness_ends_instead_of_booting_for_good() {
    let w = World::new();
    let result = exec_spec(w.svc(), &session("fix"), Path::new("/x"));
    assert!(matches!(result, Err(Error::NoRecord(_))), "{result:?}");
    assert_eq!(hold_of_fix(&w), Some(Hold::Ended));
}

#[test]
fn resurrecting_an_exited_session_marks_its_record_booting_first() {
    let w = World::new();
    w.existing_worktree("fix");
    record_for_fix(&w);
    w.add_session("fix", SessionStatus::Exited { cwd: None });
    w.store
        .activities
        .lock()
        .expect("lock")
        .insert(session("fix"), finished_long_ago());
    attach(w.svc(), &agent("fix"), None, None, w.harness()).expect("succeeds");
    assert_eq!(hold_of_fix(&w), Some(Hold::Booting));
    w.store
        .activities
        .lock()
        .expect("lock")
        .insert(session("fix"), finished_long_ago());
    resurrect(w.svc(), &session("fix")).expect("succeeds");
    assert_eq!(hold_of_fix(&w), Some(Hold::Booting));
}

fn working_agent(w: &World, who: &str) -> Activity {
    let working = Activity::next(
        None,
        Reported::main(MainEvent::Prompted(Input::User("go".into()))),
        SystemTime::UNIX_EPOCH,
    );
    w.store
        .activities
        .lock()
        .expect("lock")
        .insert(session(who), working.clone());
    working
}

fn stored_state(w: &World, who: &str) -> State {
    w.store
        .activities
        .lock()
        .expect("lock")
        .get(&session(who))
        .expect("recorded")
        .state
        .clone()
}

fn transcript() -> TranscriptRef {
    TranscriptRef::new("/t/session.jsonl")
}

fn stamp() -> TranscriptStamp {
    TranscriptStamp::new(10, None)
}

fn probing(w: &World, found: Probe) {
    *w.harnesses.0.probe.lock().expect("lock") = found;
}

fn standing_of(activity: &Activity) -> Standing {
    activity.standing().expect("going or stopped")
}

#[test]
fn a_transcript_ending_in_an_interrupt_stops_the_working_agent_it_was_read_for() {
    let w = World::new();
    let working = working_agent(&w, "fix");
    let marker = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(50);
    probing(&w, Probe::Interrupted(stamp(), marker));
    let (probe, moved) = probe_interrupt(
        w.svc(),
        &session("fix"),
        &transcript(),
        standing_of(&working),
        None,
    )
    .expect("succeeds");
    assert_eq!(probe, Probe::Interrupted(stamp(), marker));
    assert!(matches!(moved, Some(Move::Interrupted(..))));
    assert_eq!(stored_state(&w, "fix"), State::Interrupted);
    let stored = w.store.activities.lock().expect("lock")[&session("fix")].clone();
    assert_eq!(
        stored.standing().map(Standing::after),
        Some(marker),
        "it counts as interrupted from the record, so work landing within the lag still reads as going"
    );
}

#[test]
fn a_transcript_still_going_or_older_leaves_the_agent_working() {
    let w = World::new();
    let working = working_agent(&w, "fix");
    for found in [
        Probe::Going(stamp()),
        Probe::Older(stamp()),
        Probe::Unchanged,
        Probe::NoTranscript,
    ] {
        probing(&w, found);
        probe_interrupt(
            w.svc(),
            &session("fix"),
            &transcript(),
            standing_of(&working),
            None,
        )
        .expect("succeeds");
        assert!(stored_state(&w, "fix").is_going());
    }
}

#[test]
fn an_interrupt_found_after_a_newer_prompt_arrived_does_not_idle_the_newer_turn() {
    let w = World::new();
    let working = working_agent(&w, "fix");
    let asked_at = standing_of(&working);
    let newer = Activity::next(
        Some(working),
        Reported::main(MainEvent::Prompted(Input::User("again".into()))),
        SystemTime::UNIX_EPOCH,
    );
    w.store
        .activities
        .lock()
        .expect("lock")
        .insert(session("fix"), newer);
    probing(&w, Probe::Interrupted(stamp(), SystemTime::UNIX_EPOCH));
    probe_interrupt(w.svc(), &session("fix"), &transcript(), asked_at, None).expect("succeeds");
    assert!(stored_state(&w, "fix").is_going());
}

#[test]
fn an_interrupted_agent_whose_transcript_went_on_is_working_again() {
    let w = World::new();
    let working = working_agent(&w, "fix");
    probing(&w, Probe::Interrupted(stamp(), SystemTime::UNIX_EPOCH));
    probe_interrupt(
        w.svc(),
        &session("fix"),
        &transcript(),
        standing_of(&working),
        None,
    )
    .expect("succeeds");
    let stopped = w.store.activities.lock().expect("lock")[&session("fix")].clone();

    probing(&w, Probe::Interrupted(stamp(), SystemTime::UNIX_EPOCH));
    probe_interrupt(
        w.svc(),
        &session("fix"),
        &transcript(),
        standing_of(&stopped),
        None,
    )
    .expect("succeeds");
    assert_eq!(stored_state(&w, "fix"), State::Interrupted, "still stopped");

    probing(&w, Probe::Going(stamp()));
    probe_interrupt(
        w.svc(),
        &session("fix"),
        &transcript(),
        standing_of(&stopped),
        None,
    )
    .expect("succeeds");
    assert!(stored_state(&w, "fix").is_going());
}

mod restart;
