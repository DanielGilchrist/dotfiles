use super::*;
use crate::fixtures::{fleet, in_nvim, restart_approval, restart_fleet};
use crate::model::{AgentStatus, Finish, Planning, Reach, StatusKind, Unsent};
use agent_core::activity::Busy;
use agent_core::ops::{End, Interrupts, Stops};
use agent_core::ports::SurvivorState;
use agent_core::ports::{SessionInfo, SessionStatus};
use agent_core::testing::{Doing, Lurking, World};

const AGENT: &str = "auth-refresh-2";

fn selected() -> Model {
    let mut m = fleet(10);
    m.set_cursor(Cursor::Entry(name(AGENT)));
    m
}

fn press(m: &mut Model, c: char) -> Vec<Effect> {
    update(m, key(KeyCode::Char(c)))
}

fn planning_ticket(m: &Model, wanted: &Planning) -> Ticket {
    match &m.overlay {
        Overlay::Planning { ticket, planning } if planning == wanted => *ticket,
        _ => panic!("not planning {wanted:?}"),
    }
}

fn restarts_in(effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Restart(restarts) => Some(restarts.iter().map(|r| r.name().to_string())),
            _ => None,
        })
        .flatten()
        .collect()
}

fn planned(m: &mut Model, fakes: &World, doing: Doing) -> Vec<Effect> {
    let ticket = planning_ticket(m, &Planning::Restart(name(AGENT)));
    update(
        m,
        Msg::RestartPlanned {
            ticket,
            result: Ok(restart_approval(fakes, AGENT, doing)),
        },
    )
}

fn restarted(m: &mut Model, who: &str, finish: Finish) -> Vec<Effect> {
    update(
        m,
        Msg::Restarted {
            name: name(who),
            finish,
        },
    )
}

fn done(end: End) -> Finish {
    Finish::Restarted { end, label: "fake" }
}

#[test]
fn r_plans_a_restart_and_an_idle_agent_goes_ahead_with_no_question() {
    let mut m = selected();
    let effects = press(&mut m, 'r');
    assert!(matches!(
        effects.as_slice(),
        [Effect::Job(Job::PlanRestart { .. })]
    ));
    let fakes = World::new();
    let effects = planned(&mut m, &fakes, Doing::Idle);
    assert_eq!(restarts_in(&effects), [AGENT]);
    assert!(matches!(m.overlay, Overlay::None), "no confirmation");
    let entry = m.selected_entry().expect("selected").clone();
    assert_eq!(m.status(&entry), AgentStatus::Restarting);
    assert_eq!(
        m.shown_status().map(|s| s.0),
        Some(format!("restarting {AGENT}…"))
    );
}

#[test]
fn a_busy_agent_is_confirmed_and_n_leaves_it_alone() {
    let mut m = selected();
    press(&mut m, 'r');
    let fakes = World::new();
    assert!(planned(&mut m, &fakes, Doing::Working).is_empty());
    assert!(matches!(
        &m.overlay,
        Overlay::Confirm(Confirm::Restart(ask)) if ask.stops().and_then(Stops::activity) == Some(Interrupts::Session(Busy::Working))
    ));
    assert!(update(&mut m, key(KeyCode::Char('x'))).is_empty());
    assert!(
        matches!(m.overlay, Overlay::Confirm(Confirm::Restart(_))),
        "other keys keep it"
    );
    assert!(press(&mut m, 'n').is_empty());
    assert!(matches!(m.overlay, Overlay::None));
    assert!(m.restarts.is_none());
}

#[test]
fn y_restarts_a_busy_agent_and_drops_the_pane_that_showed_it() {
    let mut m = selected();
    crate::fixtures::connect(&mut m, AGENT, Mode::Nav);
    press(&mut m, 'r');
    planned(&mut m, &World::new(), Doing::Working);
    let effects = press(&mut m, 'y');
    assert_eq!(restarts_in(&effects), [AGENT]);
    assert!(
        m.pane.session().is_none(),
        "the old client is dropped before its session is killed"
    );
    assert!(m.restarting(&name(AGENT)));
}

#[test]
fn an_idle_agent_another_window_shows_is_still_confirmed() {
    let mut m = selected();
    m.record_company(&name(AGENT), in_nvim());
    press(&mut m, 'r');
    assert!(planned(&mut m, &World::new(), Doing::Idle).is_empty());
    let Overlay::Confirm(Confirm::Restart(ask)) = &m.overlay else {
        panic!("a confirmation");
    };
    assert!(ask.stops().is_none() && ask.windows.is_some());
    assert_eq!(restarts_in(&press(&mut m, 'y')), [AGENT]);
}

#[test]
fn a_plan_answering_a_cancelled_or_replaced_request_is_ignored() {
    let mut m = selected();
    press(&mut m, 'r');
    let cancelled = planning_ticket(&m, &Planning::Restart(name(AGENT)));
    update(&mut m, key(KeyCode::Esc));
    let late = update(
        &mut m,
        Msg::RestartPlanned {
            ticket: cancelled,
            result: Ok(restart_approval(&World::new(), AGENT, Doing::Idle)),
        },
    );
    assert!(late.is_empty());
    assert!(m.restarts.is_none() && matches!(m.overlay, Overlay::None));

    press(&mut m, 'r');
    let replaced = planning_ticket(&m, &Planning::Restart(name(AGENT)));
    assert_ne!(replaced, cancelled);
    let stale = update(
        &mut m,
        Msg::RestartPlanned {
            ticket: cancelled,
            result: Ok(restart_approval(&World::new(), AGENT, Doing::Idle)),
        },
    );
    assert!(stale.is_empty() && m.restarts.is_none());
    assert!(
        matches!(m.overlay, Overlay::Planning { ticket, .. } if ticket == replaced),
        "still waiting for its own answer"
    );
}

#[test]
fn a_failed_plan_is_an_error_and_the_own_session_and_idle_rows_are_refused() {
    let mut m = selected();
    press(&mut m, 'r');
    let ticket = planning_ticket(&m, &Planning::Restart(name(AGENT)));
    update(
        &mut m,
        Msg::RestartPlanned {
            ticket,
            result: Err(Cause::message("no agent record for x".into())),
        },
    );
    assert_eq!(m.status.as_ref().map(|s| s.kind), Some(StatusKind::Error));

    let mut own = selected();
    own.own_session = Some(name(AGENT));
    assert!(press(&mut own, 'r').is_empty());
    assert!(matches!(own.overlay, Overlay::None));
    assert!(
        own.status
            .as_ref()
            .expect("says why")
            .text
            .contains("this TUI runs in it")
    );
}

#[test]
fn an_entry_with_no_session_has_nothing_to_restart() {
    let mut m = fleet(10);
    let idle = m
        .snapshot()
        .expect("loaded")
        .entries()
        .find(|(_, e)| e.presence().is_none())
        .map(|(_, e)| e.name().clone())
        .expect("an entry with no session");
    m.set_cursor(Cursor::Entry(idle.clone()));
    assert!(press(&mut m, 'r').is_empty());
    assert!(matches!(m.overlay, Overlay::None));
    assert_eq!(
        m.status.as_ref().map(|s| s.text.clone()),
        Some(format!("{idle} has no session to restart"))
    );
}

#[test]
fn capital_r_lists_what_it_would_interrupt_and_y_restarts_everything() {
    let mut m = selected();
    let effects = press(&mut m, 'R');
    assert!(matches!(
        effects.as_slice(),
        [Effect::Job(Job::PlanRestarts { .. })]
    ));
    let fakes = World::new();
    fakes.agent("idle-one", Doing::Idle);
    fakes.agent("busy-one", Doing::Working);
    fakes.ghost("ghost-one", Lurking::Survivor(SurvivorState::Idle));
    let ticket = planning_ticket(&m, &Planning::Restarts);
    update(
        &mut m,
        Msg::FleetPlanned {
            ticket,
            result: Ok(restart_fleet(&fakes)),
        },
    );
    let Overlay::Confirm(Confirm::Restarts(ask)) = &m.overlay else {
        panic!("a confirmation");
    };
    assert_eq!(
        (ask.fleet.starts(), ask.fleet.stops(), ask.fleet.total()),
        (2, 1, 3)
    );
    let effects = press(&mut m, 'y');
    let mut names = restarts_in(&effects);
    names.sort();
    assert_eq!(names, ["busy-one", "ghost-one", "idle-one"]);
}

#[test]
fn i_restarts_only_the_idle_agents_and_is_not_offered_when_none_is_busy() {
    let fleet_of = |busy: bool| {
        let fakes = World::new();
        fakes.agent("idle-one", Doing::Idle);
        if busy {
            fakes.agent("busy-one", Doing::Working);
        }
        restart_fleet(&fakes)
    };
    let ask = |m: &mut Model, fleet| {
        press(m, 'R');
        let ticket = planning_ticket(m, &Planning::Restarts);
        update(
            m,
            Msg::FleetPlanned {
                ticket,
                result: Ok(fleet),
            },
        );
    };
    let mut m = selected();
    ask(&mut m, fleet_of(true));
    assert_eq!(restarts_in(&press(&mut m, 'i')), ["idle-one"]);

    let mut calm = selected();
    ask(&mut calm, fleet_of(false));
    assert!(press(&mut calm, 'i').is_empty());
    assert!(
        matches!(calm.overlay, Overlay::Confirm(Confirm::Restarts(_))),
        "i isn't offered"
    );
    assert!(press(&mut calm, 'n').is_empty());
    assert!(calm.restarts.is_none());
}

#[test]
fn an_empty_fleet_says_there_is_nothing_to_restart() {
    let mut m = selected();
    press(&mut m, 'R');
    let ticket = planning_ticket(&m, &Planning::Restarts);
    update(
        &mut m,
        Msg::FleetPlanned {
            ticket,
            result: Ok(restart_fleet(&World::new())),
        },
    );
    assert!(matches!(m.overlay, Overlay::None));
    assert_eq!(
        m.status.as_ref().map(|s| s.text.as_str()),
        Some("nothing to restart")
    );
}

#[test]
fn progress_is_read_from_the_batch_and_the_summary_replaces_it_when_the_last_answers() {
    let mut m = selected();
    let fakes = World::new();
    fakes.agent("one", Doing::Idle);
    fakes.agent("two", Doing::Idle);
    press(&mut m, 'R');
    let ticket = planning_ticket(&m, &Planning::Restarts);
    update(
        &mut m,
        Msg::FleetPlanned {
            ticket,
            result: Ok(restart_fleet(&fakes)),
        },
    );
    press(&mut m, 'y');
    assert_eq!(
        m.shown_status().map(|s| s.0),
        Some("restarting 1 of 2…".into())
    );
    restarted(&mut m, "one", done(End::Started));
    assert_eq!(
        m.shown_status().map(|s| s.0),
        Some("restarting 2 of 2…".into())
    );
    assert!(m.restarting(&name("two")) && !m.restarting(&name("one")));
    restarted(&mut m, "two", done(End::Started));
    assert!(m.restarts.is_none());
    assert_eq!(
        m.status.as_ref().map(|s| s.text.as_str()),
        Some("restarted 2")
    );
}

#[test]
fn failures_and_skips_are_in_the_summary_and_a_failure_makes_it_an_error() {
    let mut m = selected();
    let fakes = World::new();
    for agent in ["one", "two", "three"] {
        fakes.agent(agent, Doing::Idle);
    }
    press(&mut m, 'R');
    let ticket = planning_ticket(&m, &Planning::Restarts);
    update(
        &mut m,
        Msg::FleetPlanned {
            ticket,
            result: Ok(restart_fleet(&fakes)),
        },
    );
    press(&mut m, 'y');
    restarted(&mut m, "one", done(End::Started));
    restarted(
        &mut m,
        "two",
        Finish::Skipped(Stops::Activity(Interrupts::Session(Busy::Working))),
    );
    restarted(
        &mut m,
        "three",
        Finish::Failed(Cause::message("fake is still running".into())),
    );
    let status = m.status.as_ref().expect("a summary");
    assert_eq!(status.kind, StatusKind::Error);
    assert_eq!(
        status.text,
        "restarted 1 · 1 failed · skipped two: started working · three: fake is still running"
    );
}

#[test]
fn one_restart_says_what_happened_to_it() {
    let said = |end| {
        let mut m = selected();
        press(&mut m, 'r');
        planned(&mut m, &World::new(), Doing::Idle);
        restarted(&mut m, AGENT, done(end));
        m.status.as_ref().expect("a summary").text.clone()
    };
    assert_eq!(said(End::Started), format!("restarted {AGENT}"));
    assert_eq!(
        said(End::LeftExited),
        format!("stopped fake for {AGENT} · stays exited")
    );
}

#[test]
fn a_restarted_agent_that_is_selected_is_previewed_and_another_is_not() {
    let mut m = selected();
    press(&mut m, 'r');
    planned(&mut m, &World::new(), Doing::Idle);
    m.preview_at = None;
    restarted(&mut m, AGENT, done(End::Started));
    assert!(m.preview_at.is_some(), "NAV previews it once it shows");

    let mut elsewhere = selected();
    press(&mut elsewhere, 'r');
    planned(&mut elsewhere, &World::new(), Doing::Idle);
    let other = name("rate-limit-1");
    elsewhere
        .restarts
        .as_mut()
        .expect("batch")
        .add([other.clone()]);
    elsewhere.set_cursor(Cursor::Entry(other.clone()));
    elsewhere.preview_at = None;
    restarted(&mut elsewhere, AGENT, done(End::Started));
    assert!(elsewhere.preview_at.is_none());
}

#[test]
fn a_restarting_agent_is_not_attached_to_and_a_second_restart_is_refused() {
    let mut m = selected();
    press(&mut m, 'r');
    planned(&mut m, &World::new(), Doing::Idle);
    assert!(update(&mut m, key(KeyCode::Enter)).is_empty());
    assert!(
        m.status
            .as_ref()
            .expect("says why")
            .text
            .contains("is restarting")
    );
    assert!(press(&mut m, 'r').is_empty());
    assert!(
        m.status
            .as_ref()
            .expect("says why")
            .text
            .contains("already restarting")
    );
    m.schedule_preview();
    let due = later(&m, crate::model::PREVIEW_DEBOUNCE);
    let effects = tick_at(&mut m, due);
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::CheckElsewhere(_))),
        "the dying session isn't previewed"
    );
}

#[test]
fn quitting_while_restarts_are_queued_asks_and_one_in_flight_does_not() {
    let mut m = selected();
    let fakes = World::new();
    fakes.agent("one", Doing::Idle);
    fakes.agent("two", Doing::Idle);
    press(&mut m, 'R');
    let ticket = planning_ticket(&m, &Planning::Restarts);
    update(
        &mut m,
        Msg::FleetPlanned {
            ticket,
            result: Ok(restart_fleet(&fakes)),
        },
    );
    press(&mut m, 'y');
    assert_eq!(
        m.unsent(),
        Unsent {
            comments: 0,
            messages: 0,
            restarts: 1
        }
    );
    update(&mut m, key(KeyCode::Char('q')));
    assert!(matches!(
        m.overlay,
        Overlay::Confirm(Confirm::Quit(Unsent { restarts: 1, .. }))
    ));
    update(&mut m, key(KeyCode::Esc));
    restarted(&mut m, "one", done(End::Started));
    assert!(matches!(
        update(&mut m, key(KeyCode::Char('q'))).as_slice(),
        [Effect::Quit]
    ));
}

#[test]
fn a_skipped_or_failed_restart_still_previews_the_selected_agent() {
    for finish in [
        Finish::Skipped(Stops::Activity(Interrupts::Session(Busy::Working))),
        Finish::Failed(Cause::message("still running".into())),
    ] {
        let mut m = selected();
        press(&mut m, 'r');
        planned(&mut m, &World::new(), Doing::Idle);
        m.preview_at = None;
        restarted(&mut m, AGENT, finish);
        assert!(m.preview_at.is_some());
    }
}

#[test]
fn nothing_acts_on_a_restarting_agent_through_any_key() {
    let mut m = selected();
    set_state(&mut m, AGENT, working(), 1);
    press(&mut m, 'r');
    planned(&mut m, &World::new(), Doing::Idle);
    for c in ['d', 'D', 's', 'x', 'v'] {
        let effects = press(&mut m, c);
        assert_eq!(effects.len(), 0, "{c}");
        assert!(
            matches!(m.overlay, Overlay::None),
            "{c} opened {:?}",
            m.panel
        );
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("is restarting")),
            "{c}: {:?}",
            m.status.as_ref().map(|s| s.text.clone())
        );
        m.status = None;
    }
    let entry = m.selected_entry().expect("selected").clone();
    assert!(matches!(m.reach(&entry), Reach::Restarting));
}

#[test]
fn a_session_agent_did_not_start_says_so_and_plans_nothing() {
    let mut m = two_live();
    update(
        &mut m,
        Msg::Snapshot(Ok((
            snapshot(
                vec![SessionInfo {
                    name: name("stray"),
                    status: SessionStatus::Live {
                        cwd: Some("/nowhere/stray".into()),
                    },
                }],
                &[],
            ),
            Vec::new(),
        ))),
    );
    m.set_cursor(Cursor::Entry(name("stray")));
    assert!(press(&mut m, 'r').is_empty());
    assert_eq!(
        m.status.as_ref().map(|s| s.text.as_str()),
        Some("can't restart stray: agent didn't start it")
    );
}

fn plan_with_commands(m: &mut Model, commands: &[(&str, &str)]) -> Vec<Effect> {
    let fakes = World::new();
    let at = fakes.agent(AGENT, Doing::Idle);
    fakes.runs_commands(AGENT, commands);
    let plan = agent_core::ops::plan_restart(fakes.svc(), at)
        .expect("plans")
        .elsewhere()
        .expect("not own");
    let ticket = planning_ticket(m, &Planning::Restart(name(AGENT)));
    update(
        m,
        Msg::RestartPlanned {
            ticket,
            result: Ok(plan),
        },
    )
}

#[test]
fn an_idle_agent_running_commands_is_confirmed_instead_of_restarted_at_once() {
    let mut m = selected();
    press(&mut m, 'r');
    assert!(plan_with_commands(&mut m, &[("b1", "cargo test")]).is_empty());
    let Overlay::Confirm(Confirm::Restart(ask)) = &m.overlay else {
        panic!("a confirmation");
    };
    assert_eq!(ask.stops().and_then(Stops::activity), None);
    assert_eq!(
        ask.stops()
            .and_then(Stops::commands)
            .map(|c| c.count().get()),
        Some(1)
    );
    assert!(
        m.restarts.is_none(),
        "nothing restarts before the user says"
    );
    assert_eq!(restarts_in(&press(&mut m, 'y')), [AGENT]);
}

#[test]
fn a_fleet_idle_only_leaves_out_an_idle_agent_running_commands() {
    let mut m = selected();
    let fakes = World::new();
    fakes.agent("quiet", Doing::Idle);
    fakes.agent("serving", Doing::Idle);
    fakes.runs_commands("serving", &[("b1", "npm run dev")]);
    press(&mut m, 'R');
    let ticket = planning_ticket(&m, &Planning::Restarts);
    update(
        &mut m,
        Msg::FleetPlanned {
            ticket,
            result: Ok(restart_fleet(&fakes)),
        },
    );
    assert_eq!(restarts_in(&press(&mut m, 'i')), ["quiet"]);
}

#[test]
fn a_restart_skipped_for_a_new_command_says_which_one() {
    let fakes = World::new();
    let at = fakes.agent("two", Doing::Idle);
    fakes.runs_commands("two", &[("b1", "npm run dev")]);
    let stops = agent_core::ops::plan_restart(fakes.svc(), at)
        .expect("plans")
        .elsewhere()
        .expect("not own")
        .stops()
        .cloned()
        .expect("it would stop its command");
    let mut batch = crate::model::Batch::default();
    batch.add([name("two")]);
    batch.answer(&name("two"), Finish::Skipped(stops));
    let m = selected();
    assert_eq!(
        batch.summary(m.clock.mono).text,
        "skipped two: started running 1 background command: npm run dev"
    );
}
