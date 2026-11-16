use super::*;
use crate::fixtures::{connect, in_nvim, sendable};
use crate::keyspec::Keyboard;
use crate::model::{Mode, Peek};

const AGENT: &str = "fix";

fn nav() -> Msg {
    key_with(KeyCode::Char(']'), KeyModifiers::CONTROL)
}

fn on() -> Model {
    let mut m = model(&[]);
    with_activity(&mut m);
    with_changes(&mut m, 3);
    m
}

fn shown() -> Model {
    let mut m = on();
    connect(&mut m, AGENT, Mode::Nav);
    m
}

fn mode(m: &Model) -> Option<Mode> {
    m.pane.connection().map(|c| c.mode)
}

fn said(m: &Model) -> Option<String> {
    m.status.as_ref().map(|status| status.text.clone())
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

fn outcome(m: &Model, effects: &[Effect]) -> (String, Option<String>, usize, Panel) {
    (pending(m), said(m), effects.len(), m.panel.clone())
}

fn same_as_enter(build: impl Fn() -> Model) -> (Model, Vec<Effect>) {
    let mut by_enter = build();
    let enter_effects = update(&mut by_enter, key(KeyCode::Enter));
    let mut by_nav = build();
    let nav_effects = update(&mut by_nav, nav());
    assert_eq!(
        outcome(&by_nav, &nav_effects),
        outcome(&by_enter, &enter_effects)
    );
    (by_nav, nav_effects)
}

#[test]
fn the_nav_key_in_nav_enters_the_shown_agent_as_enter_does() {
    let (m, _) = same_as_enter(shown);
    assert_eq!(mode(&m), Some(Mode::Agent));
}

#[test]
fn the_nav_key_with_nothing_connected_starts_the_check_enter_starts() {
    let (m, _) = same_as_enter(|| {
        let mut m = on();
        m.pane = Pane::Empty;
        m
    });
    assert!(pending(&m).starts_with("checking Enter"), "{}", pending(&m));
}

#[test]
fn the_nav_key_on_an_agent_shown_in_another_window_moves_here_as_enter_does() {
    let (m, _) = same_as_enter(|| {
        let mut m = on();
        m.pane = Pane::Elsewhere { name: name(AGENT) };
        m.record_company(&name(AGENT), in_nvim());
        m
    });
    assert_eq!(pending(&m), "connecting MoveHere { from: Agents }");
}

#[test]
fn the_nav_key_is_refused_on_this_tuis_own_session_as_enter_refuses() {
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
fn the_nav_key_is_refused_on_an_agent_still_starting_as_enter_refuses() {
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
fn the_nav_key_enters_the_selected_agent_from_any_list_panel() {
    for key_for_panel in ['2', '3'] {
        let mut m = shown();
        update(&mut m, key(KeyCode::Char(key_for_panel)));
        update(&mut m, nav());
        assert_eq!(mode(&m), Some(Mode::Agent), "panel {key_for_panel}");
    }
}

#[test]
fn the_nav_key_in_agent_leaves_and_pressing_it_again_enters_again() {
    let mut m = shown();
    update(&mut m, nav());
    assert_eq!(mode(&m), Some(Mode::Agent));
    update(&mut m, nav());
    assert_eq!(mode(&m), Some(Mode::Nav));
    update(&mut m, nav());
    assert_eq!(mode(&m), Some(Mode::Agent));
}

#[test]
fn the_nav_key_on_a_repo_row_or_with_no_selection_does_nothing() {
    let mut m = shown();
    update(&mut m, key(KeyCode::Char('[')));
    update(&mut m, key(KeyCode::Char('g')));
    assert!(matches!(m.cursor(), Some(Cursor::Repo(_))));
    let effects = update(&mut m, nav());
    assert!(effects.is_empty());
    assert_eq!(said(&m), None);
    assert_eq!(mode(&m), Some(Mode::Nav));

    let mut m = crate::fixtures::fleet(0);
    m.pane = Pane::Empty;
    let effects = update(&mut m, nav());
    assert!(effects.is_empty());
    assert!(matches!(m.pane, Pane::Empty));
}

#[test]
fn the_escape_hatch_only_leaves_and_never_enters() {
    let mut m = shown();
    m.keys = sendable("nav = 'alt-x'", Keyboard::Legacy);
    update(&mut m, key_with(KeyCode::Char('x'), KeyModifiers::ALT));
    assert_eq!(mode(&m), Some(Mode::Agent));
    update(&mut m, nav());
    assert_eq!(mode(&m), Some(Mode::Nav));
    update(&mut m, nav());
    assert_eq!(mode(&m), Some(Mode::Nav));
}

#[test]
fn the_nav_key_is_inert_while_an_overlay_or_the_peek_is_open() {
    let mut m = shown();
    m.overlay = Overlay::Switcher(Query::default());
    update(&mut m, nav());
    assert_eq!(mode(&m), Some(Mode::Nav));

    let mut m = shown();
    m.overlay = Overlay::Actions(name(AGENT));
    update(&mut m, nav());
    assert!(matches!(m.overlay, Overlay::Actions(_)));
    assert_eq!(mode(&m), Some(Mode::Nav));

    let mut m = shown();
    m.peek = Some(Peek::Pane);
    update(&mut m, nav());
    assert!(matches!(m.peek, Some(Peek::Pane)));
    assert_eq!(mode(&m), Some(Mode::Nav));
}
