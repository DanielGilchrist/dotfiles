use super::*;
use crate::fixtures::{connect as connect_as, elsewhere_on};
use crate::model::Intent;

const SHARED: &str = "obs-layout-4";

fn held_elsewhere_model() -> Model {
    let mut m = elsewhere_on(SHARED);
    set_state(&mut m, SHARED, done(), 1);
    m
}

fn nav() -> Msg {
    Msg::Key(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL))
}

fn previewed(m: &mut Model) -> Vec<Effect> {
    m.schedule_preview();
    let due = later(m, crate::model::PREVIEW_DEBOUNCE);
    tick_at(m, due)
}

fn closed_windows_of(effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Job(Job::CloseElsewhere { name, .. }) => Some(name.to_string()),
            _ => None,
        })
        .collect()
}

fn closes_of(work: &[Background]) -> Vec<String> {
    work.iter()
        .filter_map(|w| match w {
            Background::Job(Job::CloseElsewhere { name, .. }) => Some(name.to_string()),
            _ => None,
        })
        .collect()
}

fn looks_of(work: &[Background]) -> Vec<String> {
    work.iter()
        .filter_map(|w| match w {
            Background::Look(look) => Some(look.name.to_string()),
            Background::Job(_) => None,
        })
        .collect()
}

fn attaches(effects: &[Effect]) -> Vec<(Intent, Company)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Attach {
                intent, company, ..
            } => Some((*intent, *company)),
            _ => None,
        })
        .collect()
}

fn stays_held_elsewhere(m: &Model, effects: &[Effect], agent: &str) {
    assert_eq!(attaches(effects), []);
    assert_eq!(closed_windows_of(effects), Vec::<String>::new());
    assert_eq!(m.pane.held_elsewhere(), Some(&name(agent)));
    assert!(m.pane.connection().is_none());
    assert_eq!(
        m.cursor().and_then(Cursor::entry_name),
        Some(&name(agent)),
        "the banner is under the row it is about"
    );
}

#[test]
fn a_hovered_agent_is_checked_for_other_windows_before_anything_attaches() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    let effects = previewed(&mut m);
    assert_eq!(checked_sessions(&effects), [SHARED]);
    assert_eq!(attaches(&effects), []);
    assert!(matches!(m.pane, Pane::Checking { .. }));
}

#[test]
fn the_preview_attaches_once_the_check_finds_no_other_window() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    previewed(&mut m);
    let effects = answer(&mut m, SHARED, Company::Alone);
    assert_eq!(attaches(&effects), [(Intent::Preview, Company::Alone)]);
}

#[test]
fn a_preview_of_an_agent_open_elsewhere_leaves_it_alone_instead_of_attaching() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    previewed(&mut m);
    let effects = answer(&mut m, SHARED, Company::Elsewhere);
    stays_held_elsewhere(&m, &effects, SHARED);
    assert_eq!(m.moves_here(), Some(name(SHARED)));
}

#[test]
fn the_pane_attaches_again_once_the_other_window_is_gone() {
    let mut m = held_elsewhere_model();
    assert!(recheck_elsewhere(&mut m).is_some());
    let still = answer(&mut m, SHARED, Company::Elsewhere);
    stays_held_elsewhere(&m, &still, SHARED);
    recheck_elsewhere(&mut m);
    let effects = answer(&mut m, SHARED, Company::Alone);
    assert_eq!(attaches(&effects), [(Intent::Preview, Company::Alone)]);
    assert_eq!(closed_windows_of(&effects), Vec::<String>::new());
}

#[test]
fn a_late_answer_about_another_session_changes_nothing() {
    let mut m = held_elsewhere_model();
    recheck_elsewhere(&mut m);
    let effects = answer(&mut m, "auth-refresh-2", Company::Alone);
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn an_answer_to_an_older_look_is_dropped() {
    let mut m = held_elsewhere_model();
    recheck_elsewhere(&mut m);
    let old = m.current_look().expect("asked");
    recheck_elsewhere(&mut m);
    let effects = update(
        &mut m,
        Msg::Elsewhere {
            name: name(SHARED),
            ticket: old,
            company: Ok(Company::Alone),
        },
    );
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn esc_goes_nowhere_on_an_agent_held_elsewhere() {
    let mut m = held_elsewhere_model();
    let effects = update(&mut m, key(KeyCode::Esc));
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn the_switcher_never_moves_an_agent_held_elsewhere() {
    let mut m = held_elsewhere_model();
    update(&mut m, key(KeyCode::Char(' ')));
    type_text(&mut m, SHARED);
    let effects = update(&mut m, key(KeyCode::Enter));
    stays_held_elsewhere(&m, &effects, SHARED);
    assert!(
        m.status
            .as_ref()
            .is_some_and(|s| s.text.contains("elsewhere"))
    );
}

#[test]
fn the_switcher_on_an_agent_found_elsewhere_selects_it_and_leaves_it_alone() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
    m.pane = Pane::Empty;
    update(&mut m, key(KeyCode::Char(' ')));
    type_text(&mut m, SHARED);
    let asked = update(&mut m, key(KeyCode::Enter));
    assert_eq!(checked_sessions(&asked), [SHARED]);
    let effects = answer(&mut m, SHARED, Company::Elsewhere);
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn the_a_key_on_an_agent_found_elsewhere_selects_it_and_leaves_it_alone() {
    let mut m = crate::fixtures::fleet(10);
    let urgent = m.needs_you()[0].clone();
    let other = m
        .snapshot()
        .expect("loaded")
        .entries()
        .map(|(_, e)| e.name().clone())
        .find(|n| *n != urgent && m.attachable(n))
        .expect("another live agent");
    m.set_cursor(Cursor::Entry(other));
    m.pane = Pane::Empty;
    let asked = update(&mut m, key(KeyCode::Char('a')));
    assert_eq!(checked_sessions(&asked), [urgent.as_str()]);
    let effects = answer(&mut m, urgent.as_str(), Company::Elsewhere);
    stays_held_elsewhere(&m, &effects, urgent.as_str());
}

#[test]
fn the_a_key_never_moves_an_agent_held_elsewhere() {
    let mut m = held_elsewhere_model();
    let urgent = m.needs_you()[0].clone();
    m.set_cursor(Cursor::Entry(urgent.clone()));
    m.pane = Pane::Elsewhere {
        name: urgent.clone(),
    };
    let effects = update(&mut m, key(KeyCode::Char('a')));
    stays_held_elsewhere(&m, &effects, urgent.as_str());
}

#[test]
fn the_nav_key_twice_toward_an_agent_open_elsewhere_lands_in_nav_and_leaves_it_to_its_other_window()
{
    let mut m = crate::fixtures::fleet(10);
    connect(&mut m, "auth-refresh-2");
    m.touch_mru(&name(SHARED));
    m.touch_mru(&name("auth-refresh-2"));
    m.chord = Chord::Armed;
    let effects = update(&mut m, nav());
    assert_eq!(checked_sessions(&effects), [SHARED]);
    let effects = answer(&mut m, SHARED, Company::Elsewhere);
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn i_from_another_panel_never_moves_an_agent_held_elsewhere() {
    let mut m = held_elsewhere_model();
    update(&mut m, key(KeyCode::Char('2')));
    assert!(matches!(m.panel, Panel::Agent { .. }));
    let effects = update(&mut m, key(KeyCode::Char('i')));
    stays_held_elsewhere(&m, &effects, SHARED);
}

#[test]
fn enter_zero_and_i_on_the_banner_row_attach_first_and_close_nothing_until_connected() {
    for code in [KeyCode::Enter, KeyCode::Char('0'), KeyCode::Char('i')] {
        let mut m = held_elsewhere_model();
        let effects = update(&mut m, key(code));
        assert_eq!(
            attaches(&effects),
            [(Intent::MoveHere, Company::Alone)],
            "{code:?}"
        );
        assert_eq!(closed_windows_of(&effects), Vec::<String>::new());
        assert!(matches!(m.pane, Pane::Connecting { .. }));
    }
}

#[test]
fn the_other_window_closes_only_after_the_connection_exists() {
    let mut m = held_elsewhere_model();
    update(&mut m, key(KeyCode::Enter));
    assert!(m.pane.connection().is_none(), "a failed attach ends here");
    connect(&mut m, SHARED);
    let effects = connected(&mut m, Intent::MoveHere);
    assert_eq!(closes_of(&effects), [SHARED]);
    assert!(matches!(m.focus(), Focus::Agent(_)));
}

#[test]
fn a_closed_other_window_leaves_the_connection_alone_and_a_failed_close_marks_it_shared() {
    let mut m = elsewhere_on(SHARED);
    update(&mut m, key(KeyCode::Enter));
    connect(&mut m, SHARED);
    connected(&mut m, Intent::MoveHere);
    let company = |m: &Model| m.pane.connection().map(|c| c.company);
    let closed = |m: &mut Model, closed| {
        update(
            m,
            Msg::ClosedElsewhere {
                name: name(SHARED),
                closed,
            },
        )
    };
    let in_flight = m.current_look();
    let effects = closed(&mut m, Ok(1));
    assert_eq!(company(&m), Some(Company::Alone));
    assert_eq!(checked_sessions(&effects), [SHARED], "and it looks again");
    assert_ne!(m.current_look(), in_flight);
    assert!(
        m.status
            .as_ref()
            .is_some_and(|s| s.text.contains("closed 1"))
    );
    let effects = closed(&mut m, Err("nope".into()));
    assert_eq!(company(&m), Some(Company::Elsewhere));
    assert_eq!(checked_sessions(&effects), [SHARED]);
}

#[test]
fn a_move_here_on_a_session_that_exited_resurrects_it_instead_of_moving() {
    let mut m = crate::fixtures::fleet(10);
    let exited = m
        .snapshot()
        .expect("loaded")
        .entries()
        .find(|(_, e)| {
            matches!(
                m.reach(e),
                Reach::Other {
                    enter: Enter::Resurrect | Enter::Restart(_),
                    ..
                }
            )
        })
        .map(|(_, e)| e.name().clone())
        .expect("an exited agent");
    m.set_cursor(Cursor::Entry(exited.clone()));
    m.pane = Pane::Elsewhere {
        name: exited.clone(),
    };
    let effects = update(&mut m, key(KeyCode::Enter));
    assert!(
        attaches(&effects)
            .iter()
            .all(|(intent, _)| *intent != Intent::MoveHere)
    );
    assert!(m.pane.held_elsewhere().is_none());
}

#[test]
fn moving_here_leaves_a_look_already_on_its_way_valid() {
    let mut m = elsewhere_on(SHARED);
    update(&mut m, key(KeyCode::Enter));
    connect(&mut m, SHARED);
    let before = m.current_look();
    connected(&mut m, Intent::MoveHere);
    assert_eq!(m.current_look(), before);
}

#[test]
fn a_plain_visit_connects_without_closing_anything() {
    let mut m = crate::fixtures::fleet(10);
    connect(&mut m, SHARED);
    let effects = connected(&mut m, Intent::Activate);
    assert_eq!(closes_of(&effects), Vec::<String>::new());
    assert_eq!(looks_of(&effects), [SHARED], "and watches for newcomers");
}

#[test]
fn a_window_that_opens_while_typing_is_left_alone_until_the_keys_leave() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    connect(&mut m, SHARED);
    update(&mut m, key(KeyCode::Enter));
    assert!(matches!(m.focus(), Focus::Agent(_)));
    assert!(recheck_elsewhere(&mut m).is_some());
    answer(&mut m, SHARED, Company::Elsewhere);
    assert!(matches!(m.focus(), Focus::Agent(_)), "still typing");
    assert!(m.pane.squished(&name(SHARED)));
    update(&mut m, nav());
    assert_eq!(
        m.pane.held_elsewhere(),
        Some(&name(SHARED)),
        "its client is dropped"
    );
}

#[test]
fn a_window_that_opens_beside_a_preview_leaves_it_alone() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    connect(&mut m, SHARED);
    recheck_elsewhere(&mut m);
    answer(&mut m, SHARED, Company::Elsewhere);
    assert_eq!(m.pane.held_elsewhere(), Some(&name(SHARED)));
}

#[test]
fn peeking_an_agent_open_elsewhere_holds_a_client_only_while_the_float_is_open() {
    let mut m = held_elsewhere_model();
    let effects = update(&mut m, key(KeyCode::Char('v')));
    assert_eq!(attaches(&effects), [(Intent::Preview, Company::Elsewhere)]);
    assert_eq!(m.peek, Some(name(SHARED)));
    connect_as(&mut m, SHARED, Mode::Nav, Company::Elsewhere);
    assert!(m.pane.connection().is_some(), "the float has its client");
    update(&mut m, key(KeyCode::Char('v')));
    assert_eq!(
        m.pane.held_elsewhere(),
        Some(&name(SHARED)),
        "detached with the float"
    );
}

#[test]
fn enter_in_a_peek_before_any_answer_never_enters_agent_on_an_agent_open_elsewhere() {
    for code in [KeyCode::Enter, KeyCode::Char('i')] {
        let mut m = held_elsewhere_model();
        let peeked = update(&mut m, key(KeyCode::Char('v')));
        let [(_, company)] = attaches(&peeked)[..] else {
            panic!("one attach");
        };
        connect_as(&mut m, SHARED, Mode::Nav, company);
        let effects = update(&mut m, key(code));
        stays_held_elsewhere(&m, &effects, SHARED);
        assert!(!m.peeking(), "{code:?}");
    }
}

#[test]
fn s_sends_to_an_agent_left_alone_without_a_client() {
    let mut m = held_elsewhere_model();
    update(&mut m, key(KeyCode::Char('s')));
    assert!(matches!(m.overlay, Overlay::Talk { .. }));
    type_text(&mut m, "hi");
    let effects = update(&mut m, key(KeyCode::Enter));
    assert_eq!(replies(&effects), ["hi"]);
    assert!(m.pane.connection().is_none());
}

#[test]
fn the_footer_says_move_here_on_the_banner_row_and_attach_otherwise() {
    let enter = |m: &Model| {
        crate::keymap::footer(&Situation::of(m), "⌃\\")
            .into_iter()
            .find(|h| h.keys == "⏎")
            .map(|h| h.label)
    };
    let mut m = held_elsewhere_model();
    assert_eq!(enter(&m), Some("move here"));
    m.pane = Pane::Empty;
    assert_eq!(enter(&m), Some("attach"));
}

#[test]
fn nothing_is_asked_while_idle_checking_or_connecting() {
    let mut m = crate::fixtures::fleet(10);
    assert!(recheck_elsewhere(&mut m).is_none());
    m.set_cursor(Cursor::Entry(name(SHARED)));
    previewed(&mut m);
    assert!(recheck_elsewhere(&mut m).is_none());
}

#[test]
fn a_failed_look_keeps_the_pane_and_says_so_once() {
    let mut m = held_elsewhere_model();
    let fail = |m: &mut Model| {
        recheck_elsewhere(m);
        let ticket = m.current_look().expect("asked");
        update(
            m,
            Msg::Elsewhere {
                name: name(SHARED),
                ticket,
                company: Err("ps failed".into()),
            },
        )
    };
    let effects = fail(&mut m);
    stays_held_elsewhere(&m, &effects, SHARED);
    assert!(
        m.status
            .as_ref()
            .is_some_and(|s| s.kind == StatusKind::Error && s.text.contains("ps failed"))
    );
    m.status = None;
    fail(&mut m);
    assert!(m.status.is_none(), "not again");
    recheck_elsewhere(&mut m);
    answer(&mut m, SHARED, Company::Elsewhere);
    assert_eq!(m.check_health, crate::model::CheckHealth::Working);
}

#[test]
fn a_first_look_that_fails_attaches_anyway_and_says_so() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    previewed(&mut m);
    let ticket = m.current_look().expect("asked");
    let effects = update(
        &mut m,
        Msg::Elsewhere {
            name: name(SHARED),
            ticket,
            company: Err("ps failed".into()),
        },
    );
    assert_eq!(attaches(&effects), [(Intent::Preview, Company::Alone)]);
    assert!(
        m.status
            .as_ref()
            .is_some_and(|s| s.kind == StatusKind::Error)
    );
}

#[test]
fn a_look_that_takes_too_long_attaches_without_it_and_says_nothing() {
    let mut m = crate::fixtures::fleet(10);
    m.set_cursor(Cursor::Entry(name(SHARED)));
    previewed(&mut m);
    let early = later(&m, CHECK_TIMEOUT.saturating_sub(Duration::from_millis(1)));
    assert_eq!(attaches(&tick_at(&mut m, early)), []);
    let due = later(&m, Duration::from_millis(2));
    let effects = tick_at(&mut m, due);
    assert_eq!(attaches(&effects), [(Intent::Preview, Company::Alone)]);
    assert!(m.status.is_none());
    assert_eq!(m.check_health, crate::model::CheckHealth::Working);
}
