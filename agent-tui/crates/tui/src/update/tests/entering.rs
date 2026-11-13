use super::*;
use crate::fixtures::{attach_landings, connect, fleet, sighted};
use crate::model::{CHORD_WINDOW, Landing, Mode, Seat};
use agent_core::ops::{Delivery, Launch};
use agent_core::process::LaunchSpec;

const SHOWN: &str = "auth-refresh-2";
const OTHER: &str = "obs-layout-4";
const ENTER_FROM_LIST: Landing = Landing::Enter { from: Seat::Agents };

fn arrives(m: &Model, ticket: Ticket) -> Landing {
    m.arrival(ticket).landing()
}

fn nav() -> Msg {
    Msg::Key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL))
}

fn click() -> Msg {
    Msg::Mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: 50,
        row: 5,
        modifiers: KeyModifiers::NONE,
    })
}

fn in_agent(m: &Model) -> bool {
    matches!(m.focus(), Focus::Agent(_))
}

fn shown_in_nav(agent: &str) -> Model {
    let mut m = fleet(10);
    m.set_cursor(Cursor::Entry(name(agent)));
    connect(&mut m, agent, Mode::Nav);
    m.areas.terminal = ratatui::layout::Rect::new(40, 2, 80, 24);
    m
}

fn unconnected(agent: &str) -> Model {
    let mut m = fleet(10);
    m.set_cursor(Cursor::Entry(name(agent)));
    m.pane = Pane::Empty;
    m
}

fn launch_of(agent: &str) -> Launch {
    Launch {
        session: name(agent),
        spec: LaunchSpec::new("cat"),
        delivery: Delivery::None,
    }
}

fn waiting_ticket(m: &Model) -> Ticket {
    match &m.pane {
        Pane::Connecting { ticket, .. } => *ticket,
        _ => panic!("the pane is not connecting"),
    }
}

fn connecting_for(m: &mut Model, agent: &str, landing: Landing) -> Ticket {
    let ticket = m.ticket();
    m.set_pane(Pane::Connecting {
        name: name(agent),
        ticket,
        landing,
    });
    ticket
}

fn land(m: &mut Model, agent: &str, ticket: Ticket) -> Vec<Effect> {
    update(
        m,
        Msg::Launched {
            ticket,
            result: Ok(launch_of(agent)),
        },
    )
}

fn previews_once_checked(m: &mut Model, asked: &[Effect], agent: &str) {
    assert_eq!(checked_sessions(m, asked), [agent]);
    let effects = sighted(m, &[]);
    assert_eq!(attach_landings(m, &effects), [Landing::Preview]);
    assert_eq!(arrives(m, waiting_ticket(m)), Landing::Preview);
}

#[test]
fn enter_and_i_enter_an_agent_that_is_already_connected() {
    for code in [KeyCode::Enter, KeyCode::Char('i')] {
        let mut m = shown_in_nav(SHOWN);
        assert!(!in_agent(&m));
        update(&mut m, key(code));
        assert!(in_agent(&m), "{code:?}");
    }
}

#[test]
fn enter_and_i_on_an_agent_not_yet_connected_wait_to_enter_once_checked() {
    for code in [KeyCode::Enter, KeyCode::Char('i')] {
        let mut m = unconnected(OTHER);
        let asked = update(&mut m, key(code));
        assert_eq!(checked_sessions(&m, &asked), [OTHER]);
        let effects = sighted(&mut m, &[]);
        assert_eq!(attach_landings(&m, &effects), [ENTER_FROM_LIST], "{code:?}");
        assert_eq!(arrives(&m, waiting_ticket(&m)), ENTER_FROM_LIST);
    }
}

#[test]
fn enter_then_moving_the_cursor_before_the_check_is_back_only_previews() {
    let mut m = unconnected(OTHER);
    let asked = update(&mut m, key(KeyCode::Enter));
    update(&mut m, key(KeyCode::Char('j')));
    assert_ne!(m.cursor().and_then(Cursor::entry_name), Some(&name(OTHER)));
    previews_once_checked(&mut m, &asked, OTHER);
    assert!(!in_agent(&m));
}

#[test]
fn enter_then_opening_the_peek_before_the_check_is_back_previews_and_keeps_the_peek() {
    let mut m = unconnected(OTHER);
    let asked = update(&mut m, key(KeyCode::Enter));
    update(&mut m, key(KeyCode::Char('v')));
    assert!(m.peek.is_some());
    previews_once_checked(&mut m, &asked, OTHER);
    assert!(m.peek.is_some(), "the peek is still open");
}

#[test]
fn enter_then_opening_help_before_the_check_is_back_previews_behind_it() {
    let mut m = unconnected(OTHER);
    let asked = update(&mut m, key(KeyCode::Enter));
    update(&mut m, key(KeyCode::Char('?')));
    previews_once_checked(&mut m, &asked, OTHER);
    assert!(matches!(m.overlay, Overlay::Help(_)));
    assert!(!in_agent(&m));
}

#[test]
fn zero_in_the_agent_list_shows_the_agent_in_nav_without_waiting_to_enter() {
    let mut m = unconnected(OTHER);
    let asked = update(&mut m, key(KeyCode::Char('0')));
    previews_once_checked(&mut m, &asked, OTHER);
}

#[test]
fn zero_on_the_agent_already_shown_keeps_its_connection_in_nav() {
    let mut m = shown_in_nav(SHOWN);
    update(&mut m, key(KeyCode::Char('0')));
    assert_eq!(m.pane.session(), Some(&name(SHOWN)));
    assert!(m.pane.connection().is_some_and(|c| c.mode == Mode::Nav));
}

#[test]
fn escape_in_the_agent_list_does_nothing() {
    let mut m = shown_in_nav(SHOWN);
    let effects = update(&mut m, key(KeyCode::Esc));
    assert!(effects.is_empty());
    assert!(!in_agent(&m));
    assert_eq!(m.panel, Panel::Agents);
}

fn leave_agent(m: &mut Model) {
    m.touch_mru(&name(OTHER));
    m.touch_mru(&name(SHOWN));
    update(m, key(KeyCode::Enter));
    assert!(in_agent(m));
    update(m, nav());
    assert!(!in_agent(m));
}

#[test]
fn the_nav_key_in_nav_with_no_nav_key_before_it_does_nothing() {
    let mut m = shown_in_nav(SHOWN);
    m.touch_mru(&name(OTHER));
    m.touch_mru(&name(SHOWN));
    let effects = update(&mut m, nav());
    assert!(effects.is_empty());
    assert_eq!(m.cursor().and_then(Cursor::entry_name), Some(&name(SHOWN)));
}

#[test]
fn the_nav_key_after_another_key_following_the_one_that_left_does_nothing() {
    let mut m = shown_in_nav(SHOWN);
    leave_agent(&mut m);
    update(&mut m, key(KeyCode::Char('j')));
    let after_j = m.cursor().cloned();
    let effects = update(&mut m, nav());
    assert!(effects.is_empty());
    assert_eq!(m.cursor().cloned(), after_j);
}

#[test]
fn the_nav_key_twice_selects_and_previews_the_previous_agent_and_a_third_does_nothing() {
    let mut m = shown_in_nav(SHOWN);
    leave_agent(&mut m);
    let soon = later(&m, CHORD_WINDOW / 2);
    tick_at(&mut m, soon);
    let asked = update(&mut m, nav());
    assert_eq!(m.cursor().and_then(Cursor::entry_name), Some(&name(OTHER)));
    previews_once_checked(&mut m, &asked, OTHER);
    assert!(!in_agent(&m));
    assert!(update(&mut m, nav()).is_empty());
}

#[test]
fn a_click_between_the_two_nav_key_presses_cancels_the_chord() {
    let mut m = shown_in_nav(SHOWN);
    leave_agent(&mut m);
    update(&mut m, click());
    let effects = update(&mut m, nav());
    assert!(effects.is_empty());
    assert_eq!(m.cursor().and_then(Cursor::entry_name), Some(&name(SHOWN)));
}

#[test]
fn a_second_nav_key_press_after_the_window_has_passed_does_nothing() {
    let mut m = shown_in_nav(SHOWN);
    leave_agent(&mut m);
    let late = later(&m, CHORD_WINDOW + Duration::from_millis(1));
    tick_at(&mut m, late);
    let effects = update(&mut m, nav());
    assert!(effects.is_empty());
    assert_eq!(m.cursor().and_then(Cursor::entry_name), Some(&name(SHOWN)));
}

#[test]
fn a_switcher_pick_selects_and_previews_without_entering() {
    let mut m = shown_in_nav(SHOWN);
    update(&mut m, key(KeyCode::Char(' ')));
    type_text(&mut m, OTHER);
    let asked = update(&mut m, key(KeyCode::Enter));
    assert!(matches!(m.overlay, Overlay::None));
    assert_eq!(m.cursor().and_then(Cursor::entry_name), Some(&name(OTHER)));
    previews_once_checked(&mut m, &asked, OTHER);
}

#[test]
fn the_attention_key_selects_and_previews_the_agent_that_needs_you() {
    let mut m = fleet(10);
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
    assert_eq!(m.cursor().and_then(Cursor::entry_name), Some(&urgent));
    assert!(
        m.status
            .as_ref()
            .is_some_and(|s| s.text == format!("{urgent} needs you · ⏎ enters"))
    );
    previews_once_checked(&mut m, &asked, urgent.as_str());
}

#[test]
fn a_click_on_the_terminal_does_not_enter_the_agent() {
    let mut m = shown_in_nav(SHOWN);
    let effects = update(&mut m, click());
    assert!(effects.is_empty());
    assert!(!in_agent(&m));
}

#[test]
fn a_click_on_a_document_in_the_main_pane_focuses_it_and_does_not_enter() {
    let mut m = shown_in_nav(OTHER);
    update(&mut m, key(KeyCode::Char('3')));
    assert!(m.shows_document());
    m.areas.terminal = ratatui::layout::Rect::new(40, 2, 80, 24);
    update(&mut m, click());
    assert!(m.main_focused());
    assert!(!in_agent(&m));
}

#[test]
fn a_launch_submitted_from_the_form_waits_to_preview() {
    let mut m = model(&[]);
    update(&mut m, key(KeyCode::Char('c')));
    type_text(&mut m, "feature/x");
    let effects = update(&mut m, key(KeyCode::Enter));
    let Some(Effect::Job(Job::Launch { ticket, launch, .. })) = effects.first() else {
        panic!("expected a launch job");
    };
    let (ticket, session) = (*ticket, launch.session());
    assert!(matches!(
        &m.pane,
        Pane::Connecting {
            landing: Landing::Preview,
            ..
        }
    ));
    m.set_cursor(Cursor::Entry(session.clone()));
    let landed = land(&mut m, session.as_str(), ticket);
    assert!(matches!(landed.as_slice(), [Effect::Connect { .. }]));
    assert_eq!(arrives(&m, ticket), Landing::Preview);
    assert!(
        m.status
            .as_ref()
            .is_some_and(|s| s.text == format!("{session} is ready · ⏎ enters"))
    );
}

#[test]
fn a_session_started_by_enter_enters_when_it_lands_with_the_user_still_on_its_row() {
    let mut m = model(&[]);
    m.set_cursor(Cursor::Entry(name("idle")));
    let effects = update(&mut m, key(KeyCode::Enter));
    let [Effect::Job(Job::Start { ticket, .. })] = effects.as_slice() else {
        panic!("expected one start job");
    };
    let ticket = *ticket;
    assert!(matches!(
        &m.pane,
        Pane::Connecting {
            landing: Landing::Enter { .. },
            ..
        }
    ));
    land(&mut m, "idle", ticket);
    assert!(m.status.as_ref().is_none_or(|s| !s.text.contains("ready")));
    let arrival = m.arrival(ticket);
    connect(&mut m, "idle", Mode::Nav);
    connected(&mut m, arrival);
    assert!(in_agent(&m));
}

#[test]
fn a_session_started_by_enter_only_previews_when_the_cursor_moved_on_and_says_nothing() {
    let mut m = fleet(10);
    m.set_cursor(Cursor::Entry(name(OTHER)));
    let ticket = connecting_for(&mut m, OTHER, ENTER_FROM_LIST);
    m.set_cursor(Cursor::Entry(name(SHOWN)));
    land(&mut m, OTHER, ticket);
    assert_eq!(arrives(&m, ticket), Landing::Preview);
    assert!(m.status.as_ref().is_none_or(|s| !s.text.contains("ready")));
}

#[test]
fn a_session_started_by_enter_only_previews_when_something_is_open_over_the_list() {
    let mut m = fleet(10);
    m.set_cursor(Cursor::Entry(name(OTHER)));
    let ticket = connecting_for(&mut m, OTHER, ENTER_FROM_LIST);
    update(&mut m, key(KeyCode::Char(' ')));
    land(&mut m, OTHER, ticket);
    assert_eq!(arrives(&m, ticket), Landing::Preview);
}

#[test]
fn a_launch_that_lands_after_another_launch_took_the_pane_only_previews() {
    let mut m = fleet(10);
    m.set_cursor(Cursor::Entry(name(OTHER)));
    let first = connecting_for(&mut m, OTHER, ENTER_FROM_LIST);
    connecting_for(&mut m, OTHER, ENTER_FROM_LIST);
    land(&mut m, OTHER, first);
    assert_eq!(arrives(&m, first), Landing::Preview);
}

#[test]
fn selecting_the_agent_enter_is_starting_keeps_the_pane_waiting_for_it() {
    let sessions_keys = [KeyCode::Char('0')];
    for code in sessions_keys {
        let mut m = model(&[]);
        m.set_cursor(Cursor::Entry(name("idle")));
        update(&mut m, key(KeyCode::Enter));
        let ticket = waiting_ticket(&m);
        update(&mut m, key(code));
        assert_eq!(waiting_ticket(&m), ticket, "{code:?}");
    }
    let mut m = model(&[]);
    m.set_cursor(Cursor::Entry(name("idle")));
    update(&mut m, key(KeyCode::Enter));
    let ticket = waiting_ticket(&m);
    update(&mut m, key(KeyCode::Char(' ')));
    type_text(&mut m, "idle");
    update(&mut m, key(KeyCode::Enter));
    assert_eq!(waiting_ticket(&m), ticket, "the switcher");
}

#[test]
fn a_later_selection_of_the_agent_enter_is_starting_downgrades_the_entry_to_a_preview() {
    for pick in ["0", "switcher"] {
        let mut m = model(&[]);
        m.set_cursor(Cursor::Entry(name("idle")));
        update(&mut m, key(KeyCode::Enter));
        let ticket = waiting_ticket(&m);
        assert_eq!(arrives(&m, ticket), ENTER_FROM_LIST, "{pick}");
        if pick == "0" {
            update(&mut m, key(KeyCode::Char('0')));
        } else {
            update(&mut m, key(KeyCode::Char(' ')));
            type_text(&mut m, "idle");
            update(&mut m, key(KeyCode::Enter));
        }
        assert_eq!(arrives(&m, ticket), Landing::Preview, "{pick}");
    }
}

#[test]
fn a_switcher_pick_of_the_agent_enter_is_checking_does_not_enter_once_checked() {
    let mut m = unconnected(OTHER);
    let asked = update(&mut m, key(KeyCode::Enter));
    update(&mut m, key(KeyCode::Char(' ')));
    type_text(&mut m, OTHER);
    update(&mut m, key(KeyCode::Enter));
    previews_once_checked(&mut m, &asked, OTHER);
}

#[test]
fn a_start_landing_after_the_keys_moved_to_another_panel_only_previews() {
    let mut m = fleet(10);
    m.set_cursor(Cursor::Entry(name(OTHER)));
    let ticket = connecting_for(&mut m, OTHER, ENTER_FROM_LIST);
    m.panel = Panel::activity();
    land(&mut m, OTHER, ticket);
    assert_eq!(arrives(&m, ticket), Landing::Preview);
}

#[test]
fn a_start_landing_with_the_keys_still_in_the_panel_that_asked_enters() {
    let mut m = fleet(10);
    m.set_cursor(Cursor::Entry(name(OTHER)));
    m.panel = Panel::Changes { file: 0 };
    let from = Landing::Enter {
        from: Seat::Changes,
    };
    let ticket = connecting_for(&mut m, OTHER, from);
    m.panel = Panel::Changes { file: 1 };
    land(&mut m, OTHER, ticket);
    assert_eq!(arrives(&m, ticket), from);
}

#[test]
fn enter_then_moving_the_keys_to_the_activity_before_the_check_is_back_only_previews() {
    let mut m = unconnected(OTHER);
    let asked = update(&mut m, key(KeyCode::Enter));
    m.panel = Panel::activity();
    previews_once_checked(&mut m, &asked, OTHER);
    assert!(!in_agent(&m));
}

fn attached_with_nav(nav: &str) -> Model {
    let mut m = shown_in_nav(SHOWN);
    let config: agent_core::config::Config =
        format!("[keys]\nnav = '{nav}'").parse().expect("parses");
    m.keys =
        crate::keyspec::Sendable::resolve(config.keys, crate::keyspec::Keyboard::Disambiguated);
    m.touch_mru(&name(OTHER));
    m.touch_mru(&name(SHOWN));
    update(&mut m, key(KeyCode::Enter));
    m
}

#[test]
fn leaving_through_the_escape_hatch_does_not_arm_the_nav_key_chord() {
    let mut m = attached_with_nav("alt-,");
    update(&mut m, key_with(KeyCode::Char(']'), KeyModifiers::CONTROL));
    assert!(!in_agent(&m));
    assert_eq!(m.chord, crate::model::Chord::Idle);
}

#[test]
fn leaving_with_the_configured_nav_key_arms_the_chord() {
    let mut m = attached_with_nav("alt-,");
    update(&mut m, key_with(KeyCode::Char(','), KeyModifiers::ALT));
    assert!(!in_agent(&m));
    assert!(m.chord.is_live(m.clock.mono));
}

#[test]
fn the_nav_key_twice_skips_a_previous_agent_that_no_longer_exists() {
    let mut m = shown_in_nav(SHOWN);
    m.touch_mru(&name(OTHER));
    m.touch_mru(&name("deleted-agent"));
    m.touch_mru(&name(SHOWN));
    update(&mut m, key(KeyCode::Enter));
    update(&mut m, nav());
    let asked = update(&mut m, nav());
    assert_eq!(m.cursor().and_then(Cursor::entry_name), Some(&name(OTHER)));
    previews_once_checked(&mut m, &asked, OTHER);
}

#[test]
fn selecting_an_agent_the_form_is_still_launching_is_refused_and_keeps_its_pane() {
    let mut m = fleet(10);
    m.set_cursor(Cursor::Entry(name(OTHER)));
    let ticket = connecting_for(&mut m, OTHER, Landing::Preview);
    m.launching.push((ticket, name(OTHER)));
    let effects = update(&mut m, key(KeyCode::Char('0')));
    assert!(effects.is_empty());
    assert_eq!(waiting_ticket(&m), ticket);
    assert!(
        m.status
            .as_ref()
            .is_some_and(|s| s.text.contains("still starting"))
    );
}
