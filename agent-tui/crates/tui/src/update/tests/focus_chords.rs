use super::*;
use crate::fixtures::{connect, in_nvim, sendable};
use crate::keyspec::Keyboard;
use crate::model::{Mode, Origin, Peek};

const ALT: KeyModifiers = KeyModifiers::ALT;
const AGENT: &str = "fix";

fn left() -> Msg {
    key_with(KeyCode::Char('h'), ALT)
}

fn right() -> Msg {
    key_with(KeyCode::Char('l'), ALT)
}

fn enter() -> Msg {
    key(KeyCode::Enter)
}

fn on() -> Model {
    let mut m = model(&[]);
    m.keys = sendable(
        "focus_left = 'alt-h'\nfocus_right = 'alt-l'",
        Keyboard::Legacy,
    );
    with_activity(&mut m);
    with_changes(&mut m, 3);
    m
}

fn shown() -> Model {
    let mut m = on();
    connect(&mut m, AGENT, Mode::Nav);
    m
}

fn typing() -> Model {
    let mut m = on();
    connect(&mut m, AGENT, Mode::Agent);
    m
}

fn pty_bytes(effects: &[Effect]) -> usize {
    effects
        .iter()
        .filter(|effect| matches!(effect, Effect::Pty { .. }))
        .count()
}

fn mode(m: &Model) -> Option<Mode> {
    m.pane.connection().map(|c| c.mode)
}

fn pending(m: &Model) -> String {
    match &m.pane {
        Pane::Empty => "empty".into(),
        Pane::Checking { then, .. } => format!("checking {then:?}"),
        Pane::Connecting { landing, .. } => format!("connecting {landing:?}"),
        Pane::Connected(c) => format!("connected {:?}", c.mode),
        Pane::Elsewhere { .. } => "elsewhere".into(),
    }
}

fn said(m: &Model) -> Option<String> {
    m.status.as_ref().map(|status| status.text.clone())
}

fn outcome(m: &Model, effects: &[Effect]) -> (String, Option<String>, usize, Panel) {
    (pending(m), said(m), effects.len(), m.panel.clone())
}

fn same_as_enter(build: impl Fn() -> Model) -> (Model, Vec<Effect>) {
    let mut by_enter = build();
    let enter_effects = update(&mut by_enter, enter());
    let mut by_chord = build();
    let chord_effects = update(&mut by_chord, right());
    assert_eq!(
        outcome(&by_chord, &chord_effects),
        outcome(&by_enter, &enter_effects)
    );
    (by_chord, chord_effects)
}

#[test]
fn focus_right_in_agents_enters_the_shown_agent_as_enter_does() {
    let (m, effects) = same_as_enter(shown);
    assert_eq!(mode(&m), Some(Mode::Agent));
    assert_eq!(pty_bytes(&effects), 0);
}

#[test]
fn focus_right_with_nothing_connected_starts_the_same_check_enter_starts() {
    let (m, _) = same_as_enter(|| {
        let mut m = on();
        m.pane = Pane::Empty;
        m
    });
    assert!(pending(&m).starts_with("checking Enter"), "{}", pending(&m));
}

#[test]
fn focus_right_on_an_agent_shown_in_another_window_moves_here_as_enter_does() {
    let (m, _) = same_as_enter(|| {
        let mut m = on();
        m.pane = Pane::Elsewhere { name: name(AGENT) };
        m.record_company(&name(AGENT), in_nvim());
        m
    });
    assert_eq!(pending(&m), "connecting MoveHere { from: Agents }");
}

#[test]
fn focus_right_on_this_tuis_own_session_is_refused_as_enter_refuses() {
    let (m, effects) = same_as_enter(|| {
        let mut m = shown();
        m.own_session = Some(name(AGENT));
        m
    });
    assert!(effects.is_empty());
    assert_eq!(said(&m).as_deref(), Some("this TUI is running inside fix"));
    assert_eq!(mode(&m), Some(Mode::Nav));
}

#[test]
fn focus_right_on_an_agent_still_starting_is_refused_as_enter_refuses() {
    let (m, effects) = same_as_enter(|| {
        let mut m = on();
        m.pane = Pane::Empty;
        let ticket = m.ticket();
        m.launching.push((ticket, name(AGENT)));
        m
    });
    assert!(effects.is_empty());
    assert!(said(&m).is_some_and(|text| text.contains("still starting")));
}

#[test]
fn focus_right_in_the_activity_and_changes_panels_focuses_their_document_in_main() {
    let mut m = shown();
    update(&mut m, key(KeyCode::Char('2')));
    let effects = update(&mut m, right());
    assert!(effects.is_empty());
    assert!(matches!(m.panel, Panel::Main(Origin::Activity { .. }, _)));
    assert_eq!(mode(&m), Some(Mode::Nav));

    let mut m = shown();
    update(&mut m, key(KeyCode::Char('3')));
    update(&mut m, right());
    assert!(matches!(m.panel, Panel::Main(Origin::Changes { .. }, _)));
}

#[test]
fn focus_right_does_nothing_in_main_in_agent_or_on_a_repo_row() {
    let mut m = shown();
    update(&mut m, key(KeyCode::Char('2')));
    update(&mut m, right());
    let before = m.panel.clone();
    let effects = update(&mut m, right());
    assert!(effects.is_empty());
    assert_eq!(m.panel, before);
    assert_eq!(said(&m), None);

    let mut m = typing();
    let effects = update(&mut m, right());
    assert_eq!(pty_bytes(&effects), 0, "not forwarded to the agent");
    assert_eq!(mode(&m), Some(Mode::Agent));
    assert_eq!(said(&m), None);

    let mut m = shown();
    update(&mut m, key(KeyCode::Char('[')));
    update(&mut m, key(KeyCode::Char('g')));
    assert!(matches!(m.cursor(), Some(Cursor::Repo(_))));
    let effects = update(&mut m, right());
    assert!(effects.is_empty());
    assert_eq!(said(&m), None);
}

#[test]
fn focus_left_from_agent_goes_back_to_nav_in_the_panel_it_was_entered_from() {
    for entered_from in [
        Panel::Agents,
        Panel::activity(),
        Panel::Changes { file: 1 },
        Panel::Main(crate::model::Origin::Changes { file: 1 }, None),
    ] {
        let mut m = typing();
        m.panel = entered_from.clone();
        let effects = update(&mut m, left());
        assert_eq!(pty_bytes(&effects), 0, "not forwarded to the agent");
        assert_eq!(mode(&m), Some(Mode::Nav));
        assert_eq!(m.panel, entered_from);
    }
}

#[test]
fn focus_left_from_agent_selects_the_agent_it_was_typing_into() {
    let mut m = typing();
    m.set_cursor(Cursor::Entry(name("api")));
    update(&mut m, left());
    assert_eq!(selected(&m).as_deref(), Some(AGENT));
    assert_eq!(mode(&m), Some(Mode::Nav));
}

#[test]
fn focus_left_from_a_document_in_main_goes_back_to_the_list_panel_it_came_from() {
    let mut m = shown();
    update(&mut m, key(KeyCode::Char('3')));
    update(&mut m, key(KeyCode::Char('j')));
    update(&mut m, right());
    let effects = update(&mut m, left());
    assert!(effects.is_empty());
    assert_eq!(m.panel, Panel::Changes { file: 1 });

    let mut m = shown();
    update(&mut m, key(KeyCode::Char('2')));
    update(&mut m, right());
    update(&mut m, left());
    assert!(matches!(m.panel, Panel::Agent { .. }));
}

#[test]
fn focus_left_does_nothing_from_a_list_panel() {
    for panel in [Panel::Agents, Panel::activity(), Panel::Changes { file: 0 }] {
        let mut m = shown();
        m.panel = panel.clone();
        let effects = update(&mut m, left());
        assert!(effects.is_empty());
        assert_eq!(m.panel, panel);
        assert_eq!(mode(&m), Some(Mode::Nav));
        assert_eq!(said(&m), None);
    }
}

#[test]
fn the_user_can_walk_the_list_and_enter_another_agent_with_the_two_chords() {
    let mut m = typing();
    update(&mut m, left());
    update(&mut m, key(KeyCode::Char('j')));
    assert_ne!(selected(&m).as_deref(), Some(AGENT));
    let effects = update(&mut m, right());
    assert!(!effects.is_empty(), "{}", pending(&m));
    assert!(
        pending(&m).contains("Enter { from: Agents }"),
        "{}",
        pending(&m)
    );
}

#[test]
fn both_chords_are_inert_while_an_overlay_or_the_peek_is_open() {
    for chord in [left, right] {
        let mut m = typing();
        m.overlay = Overlay::Switcher(Query::default());
        update(&mut m, chord());
        assert!(matches!(m.overlay, Overlay::Switcher(_)));
        assert_eq!(mode(&m), Some(Mode::Agent));

        let mut m = shown();
        m.overlay = Overlay::Actions(name(AGENT));
        update(&mut m, chord());
        assert!(matches!(m.overlay, Overlay::Actions(_)));
        assert_eq!(m.panel, Panel::Agents);

        let mut m = shown();
        m.peek = Some(Peek::Pane);
        update(&mut m, chord());
        assert!(matches!(m.peek, Some(Peek::Pane)));
        assert_eq!(m.panel, Panel::Agents);
        assert_eq!(mode(&m), Some(Mode::Nav));
    }
}

#[test]
fn with_the_chords_off_they_do_nothing_in_nav_and_go_to_the_agent_in_agent() {
    let mut m = typing();
    m.keys = sendable("", Keyboard::Legacy);
    let effects = update(&mut m, left());
    assert_eq!(pty_bytes(&effects), 1);
    assert_eq!(mode(&m), Some(Mode::Agent));

    let mut m = shown();
    m.keys = sendable("", Keyboard::Legacy);
    update(&mut m, right());
    assert_eq!(mode(&m), Some(Mode::Nav));
}

#[test]
fn the_nav_key_still_leaves_agent_while_the_chords_are_on() {
    let mut m = typing();
    update(&mut m, key_with(KeyCode::Char(']'), KeyModifiers::CONTROL));
    assert_eq!(mode(&m), Some(Mode::Nav));
}
