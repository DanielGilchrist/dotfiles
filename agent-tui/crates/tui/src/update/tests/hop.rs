use super::*;
use crate::fixtures::{
    attach_landings, connect, connect_printing, fleet, hear_the_terminal, in_nvim, sendable,
    sighted,
};
use crate::keyspec::Keyboard;
use crate::model::{HELD_CAP, Landing, Mode, SETTLE_TIMEOUT, Visibility};

const ALT: KeyModifiers = KeyModifiers::ALT;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;

fn previous() -> Msg {
    key_with(KeyCode::Char(','), ALT)
}

fn next() -> Msg {
    key_with(KeyCode::Char('.'), ALT)
}

fn listed(m: &Model) -> Vec<String> {
    m.items()
        .iter()
        .filter_map(Cursor::entry_name)
        .map(ToString::to_string)
        .collect()
}

fn on(agent: &str) -> Model {
    let mut m = fleet(10);
    m.visibility = Visibility::All;
    m.set_cursor(Cursor::Entry(name(agent)));
    m
}

fn typing_into(agent: &str) -> Model {
    let mut m = on(agent);
    connect(&mut m, agent, Mode::Agent);
    m
}

fn pty_bytes(effects: &[Effect]) -> Vec<Vec<u8>> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Pty { bytes, .. } => Some(bytes.clone()),
            _ => None,
        })
        .collect()
}

fn in_agent(m: &Model) -> bool {
    matches!(m.focus(), Focus::Agent(_))
}

fn said(m: &Model) -> Option<String> {
    m.status.as_ref().map(|status| status.text.clone())
}

fn connected_to(m: &Model) -> Option<String> {
    m.pane.connection().map(|c| c.name.to_string())
}

/// The agent's neighbours in the list, from the live fixture.
fn around(agent: &str) -> (String, String) {
    let names = listed(&on(agent));
    let at = names.iter().position(|n| n == agent).expect("listed");
    (
        names[(at + names.len() - 1) % names.len()].clone(),
        names[(at + 1) % names.len()].clone(),
    )
}

fn type_keys(m: &mut Model, text: &str) -> Vec<Effect> {
    text.chars()
        .flat_map(|c| update(m, key(KeyCode::Char(c))))
        .collect()
}

/// The check comes back with nobody else showing `agent` and its client
/// arrives. The client's program prints `escapes` first, then echoes.
fn arrives_printing(m: &mut Model, agent: &str, escapes: &str) {
    let effects = sighted(m, &[]);
    assert!(matches!(
        attach_landings(m, &effects).as_slice(),
        [Landing::Hop { .. } | Landing::Preview | Landing::Enter { .. }]
    ));
    let ticket = match &m.pane {
        Pane::Connecting { ticket, .. } => *ticket,
        _ => panic!("the pane is not connecting"),
    };
    let arrival = m.arrival(ticket);
    connect_printing(m, agent, Mode::Nav, escapes);
    connected(m, arrival);
}

fn arrives(m: &mut Model, agent: &str) {
    arrives_printing(m, agent, "\\033[?25h");
}

/// What the next tick types for the client once its terminal has printed.
fn delivered(m: &mut Model) -> Vec<u8> {
    hear_the_terminal(m);
    pty_bytes(&tick(m)).concat()
}

#[test]
fn in_nav_the_chords_select_the_neighbour_and_preview_it_without_entering() {
    let (before, after) = around("auth-refresh-2");
    for (chord, expected) in [(previous(), before), (next(), after)] {
        let mut m = on("auth-refresh-2");
        let effects = update(&mut m, chord);
        assert_eq!(selected(&m).as_deref(), Some(expected.as_str()));
        assert_eq!(checked_sessions(&m, &effects), [expected.as_str()]);
        let effects = sighted(&mut m, &[]);
        assert_eq!(attach_landings(&m, &effects), [Landing::Preview]);
        assert!(!in_agent(&m));
    }
}

#[test]
fn the_chords_wrap_around_the_ends_of_the_list_in_nav_and_in_agent() {
    let names = listed(&on("auth-refresh-2"));
    let (first, last) = (names[0].clone(), names[names.len() - 1].clone());
    let mut m = on(&first);
    update(&mut m, previous());
    assert_eq!(selected(&m), Some(last.clone()));
    update(&mut m, next());
    assert_eq!(selected(&m), Some(first.clone()));
    let mut m = typing_into(&first);
    update(&mut m, previous());
    assert_eq!(selected(&m), Some(last.clone()));
    let mut m = typing_into(&last);
    update(&mut m, next());
    assert_eq!(selected(&m), Some(first));
}

#[test]
fn with_no_agent_or_only_one_the_chords_do_nothing_and_say_nothing() {
    for count in [0, 1] {
        for chord in [previous, next] {
            for mode in [Mode::Nav, Mode::Agent] {
                let mut m = fleet(count);
                m.visibility = Visibility::All;
                if let Some(only) = listed(&m).first().cloned() {
                    connect(&mut m, &only, mode);
                }
                let before = selected(&m);
                let effects = update(&mut m, chord());
                assert!(effects.is_empty(), "{count} agents");
                assert_eq!(selected(&m), before);
                assert_eq!(said(&m), None);
            }
        }
    }
}

#[test]
fn the_candidates_follow_the_list_skipping_repo_rows_in_every_view() {
    for view in [View::Focus, View::Overview] {
        let mut m = fleet(10);
        m.visibility = Visibility::All;
        m.view = view;
        let agents = listed(&m);
        if view == View::Overview {
            assert!(m.items().len() > agents.len(), "repo rows are in the list");
        }
        for (at, agent) in agents.iter().enumerate() {
            let from = Some(name(agent));
            let first = |hop| {
                m.candidates(hop, from.as_ref())
                    .first()
                    .map(ToString::to_string)
            };
            assert_eq!(
                first(Hop::Next),
                Some(agents[(at + 1) % agents.len()].clone()),
                "{view:?} next of {agent}"
            );
            assert_eq!(
                first(Hop::Previous),
                Some(agents[(at + agents.len() - 1) % agents.len()].clone()),
                "{view:?} previous of {agent}"
            );
            assert_eq!(
                m.candidates(Hop::Next, from.as_ref()).len(),
                agents.len() - 1,
                "every other agent once"
            );
        }
    }
}

#[test]
fn each_agent_is_a_candidate_once_in_every_view() {
    for view in [View::Focus, View::Overview, View::Pinned] {
        let mut m = fleet(10);
        m.visibility = Visibility::All;
        m.view = view;
        for agent in listed(&m) {
            for hop in Hop::ALL {
                let names = m.candidates(hop, Some(&name(&agent)));
                let mut once = names.clone();
                once.sort_by_key(ToString::to_string);
                once.dedup();
                assert_eq!(once.len(), names.len(), "{view:?} {hop:?} from {agent}");
                assert!(!names.contains(&name(&agent)));
            }
        }
    }
}

#[test]
fn from_a_repo_row_the_chords_reach_the_agent_listed_after_it() {
    let mut m = fleet(10);
    m.visibility = Visibility::All;
    m.view = View::Overview;
    let items = m.items();
    let at = items
        .iter()
        .position(|item| matches!(item, Cursor::Repo(_)))
        .expect("a repo row");
    let after = items[at..]
        .iter()
        .find_map(Cursor::entry_name)
        .expect("an agent after it")
        .to_string();
    m.set_cursor(items[at].clone());
    update(&mut m, next());
    assert_eq!(selected(&m), Some(after));
}

#[test]
fn in_agent_a_chord_is_not_sent_to_the_agent_and_other_keys_still_are() {
    for event in [previous(), next()] {
        let mut m = typing_into("auth-refresh-2");
        let effects = update(&mut m, event);
        assert_eq!(pty_bytes(&effects), Vec::<Vec<u8>>::new());
    }
    let mut m = typing_into("auth-refresh-2");
    let others = [
        key(KeyCode::Char('x')),
        key(KeyCode::Char(',')),
        key_with(KeyCode::Char('x'), ALT),
        key_with(KeyCode::Char('.'), ALT | KeyModifiers::SHIFT),
    ];
    for (index, event) in others.into_iter().enumerate() {
        let effects = update(&mut m, event);
        assert_eq!(pty_bytes(&effects).len(), 1, "key {index}");
        assert!(in_agent(&m));
    }
}

#[test]
fn in_agent_a_chord_switches_to_the_neighbour_and_lands_in_agent_there() {
    let (before, after) = around("auth-refresh-2");
    for (chord, expected) in [(previous(), before), (next(), after)] {
        let mut m = typing_into("auth-refresh-2");
        let asked = update(&mut m, chord);
        assert_eq!(checked_sessions(&m, &asked), [expected.as_str()]);
        assert!(!in_agent(&m), "the old agent is let go while it connects");
        assert_eq!(selected(&m).as_deref(), Some(expected.as_str()));
        arrives(&mut m, &expected);
        assert!(in_agent(&m));
        assert_eq!(connected_to(&m), Some(expected));
    }
}

#[test]
fn in_agent_the_chord_walks_past_agents_that_cannot_take_the_keys_in_one_press() {
    let mut m = typing_into("rate-limit-9");
    assert_eq!(
        &listed(&m)[3..],
        ["leave-request-7", "triage-flow-8"],
        "an exited agent and one with no session follow it"
    );
    let asked = update(&mut m, next());
    assert_eq!(checked_sessions(&m, &asked), ["agent-tui0"]);
    arrives(&mut m, "agent-tui0");
    assert!(in_agent(&m));
    assert_eq!(said(&m), None);
}

#[test]
fn in_agent_the_chord_walks_past_an_agent_another_window_shows() {
    let (_, after) = around("auth-refresh-2");
    let names = listed(&on("auth-refresh-2"));
    let at = names.iter().position(|n| *n == after).expect("listed");
    let beyond = names[(at + 1) % names.len()].clone();
    let mut m = typing_into("auth-refresh-2");
    m.record_company(&name(&after), in_nvim());
    let asked = update(&mut m, next());
    assert_eq!(checked_sessions(&m, &asked), [beyond.as_str()]);
}

#[test]
fn in_agent_with_no_agent_that_can_take_the_keys_the_chord_says_so_and_stays() {
    let mut m = typing_into("auth-refresh-2");
    for other in listed(&m) {
        if other != "auth-refresh-2" {
            m.record_company(&name(&other), in_nvim());
        }
    }
    let effects = update(&mut m, next());
    assert!(effects.is_empty());
    assert!(in_agent(&m));
    assert_eq!(connected_to(&m).as_deref(), Some("auth-refresh-2"));
    assert_eq!(said(&m).as_deref(), Some("no other agent to hop to"));
}

#[test]
fn in_nav_the_chords_still_step_over_every_agent_even_one_that_is_not_running() {
    let mut m = on("rate-limit-9");
    update(&mut m, next());
    assert_eq!(selected(&m).as_deref(), Some("leave-request-7"));
    update(&mut m, next());
    assert_eq!(selected(&m).as_deref(), Some("triage-flow-8"));
}

#[test]
fn a_chord_pressed_while_a_hop_connects_goes_on_from_it_and_keeps_what_was_typed() {
    let names = listed(&on("auth-refresh-2"));
    let at = names
        .iter()
        .position(|n| n == "auth-refresh-2")
        .expect("listed");
    let (first, second) = (names[at + 1].clone(), names[(at + 2) % names.len()].clone());
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    assert_eq!(selected(&m), Some(first));
    type_keys(&mut m, "a");
    let asked = update(&mut m, next());
    assert_eq!(checked_sessions(&m, &asked), [second.as_str()]);
    arrives(&mut m, &second);
    assert_eq!(delivered(&mut m), b"a");
    assert!(in_agent(&m));
}

#[test]
fn a_chord_pressed_while_an_enter_is_still_checking_hops_from_it() {
    let mut m = on("auth-refresh-2");
    let asked = update(&mut m, key(KeyCode::Enter));
    assert_eq!(checked_sessions(&m, &asked), ["auth-refresh-2"]);
    let (_, after) = around("auth-refresh-2");
    let asked = update(&mut m, next());
    assert_eq!(checked_sessions(&m, &asked), [after.as_str()]);
    arrives(&mut m, &after);
    assert!(in_agent(&m));
}

#[test]
fn what_is_typed_right_after_a_hop_reaches_the_new_agent_and_does_nothing_here() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    let effects = type_keys(&mut m, "dryq");
    assert!(effects.is_empty(), "no kill, no quit, nothing at all");
    assert!(matches!(m.overlay, Overlay::None), "no confirmation opened");
    assert_eq!(selected(&m).as_deref(), Some(after.as_str()));
    arrives(&mut m, &after);
    assert_eq!(delivered(&mut m), b"dryq");
    assert!(in_agent(&m));
}

#[test]
fn movement_keys_typed_during_a_hop_do_not_move_the_cursor_or_spoil_the_landing() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    type_keys(&mut m, "jjk");
    update(&mut m, key(KeyCode::Down));
    assert_eq!(selected(&m).as_deref(), Some(after.as_str()));
    arrives(&mut m, &after);
    assert!(in_agent(&m), "the landing is still an enter");
}

#[test]
fn a_paste_held_across_the_connect_gets_the_brackets_the_new_terminal_asks_for() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    let effects = update(&mut m, Msg::Paste("fix this\nand that".into()));
    assert!(effects.is_empty());
    arrives_printing(&mut m, &after, "\\033[?2004h");
    assert_eq!(delivered(&mut m), b"\x1b[200~fix this\nand that\x1b[201~");
    assert!(in_agent(&m));
}

#[test]
fn a_paste_held_for_a_terminal_that_never_asks_for_brackets_is_sent_bare() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    update(&mut m, Msg::Paste("one line".into()));
    arrives(&mut m, &after);
    assert_eq!(delivered(&mut m), b"one line");
}

#[test]
fn held_arrow_keys_use_the_cursor_mode_the_new_terminal_set() {
    let (_, after) = around("auth-refresh-2");
    for (escapes, expected) in [
        ("\\033[?1h", &b"\x1bOA"[..]),
        ("\\033[?25h", &b"\x1b[A"[..]),
    ] {
        let mut m = typing_into("auth-refresh-2");
        update(&mut m, next());
        update(&mut m, key(KeyCode::Up));
        arrives_printing(&mut m, &after, escapes);
        assert_eq!(delivered(&mut m), expected, "{escapes}");
    }
}

#[test]
fn nothing_is_written_before_the_new_terminal_has_printed_and_later_keys_are_still_held() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    type_keys(&mut m, "ab");
    sighted(&mut m, &[]);
    let ticket = match &m.pane {
        Pane::Connecting { ticket, .. } => *ticket,
        _ => panic!("the pane is not connecting"),
    };
    let arrival = m.arrival(ticket);
    connect(&mut m, &after, Mode::Nav);
    connected(&mut m, arrival);
    assert!(
        in_agent(&m),
        "the keys are in the agent as far as the user can tell"
    );
    assert_eq!(pty_bytes(&tick(&mut m)), Vec::<Vec<u8>>::new());
    let effects = type_keys(&mut m, "c");
    assert_eq!(pty_bytes(&effects), Vec::<Vec<u8>>::new(), "held, not sent");
    assert_eq!(said(&m), None);
}

#[test]
fn a_terminal_that_prints_nothing_in_time_loses_what_was_typed_and_says_so() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    type_keys(&mut m, "ab");
    sighted(&mut m, &[]);
    let ticket = match &m.pane {
        Pane::Connecting { ticket, .. } => *ticket,
        _ => panic!("the pane is not connecting"),
    };
    let arrival = m.arrival(ticket);
    connect(&mut m, &after, Mode::Nav);
    connected(&mut m, arrival);
    let almost = later(&m, SETTLE_TIMEOUT.saturating_sub(Duration::from_millis(1)));
    assert_eq!(pty_bytes(&tick_at(&mut m, almost)), Vec::<Vec<u8>>::new());
    assert_eq!(said(&m), None);
    let over = later(&m, SETTLE_TIMEOUT);
    assert_eq!(pty_bytes(&tick_at(&mut m, over)), Vec::<Vec<u8>>::new());
    assert_eq!(said(&m).as_deref(), Some("dropped 2 keys"));
    let after_that = type_keys(&mut m, "z");
    assert_eq!(
        pty_bytes(&after_that).len(),
        1,
        "keys go straight through again"
    );
}

#[test]
fn the_nav_key_and_the_escape_hatch_give_a_hop_up_and_say_what_was_dropped() {
    for leave in [
        key_with(KeyCode::Char(']'), CTRL),
        key_with(KeyCode::Char('5'), CTRL),
    ] {
        let (_, after) = around("auth-refresh-2");
        let mut m = typing_into("auth-refresh-2");
        update(&mut m, next());
        type_keys(&mut m, "ab");
        let effects = update(&mut m, leave);
        assert!(effects.is_empty());
        assert_eq!(said(&m).as_deref(), Some("dropped 2 keys"));
        assert_eq!(selected(&m).as_deref(), Some(after.as_str()));
        let landing = sighted(&mut m, &[]);
        assert_eq!(attach_landings(&m, &landing), [Landing::Preview]);
        assert!(!in_agent(&m));
    }
}

#[test]
fn leaving_before_anything_was_typed_says_nothing() {
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    update(&mut m, key_with(KeyCode::Char(']'), CTRL));
    assert_eq!(said(&m), None);
}

#[test]
fn a_hop_that_stops_being_wanted_before_it_lands_drops_what_was_typed_and_says_so() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    type_keys(&mut m, "x");
    update(&mut m, Msg::Paste("y".into()));
    m.set_cursor(Cursor::Entry(name("rate-limit-1")));
    let effects = sighted(&mut m, &[]);
    assert_eq!(attach_landings(&m, &effects), [Landing::Preview]);
    assert_eq!(said(&m).as_deref(), Some("dropped 1 key and a paste"));
    assert_ne!(selected(&m).as_deref(), Some(after.as_str()));
}

#[test]
fn a_hop_whose_client_arrives_after_the_user_moved_on_says_what_it_dropped() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    sighted(&mut m, &[]);
    type_keys(&mut m, "xyz");
    m.set_cursor(Cursor::Entry(name("rate-limit-1")));
    let ticket = match &m.pane {
        Pane::Connecting { ticket, .. } => *ticket,
        _ => panic!("the pane is not connecting"),
    };
    let arrival = m.arrival(ticket);
    connect(&mut m, &after, Mode::Nav);
    connected(&mut m, arrival);
    assert_eq!(pty_bytes(&tick(&mut m)), Vec::<Vec<u8>>::new());
    assert_eq!(said(&m).as_deref(), Some("dropped 3 keys"));
    assert!(!in_agent(&m));
}

#[test]
fn a_hop_that_finds_the_agent_shown_elsewhere_drops_what_was_typed_and_says_both() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    type_keys(&mut m, "ab");
    sighted(&mut m, &[(after.as_str(), in_nvim())]);
    assert!(matches!(m.pane, Pane::Elsewhere { .. }));
    assert_eq!(
        said(&m),
        Some(format!("{after}: ⇄ in nvim · ⏎ move here · dropped 2 keys"))
    );
    assert!(!in_agent(&m));
}

#[test]
fn leaving_during_the_wait_for_the_new_terminal_drops_what_was_held_and_leaves_for_nav() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    sighted(&mut m, &[]);
    let ticket = match &m.pane {
        Pane::Connecting { ticket, .. } => *ticket,
        _ => panic!("the pane is not connecting"),
    };
    let arrival = m.arrival(ticket);
    connect(&mut m, &after, Mode::Nav);
    connected(&mut m, arrival);
    type_keys(&mut m, "a");
    let effects = update(&mut m, key_with(KeyCode::Char(']'), CTRL));
    assert_eq!(pty_bytes(&effects), Vec::<Vec<u8>>::new());
    assert!(!in_agent(&m));
    assert_eq!(said(&m).as_deref(), Some("dropped 1 key"));
    assert_eq!(pty_bytes(&tick(&mut m)), Vec::<Vec<u8>>::new());
}

#[test]
fn once_too_much_was_typed_during_a_hop_nothing_of_it_is_delivered_not_even_the_later_keys() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    update(&mut m, Msg::Paste("a".repeat(HELD_CAP + 1)));
    update(&mut m, key(KeyCode::Enter));
    arrives(&mut m, &after);
    assert_eq!(
        delivered(&mut m),
        Vec::<u8>::new(),
        "no Enter without its paste"
    );
    assert_eq!(said(&m).as_deref(), Some("dropped 1 key and a paste"));
    assert!(in_agent(&m));
}

#[test]
fn what_fit_before_the_cap_is_lost_with_what_didnt_and_all_of_it_is_counted() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    type_keys(&mut m, "ab");
    update(&mut m, Msg::Paste("a".repeat(HELD_CAP)));
    update(&mut m, Msg::Paste("b".into()));
    arrives(&mut m, &after);
    assert_eq!(delivered(&mut m), Vec::<u8>::new());
    assert_eq!(said(&m).as_deref(), Some("dropped 2 keys and 2 pastes"));
}

#[test]
fn exactly_the_cap_is_kept() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    update(&mut m, next());
    update(&mut m, Msg::Paste("a".repeat(HELD_CAP)));
    arrives(&mut m, &after);
    assert_eq!(delivered(&mut m).len(), HELD_CAP);
    assert_eq!(said(&m), None);
}

#[test]
fn a_chord_set_to_none_is_not_taken_in_either_mode_and_the_other_still_works() {
    let mut m = typing_into("auth-refresh-2");
    m.keys = sendable("next_agent = 'none'", Keyboard::Legacy);
    let effects = update(&mut m, next());
    assert_eq!(pty_bytes(&effects).len(), 1, "alt-. goes to the agent");
    assert!(in_agent(&m));
    let effects = update(&mut m, previous());
    assert_eq!(checked_sessions(&m, &effects).len(), 1, "alt-, still hops");

    let mut m = on("auth-refresh-2");
    m.keys = sendable("prev_agent = 'off'", Keyboard::Legacy);
    update(&mut m, previous());
    assert_eq!(selected(&m).as_deref(), Some("auth-refresh-2"));
    update(&mut m, next());
    assert_ne!(selected(&m).as_deref(), Some("auth-refresh-2"));
}

#[test]
fn a_configured_chord_replaces_the_default_one() {
    let mut m = on("auth-refresh-2");
    m.keys = sendable("next_agent = 'f6'", Keyboard::Legacy);
    update(&mut m, next());
    assert_eq!(selected(&m).as_deref(), Some("auth-refresh-2"));
    update(&mut m, key(KeyCode::F(6)));
    assert_ne!(selected(&m).as_deref(), Some("auth-refresh-2"));
}

#[test]
fn open_overlays_and_the_peek_keep_their_own_keys_and_are_not_closed_by_a_chord() {
    type Open = (fn(&mut Model), fn(&Model) -> bool);
    let opens: [Open; 2] = [
        (
            |m| m.overlay = Overlay::Switcher(Query::default()),
            |m| matches!(m.overlay, Overlay::Switcher(_)),
        ),
        (
            |m| m.peek = Some(crate::model::Peek::Pane),
            |m| matches!(m.peek, Some(crate::model::Peek::Pane)),
        ),
    ];
    for chord in [previous, next] {
        for (open, still_open) in opens {
            let mut m = on("auth-refresh-2");
            connect(&mut m, "auth-refresh-2", Mode::Nav);
            open(&mut m);
            update(&mut m, chord());
            assert_eq!(selected(&m).as_deref(), Some("auth-refresh-2"));
            assert!(still_open(&m), "the chord went to the overlay or peek");
        }
    }
}

fn scroll_over_the_terminal() -> Msg {
    Msg::Mouse(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollUp,
        column: 50,
        row: 5,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn the_mouse_is_not_forwarded_while_held_keys_wait_for_the_new_terminal() {
    let (_, after) = around("auth-refresh-2");
    let mut m = typing_into("auth-refresh-2");
    let area = ratatui::layout::Rect::new(40, 2, 80, 24);
    update(&mut m, next());
    type_keys(&mut m, "a");
    arrives_printing(&mut m, &after, "\\033[?1000h\\033[?1006h");
    hear_the_terminal(&mut m);
    m.areas.terminal = area;
    let held = update(&mut m, scroll_over_the_terminal());
    assert_eq!(pty_bytes(&held), Vec::<Vec<u8>>::new());
    assert_eq!(pty_bytes(&tick(&mut m)).concat(), b"a");
    m.areas.terminal = area;
    let ready = update(&mut m, scroll_over_the_terminal());
    assert_eq!(pty_bytes(&ready).len(), 1, "once settled it is forwarded");
}
