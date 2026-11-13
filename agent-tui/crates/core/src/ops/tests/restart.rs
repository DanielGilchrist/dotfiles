use std::sync::atomic::Ordering;
use std::time::Duration;

use super::*;
use crate::activity::Busy;
use crate::ports::SurvivorState;
use crate::testing::{Doing, Lurking, Step, Stopping};

const NOW: Patience = Patience {
    polls: 3,
    every: Duration::ZERO,
};

fn plan(w: &World, at: &Startable) -> RestartApproval {
    plan_restart(w.svc(), at.clone())
        .expect("plans")
        .elsewhere()
        .expect("not own")
}

fn go(w: &World, at: &Startable) -> Restart {
    match plan(w, at) {
        RestartApproval::Free(go) => go,
        RestartApproval::Needs(would_stop) => panic!("it would stop {:?}", would_stop.stops()),
    }
}

fn would_stop(w: &World, at: &Startable) -> WouldStop {
    match plan(w, at) {
        RestartApproval::Needs(would_stop) => would_stop,
        RestartApproval::Free(_) => panic!("nothing to stop"),
    }
}

fn run(w: &World, go: Restart) -> Result<Restarted> {
    restart_within(w.svc(), go, NOW)
}

fn steps(w: &World) -> Vec<Step> {
    w.journal().steps()
}

fn lurk(w: &World, at: &Startable, lurking: Lurking) {
    w.harnesses.0.lurk(at.cwd(), lurking);
}

fn stopping(w: &World, how: Stopping) {
    *w.harnesses.0.stop.lock().expect("lock") = how;
}

#[test]
fn a_session_agent_did_not_start_has_no_record_to_restart() {
    let w = World::new();
    w.existing_worktree("fix");
    w.add_session("fix", SessionStatus::Live { cwd: None });
    let at = w.agent("other", Doing::Idle);
    w.store.records.lock().expect("lock").clear();
    assert!(matches!(
        plan_restart(w.svc(), at),
        Err(Error::NoRecord(name)) if name == session("other")
    ));
}

#[test]
fn an_idle_agent_is_approved_and_a_busy_one_must_be_forced() {
    let w = World::new();
    let idle = w.agent("idle", Doing::Idle);
    assert!(matches!(plan(&w, &idle), RestartApproval::Free(_)));

    let busy = w.agent("busy", Doing::Working);
    let interrupting = would_stop(&w, &busy);
    assert_eq!(
        interrupting.stops().activity(),
        Some(Interrupts::Session(Busy::Working))
    );
    assert_eq!(interrupting.plan().name(), session("busy"));
    run(&w, interrupting.force()).expect("forced");
    assert_eq!(steps(&w).first(), Some(&Step::Kill(session("busy"))));
}

#[test]
fn a_restart_checks_kills_the_session_stops_the_process_marks_the_launch_then_starts() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    lurk(&w, &at, Lurking::Survivor(SurvivorState::Idle));
    let done = run(&w, go(&w, &at)).expect("restarts");
    assert_eq!((done.end, done.label), (End::Started, "fake"));
    assert_eq!(
        steps(&w),
        vec![
            Step::Kill(session("fix")),
            Step::Stop(at.cwd().to_path_buf()),
            Step::Report(session("fix")),
            Step::Create(session("fix")),
            Step::Detach(session("fix")),
        ]
    );
    assert!(w.harnesses.0.survivors.lock().expect("lock").is_empty());
}

#[test]
fn an_agent_that_became_busy_since_it_was_approved_idle_is_left_alone() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    let approved = go(&w, &at);
    w.agent("elsewhere", Doing::Working);
    let working = w.store.activities.lock().expect("lock")[&session("elsewhere")].clone();
    w.store
        .activities
        .lock()
        .expect("lock")
        .insert(session("fix"), working);
    assert!(matches!(
        run(&w, approved),
        Err(Error::WouldStop { stops, .. }) if stops.activity() == Some(Interrupts::Session(Busy::Working))
    ));
    assert_eq!(steps(&w), vec![], "not even killed");
}

#[test]
fn a_live_agent_whose_process_is_mid_task_must_be_forced_and_is_never_killed_when_refused() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    lurk(&w, &at, Lurking::Survivor(SurvivorState::Busy));
    let interrupting = would_stop(&w, &at);
    assert_eq!(interrupting.stops().activity(), Some(Interrupts::Survivor));

    lurk(&w, &at, Lurking::Survivor(SurvivorState::Idle));
    let approved = go(&w, &at);
    lurk(&w, &at, Lurking::Survivor(SurvivorState::Busy));
    assert!(matches!(
        run(&w, approved),
        Err(Error::WouldStop { stops, .. }) if stops.activity() == Some(Interrupts::Survivor)
    ));
    assert_eq!(steps(&w), vec![], "a refusal leaves the session running");
}

#[test]
fn a_registry_that_cant_be_read_refuses_before_anything_is_killed() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    let approved = go(&w, &at);
    w.harnesses.0.lookup_fails.store(true, Ordering::SeqCst);
    assert!(matches!(run(&w, approved), Err(Error::Harness(_))));
    assert_eq!(steps(&w), vec![]);
}

#[test]
fn a_process_that_wont_stop_fails_the_restart_before_anything_starts() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    lurk(&w, &at, Lurking::Survivor(SurvivorState::Idle));
    stopping(&w, Stopping::Fails);
    assert!(matches!(run(&w, go(&w, &at)), Err(Error::Harness(_))));
    assert!(
        !steps(&w)
            .iter()
            .any(|s| matches!(s, Step::Create(_) | Step::Detach(_) | Step::Report(_)))
    );
}

#[test]
fn a_stop_that_leaves_the_process_running_is_an_error_not_a_quiet_attach() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    lurk(&w, &at, Lurking::Survivor(SurvivorState::Idle));
    stopping(&w, Stopping::Ignored);
    assert!(matches!(
        run(&w, go(&w, &at)),
        Err(Error::Survives { label: "fake", .. })
    ));
    assert_eq!(w.host.created.lock().expect("lock").len(), 0);
    let stops = steps(&w)
        .iter()
        .filter(|s| matches!(s, Step::Stop(_)))
        .count();
    assert_eq!(stops, 1, "it is stopped once, not on every poll");
}

#[test]
fn a_process_the_harness_only_registers_after_the_kill_is_waited_for_and_stopped() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    lurk(
        &w,
        &at,
        Lurking::Appears {
            after: 2,
            state: SurvivorState::Idle,
        },
    );
    run(&w, go(&w, &at)).expect("restarts");
    let steps = steps(&w);
    let at_kill = steps.iter().position(|s| matches!(s, Step::Kill(_)));
    let at_stop = steps.iter().position(|s| matches!(s, Step::Stop(_)));
    assert!(at_kill < at_stop && at_stop.is_some(), "{steps:?}");
    assert!(steps.contains(&Step::Detach(session("fix"))));
}

#[test]
fn a_process_still_in_its_terminal_times_out_and_nothing_starts() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    lurk(&w, &at, Lurking::Interactive(SurvivorState::Idle));
    assert!(matches!(
        run(&w, go(&w, &at)),
        Err(Error::StillInTerminal { label: "fake", .. })
    ));
    assert!(steps(&w).contains(&Step::Kill(session("fix"))));
    assert!(
        !steps(&w).iter().any(|s| matches!(s, Step::Stop(_))),
        "nothing to stop"
    );
    assert!(
        !steps(&w)
            .iter()
            .any(|s| matches!(s, Step::Create(_) | Step::Detach(_)))
    );
}

#[test]
fn an_exited_agent_with_a_busy_process_needs_forcing_and_is_started_by_a_restart_of_it() {
    let w = World::new();
    let at = w.ghost("fix", Lurking::Survivor(SurvivorState::Busy));
    let interrupting = would_stop(&w, &at);
    assert_eq!(interrupting.stops().activity(), Some(Interrupts::Survivor));
    let done = run(&w, interrupting.force()).expect("restarts");
    assert_eq!(done.end, End::Started);
    assert!(!steps(&w).contains(&Step::Kill(session("fix"))));
    assert_eq!(w.host.detached().len(), 1);
}

#[test]
fn an_exited_agents_process_that_became_busy_since_approval_is_left_alone() {
    let w = World::new();
    let at = w.ghost("fix", Lurking::Survivor(SurvivorState::Idle));
    let approved = go(&w, &at);
    lurk(&w, &at, Lurking::Survivor(SurvivorState::Busy));
    assert!(matches!(run(&w, approved), Err(Error::WouldStop { .. })));
    assert_eq!(steps(&w), vec![]);
}

#[test]
fn the_callers_own_session_has_a_plan_that_cant_be_approved() {
    let mut w = World::new();
    w.own = Some(session("fix"));
    let at = w.agent("fix", Doing::Working);
    let planned = plan_restart(w.svc(), at).expect("plans");
    assert!(matches!(
        planned,
        Planned::Own(ref own)
            if own.name() == session("fix")
                && own.plan().stops().and_then(Stops::activity) == Some(Interrupts::Session(Busy::Working))
    ));
    assert!(matches!(planned.elsewhere(), Err(Error::OwnSession(_))));
}

#[test]
fn a_kill_that_fails_stops_the_restart_before_the_process_is_touched() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    lurk(&w, &at, Lurking::Survivor(SurvivorState::Idle));
    *w.host.kill_fails.lock().expect("lock") = true;
    assert!(matches!(run(&w, go(&w, &at)), Err(Error::Host(_))));
    assert_eq!(steps(&w), vec![Step::Kill(session("fix"))]);
}

#[test]
fn a_session_the_host_still_lists_as_running_for_a_while_after_its_kill_is_waited_for() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    *w.host.kill_lag.lock().expect("lock") = 2;
    run(&w, go(&w, &at)).expect("restarts");

    let slow = World::new();
    let at = slow.agent("fix", Doing::Idle);
    *slow.host.kill_lag.lock().expect("lock") = 50;
    assert!(matches!(
        run(&slow, go(&slow, &at)),
        Err(Error::Lingers(name)) if name == session("fix")
    ));
    assert!(!steps(&slow).iter().any(|s| matches!(s, Step::Create(_))));
}

#[test]
fn a_live_plan_for_a_session_that_exited_since_is_not_killed_again() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    let approved = go(&w, &at);
    w.host.sessions.lock().expect("lock").clear();
    run(&w, approved).expect("restarts");
    assert_eq!(w.host.killed(), vec![]);
    assert_eq!(w.host.detached(), vec![session("fix")]);
}

#[test]
fn a_session_that_cant_be_started_detached_ends_the_launch_mark() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    *w.host.start_fails.lock().expect("lock") = true;
    assert!(matches!(run(&w, go(&w, &at)), Err(Error::Host(_))));
    let activity = w.store.activities.lock().expect("lock")[&session("fix")].clone();
    assert!(
        activity.boot(std::time::SystemTime::now()).is_none(),
        "not left booting"
    );
    assert_eq!(activity.state, crate::activity::State::Exited);
}

#[test]
fn a_session_the_host_lists_late_is_waited_for_and_one_that_never_shows_is_an_error() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    *w.host.start_lag.lock().expect("lock") = 2;
    run(&w, go(&w, &at)).expect("restarts");

    let never = World::new();
    let at = never.agent("fix", Doing::Idle);
    *never.host.start_lag.lock().expect("lock") = 50;
    assert!(matches!(
        run(&never, go(&never, &at)),
        Err(Error::NotStarted(name)) if name == session("fix")
    ));
    let activity = never.store.activities.lock().expect("lock")[&session("fix")].clone();
    assert!(activity.boot(std::time::SystemTime::now()).is_none());
}

#[test]
fn a_fleet_splits_idle_from_busy_sets_own_aside_and_skips_what_is_not_agents() {
    let mut w = World::new();
    w.own = Some(session("me"));
    w.agent("idle", Doing::Idle);
    w.agent("busy", Doing::Working);
    w.agent("me", Doing::Idle);
    w.agent("stranger", Doing::Idle);
    w.store
        .records
        .lock()
        .expect("lock")
        .remove(&session("stranger"));
    let fleet = plan_restarts(w.svc()).expect("plans");
    assert_eq!(
        fleet.idle().iter().map(Restart::name).collect::<Vec<_>>(),
        vec![session("idle")]
    );
    assert_eq!(
        fleet
            .would_stop()
            .iter()
            .map(|b| b.plan().name())
            .collect::<Vec<_>>(),
        vec![session("busy")]
    );
    assert_eq!(fleet.own().map(Own::name), Some(session("me")));
    assert_eq!(fleet.unplanned().len(), 0);
}

#[test]
fn a_fleet_stops_exited_agents_processes_and_leaves_them_exited() {
    let w = World::new();
    let with_process = w.ghost("ghost", Lurking::Survivor(SurvivorState::Idle));
    w.exited("quiet");
    let fleet = plan_restarts(w.svc()).expect("plans");
    assert_eq!(
        fleet.idle().len(),
        1,
        "the exited one with no process is skipped"
    );
    assert_eq!(fleet.idle()[0].plan().end(), End::LeftExited);
    let approvals = fleet.approve(StopPolicy::Skip);
    let done = run(&w, approvals.restarts.into_iter().next().expect("one")).expect("stops");
    assert_eq!(done.end, End::LeftExited);
    assert_eq!(
        steps(&w),
        vec![Step::Stop(with_process.cwd().to_path_buf())]
    );
}

#[test]
fn a_fleet_approval_skips_busy_agents_or_forces_them() {
    let w = World::new();
    w.agent("idle", Doing::Idle);
    w.agent("busy", Doing::Working);
    let skipped = plan_restarts(w.svc())
        .expect("plans")
        .approve(StopPolicy::Skip);
    assert_eq!((skipped.restarts.len(), skipped.skipped.len()), (1, 1));
    let forced = plan_restarts(w.svc())
        .expect("plans")
        .approve(StopPolicy::Allow);
    assert_eq!((forced.restarts.len(), forced.skipped.len()), (2, 0));
}

#[test]
fn a_fleets_summary_leaves_out_the_parts_that_are_zero() {
    let w = World::new();
    w.agent("idle", Doing::Idle);
    assert_eq!(
        plan_restarts(w.svc()).expect("plans").summary(),
        "restart 1"
    );
    w.agent("busy", Doing::Working);
    w.ghost("ghost", Lurking::Survivor(SurvivorState::Idle));
    let fleet = plan_restarts(w.svc()).expect("plans");
    assert_eq!(fleet.summary(), "restart 2 · stop 1 exited · 1 busy");
    assert_eq!(fleet.title(), "Restart 2 · stop 1 exited · 1 busy");
    assert_eq!((fleet.starts(), fleet.stops(), fleet.total()), (2, 1, 3));

    let only_stops = World::new();
    only_stops.ghost("ghost", Lurking::Survivor(SurvivorState::Idle));
    assert_eq!(
        plan_restarts(only_stops.svc()).expect("plans").title(),
        "Stop 1 exited"
    );
}

#[test]
fn a_process_open_in_another_terminal_is_refused_before_the_session_is_killed() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    let approved = go(&w, &at);
    lurk(&w, &at, Lurking::Elsewhere);
    assert!(matches!(
        run(&w, approved),
        Err(Error::OpenElsewhere { label: "fake", .. })
    ));
    assert_eq!(steps(&w), vec![], "the session is left running");
    assert!(matches!(
        plan_restart(w.svc(), at),
        Err(Error::OpenElsewhere { .. })
    ));
}

#[test]
fn a_live_agents_process_still_in_its_terminal_says_nothing_about_what_it_interrupts() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    lurk(&w, &at, Lurking::Interactive(SurvivorState::Busy));
    let plan = plan(&w, &at);
    assert_eq!(plan.stops(), None, "the session's activity says it is idle");

    let busy = w.agent("busy", Doing::Working);
    lurk(&w, &busy, Lurking::Interactive(SurvivorState::Busy));
    assert_eq!(
        self::plan(&w, &busy).stops().and_then(Stops::activity),
        Some(Interrupts::Session(Busy::Working)),
        "never working in the background"
    );
}

const BUILD: (&str, &str) = ("b1", "cargo test --workspace");
const SERVER: (&str, &str) = ("b2", "npm run dev");

#[test]
fn an_idle_agent_running_commands_must_be_forced_and_the_commands_are_what_it_proves() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    w.runs_commands("fix", &[BUILD, SERVER]);
    let interrupting = would_stop(&w, &at);
    let stops = interrupting.stops();
    assert_eq!(stops.activity(), None);
    assert_eq!(stops.commands().map(|c| c.count().get()), Some(2));
    assert_eq!(stops.label(), "running 2 background commands");
    run(&w, interrupting.force()).expect("forced");
    assert_eq!(steps(&w).first(), Some(&Step::Kill(session("fix"))));
}

#[test]
fn a_busy_agent_running_commands_names_both() {
    let w = World::new();
    let at = w.agent("fix", Doing::Working);
    w.runs_commands("fix", &[BUILD]);
    let interrupting = would_stop(&w, &at);
    assert_eq!(
        interrupting.stops().label(),
        "working and running 1 background command"
    );
}

#[test]
fn commands_that_appear_between_the_plan_and_the_restart_are_not_dropped() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    let approved = go(&w, &at);
    w.runs_commands("fix", &[BUILD]);
    assert!(matches!(
        run(&w, approved),
        Err(Error::WouldStop { stops, .. }) if stops.commands().is_some() && stops.activity().is_none()
    ));
    assert_eq!(steps(&w), vec![], "not even killed");
}

#[test]
fn an_exited_agent_with_no_process_has_no_commands_to_lose() {
    let w = World::new();
    let at = w.exited("fix");
    w.runs_commands("fix", &[BUILD]);
    assert!(matches!(plan(&w, &at), RestartApproval::Free(_)));
}

#[test]
fn an_exited_agents_process_that_may_hold_commands_needs_forcing() {
    let w = World::new();
    let at = w.ghost("fix", Lurking::Survivor(SurvivorState::Idle));
    w.runs_commands("fix", &[BUILD]);
    let interrupting = would_stop(&w, &at);
    assert_eq!(interrupting.stops().activity(), None);
    assert_eq!(
        interrupting.stops().commands().map(|c| c.count().get()),
        Some(1)
    );
}

#[test]
fn the_callers_own_session_reports_its_commands_too() {
    let mut w = World::new();
    w.own = Some(session("fix"));
    let at = w.agent("fix", Doing::Idle);
    w.runs_commands("fix", &[BUILD]);
    let Planned::Own(own) = plan_restart(w.svc(), at).expect("plans") else {
        panic!("own session")
    };
    assert_eq!(
        own.plan().stops().map(Stops::label).as_deref(),
        Some("running 1 background command")
    );
}

#[test]
fn a_fleet_idle_only_skips_idle_agents_running_commands_and_forcing_restarts_them() {
    let w = World::new();
    w.agent("quiet", Doing::Idle);
    w.agent("serving", Doing::Idle);
    w.runs_commands("serving", &[SERVER]);
    let fleet = plan_restarts(w.svc()).expect("plans");
    assert_eq!(
        fleet
            .would_stop()
            .iter()
            .map(|b| b.plan().name())
            .collect::<Vec<_>>(),
        vec![session("serving")]
    );
    assert_eq!(fleet.summary(), "restart 2 · 1 busy");

    let skipped = fleet.approve(StopPolicy::Skip);
    assert_eq!(
        skipped
            .restarts
            .iter()
            .map(Restart::name)
            .collect::<Vec<_>>(),
        vec![session("quiet")]
    );
    assert_eq!(
        skipped
            .skipped
            .iter()
            .map(|w| (w.plan().name(), w.stops().label()))
            .collect::<Vec<_>>(),
        vec![(
            session("serving"),
            "running 1 background command".to_string()
        )]
    );

    let forced = plan_restarts(w.svc())
        .expect("plans")
        .approve(StopPolicy::Allow);
    assert_eq!(forced.restarts.len(), 2);
}

#[test]
fn a_restart_confirmed_on_the_activity_is_refused_when_commands_appear_after() {
    let w = World::new();
    let at = w.agent("fix", Doing::Working);
    let forced = would_stop(&w, &at).force();
    w.runs_commands("fix", &[BUILD]);
    assert!(matches!(
        run(&w, forced),
        Err(Error::WouldStop { stops, .. }) if stops.commands().is_some()
    ));
    assert_eq!(steps(&w), vec![], "not even killed");
}

#[test]
fn a_restart_confirmed_on_commands_is_refused_when_the_agent_starts_working_after() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    w.runs_commands("fix", &[BUILD]);
    let forced = would_stop(&w, &at).force();
    w.prompts("fix");
    assert!(matches!(
        run(&w, forced),
        Err(Error::WouldStop { stops, .. }) if stops.activity().is_some()
    ));
    assert_eq!(steps(&w), vec![], "not even killed");
}

#[test]
fn a_restart_confirmed_on_commands_goes_ahead_when_they_are_the_same_or_fewer() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    w.runs_commands("fix", &[BUILD, SERVER]);
    let forced = would_stop(&w, &at).force();
    w.runs_commands("fix", &[BUILD]);
    run(&w, forced).expect("what it would stop was all shown");
    assert_eq!(steps(&w).first(), Some(&Step::Kill(session("fix"))));
}

#[test]
fn a_restart_confirmed_on_commands_is_refused_when_a_different_one_appears() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    w.runs_commands("fix", &[BUILD]);
    let forced = would_stop(&w, &at).force();
    w.runs_commands("fix", &[BUILD, SERVER]);
    assert!(matches!(run(&w, forced), Err(Error::WouldStop { .. })));
    assert_eq!(steps(&w), vec![]);
}

#[test]
fn what_an_agent_became_names_its_commands_and_its_activity() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    w.runs_commands("fix", &[BUILD, SERVER]);
    let commands = would_stop(&w, &at);
    assert_eq!(
        commands.stops().became(),
        "started running 2 background commands"
    );

    let busy = w.agent("busy", Doing::Working);
    w.runs_commands("busy", &[BUILD]);
    let both = would_stop(&w, &busy);
    assert_eq!(
        both.stops().became(),
        "started working and started running 1 background command"
    );
}

fn command_ids(stops: &Stops) -> Vec<String> {
    stops
        .commands()
        .map(|commands| commands.iter().map(|c| c.id.as_str().to_string()).collect())
        .unwrap_or_default()
}

#[test]
fn a_skip_after_working_with_a_known_command_names_only_the_new_command() {
    let w = World::new();
    let at = w.agent("fix", Doing::Working);
    w.runs_commands("fix", &[BUILD]);
    let forced = would_stop(&w, &at).force();
    w.runs_commands("fix", &[BUILD, SERVER]);
    let Err(Error::WouldStop { stops, .. }) = run(&w, forced) else {
        panic!("a new command is refused");
    };
    assert_eq!(stops.activity(), None, "it was already working");
    assert_eq!(command_ids(&stops), [SERVER.0], "only the one not shown");
}

#[test]
fn a_skip_after_commands_were_swapped_names_the_new_one_not_the_old() {
    let w = World::new();
    let at = w.agent("fix", Doing::Idle);
    w.runs_commands("fix", &[BUILD]);
    let forced = would_stop(&w, &at).force();
    w.runs_commands("fix", &[SERVER]);
    let Err(Error::WouldStop { stops, .. }) = run(&w, forced) else {
        panic!("a new command is refused");
    };
    assert_eq!(command_ids(&stops), [SERVER.0]);
    assert_eq!(stops.became(), "started running 1 background command");
}

#[test]
fn a_restart_confirmed_on_working_goes_ahead_when_the_agent_is_asking_by_then() {
    let w = World::new();
    let at = w.agent("fix", Doing::Working);
    let forced = would_stop(&w, &at).force();
    w.asks("fix");
    assert_eq!(
        plan(&w, &at).stops().and_then(Stops::activity),
        Some(Interrupts::Session(Busy::Asking)),
        "it really is asking now"
    );
    run(&w, forced).expect("the activity was shown, whatever it became");
    assert_eq!(steps(&w).first(), Some(&Step::Kill(session("fix"))));
}
