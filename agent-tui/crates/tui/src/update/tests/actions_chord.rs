use super::*;
use crate::fixtures::{connect, fleet, sendable};
use crate::keyspec::Keyboard;
use crate::model::{Mode, Visibility};

const ALT: KeyModifiers = KeyModifiers::ALT;
const AGENT: &str = "auth-refresh-2";

fn chord() -> Msg {
    key_with(KeyCode::Char('x'), ALT)
}

fn configured(m: &mut Model, keys: &str) {
    m.keys = sendable(keys, Keyboard::Legacy);
    m.actions = r#"
        [[action]]
        key = "t"
        label = "tests"
        run = ["bin/test", "{name}"]
    "#
    .parse::<agent_core::config::Config>()
    .expect("parses")
    .actions;
}

fn on(agent: &str) -> Model {
    let mut m = fleet(10);
    m.visibility = Visibility::All;
    m.set_cursor(Cursor::Entry(name(agent)));
    configured(&mut m, "actions = 'alt-x'");
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

fn menu_target(m: &Model) -> Option<String> {
    match &m.overlay {
        Overlay::Actions(target) => Some(target.to_string()),
        _ => None,
    }
}

fn said(m: &Model) -> Option<String> {
    m.status.as_ref().map(|status| status.text.clone())
}

#[test]
fn in_nav_the_chord_opens_the_menu_for_the_selected_agent() {
    let mut m = on(AGENT);
    let effects = update(&mut m, chord());
    assert_eq!(menu_target(&m).as_deref(), Some(AGENT));
    assert!(effects.is_empty());
}

#[test]
fn in_agent_it_opens_the_menu_for_the_agent_typed_into_and_sends_nothing() {
    let mut m = typing_into(AGENT);
    let other = m
        .items()
        .iter()
        .filter_map(Cursor::entry_name)
        .find(|entry| entry.as_str() != AGENT)
        .cloned()
        .expect("another agent");
    m.set_cursor(Cursor::Entry(other));
    let effects = update(&mut m, chord());
    assert_eq!(menu_target(&m).as_deref(), Some(AGENT));
    assert_eq!(pty_bytes(&effects), Vec::<Vec<u8>>::new());
}

#[test]
fn closing_the_menu_leaves_the_user_typing_in_the_same_agent() {
    let mut m = typing_into(AGENT);
    update(&mut m, chord());
    let effects = update(&mut m, key(KeyCode::Esc));
    assert!(matches!(m.overlay, Overlay::None));
    assert!(pty_bytes(&effects).is_empty(), "Esc went to the menu");
    let connection = m.pane.connection().expect("still connected");
    assert_eq!(
        (connection.name.as_str(), connection.mode),
        (AGENT, Mode::Agent)
    );
    let effects = update(&mut m, key(KeyCode::Char('q')));
    assert_eq!(pty_bytes(&effects), [b"q".to_vec()]);
}

#[test]
fn keys_go_to_the_menu_while_it_is_open_in_agent() {
    let mut m = typing_into(AGENT);
    update(&mut m, chord());
    let effects = update(&mut m, key(KeyCode::Char('t')));
    assert_eq!(pty_bytes(&effects), Vec::<Vec<u8>>::new());
    assert!(matches!(effects.as_slice(), [Effect::Job(Job::Run { .. })]));
    assert!(matches!(m.overlay, Overlay::None));
    assert!(matches!(
        m.pane.connection().map(|c| c.mode),
        Some(Mode::Agent)
    ));
}

#[test]
fn the_chord_is_off_by_default_and_then_goes_to_the_agent() {
    let mut m = typing_into(AGENT);
    configured(&mut m, "");
    let effects = update(&mut m, chord());
    assert!(matches!(m.overlay, Overlay::None));
    assert_eq!(pty_bytes(&effects).len(), 1);
}

#[test]
fn it_does_nothing_while_an_overlay_or_the_peek_is_open() {
    let mut m = on(AGENT);
    m.overlay = Overlay::Switcher(Query::default());
    update(&mut m, chord());
    assert!(matches!(m.overlay, Overlay::Switcher(_)));

    let mut m = on(AGENT);
    m.peek = Some(crate::model::Peek::Pane);
    update(&mut m, chord());
    assert!(matches!(m.overlay, Overlay::None));
    assert!(matches!(m.peek, Some(crate::model::Peek::Pane)));
}

#[test]
fn it_says_why_when_there_is_nothing_to_open() {
    let mut m = on(AGENT);
    m.actions = agent_core::actions::Actions::default();
    update(&mut m, chord());
    assert!(said(&m).is_some_and(|text| text.starts_with("no actions")));

    let mut m = fleet(0);
    configured(&mut m, "actions = 'alt-x'");
    update(&mut m, chord());
    assert_eq!(
        said(&m).as_deref(),
        Some("select an agent to run an action on")
    );
}

#[test]
fn it_is_refused_while_the_agent_is_restarting() {
    let mut m = on(AGENT);
    m.pane = Pane::Empty;
    let mut batch = crate::model::Batch::default();
    batch.add([name(AGENT)]);
    m.restarts = Some(batch);
    update(&mut m, chord());
    assert!(matches!(m.overlay, Overlay::None));
    assert!(said(&m).is_some_and(|text| text.ends_with("is restarting")));
}

#[test]
fn a_paste_with_the_menu_open_over_an_agent_is_dropped_not_typed_into_it() {
    let mut m = typing_into(AGENT);
    update(&mut m, chord());
    assert!(matches!(m.overlay, Overlay::Actions(_)));
    let effects = update(&mut m, Msg::Paste("rm -rf".into()));
    assert_eq!(pty_bytes(&effects), Vec::<Vec<u8>>::new());
    assert!(said(&m).is_some_and(|text| text.contains("paste not taken")));
}
