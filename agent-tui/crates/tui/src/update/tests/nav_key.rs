use super::*;
use crate::fixtures::{connect, fleet};
use crate::keyspec::{Keyboard, Sendable};
use crate::model::Mode;
use agent_core::config::Config;

const WHO: &str = "auth-refresh-2";
const CTRL: KeyModifiers = KeyModifiers::CONTROL;

fn config_keys(nav: &str) -> agent_core::config::Keys {
    let config: Config = format!("[keys]\nnav = '{nav}'").parse().expect("parses");
    config.keys
}

fn attached(nav: &str, keyboard: Keyboard) -> Model {
    let mut m = fleet(10);
    connect(&mut m, WHO, Mode::Agent);
    let config = config_keys(nav);
    m.keys = Sendable::resolve(config, keyboard);
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
    m.pane
        .connection()
        .is_some_and(|connection| connection.mode == Mode::Agent)
}

fn leaves(m: &mut Model, event: Msg) -> bool {
    let effects = update(m, event);
    assert_eq!(pty_bytes(&effects), Vec::<Vec<u8>>::new());
    !in_agent(m)
}

#[test]
fn the_default_nav_key_leaves_agent_in_the_encoding_the_terminal_sends() {
    let mut legacy = attached("ctrl-]", Keyboard::Legacy);
    assert!(leaves(&mut legacy, key_with(KeyCode::Char('5'), CTRL)));
    let mut kitty = attached("ctrl-]", Keyboard::Disambiguated);
    assert!(leaves(&mut kitty, key_with(KeyCode::Char(']'), CTRL)));
}

#[test]
fn under_kitty_ctrl_5_is_the_agents_and_does_not_leave() {
    let mut m = attached("ctrl-]", Keyboard::Disambiguated);
    let effects = update(&mut m, key_with(KeyCode::Char('5'), CTRL));
    assert_eq!(pty_bytes(&effects).len(), 1);
    assert!(in_agent(&m));
}

#[test]
fn the_nav_key_with_another_modifier_held_goes_to_the_agent() {
    let mut m = attached("ctrl-]", Keyboard::Legacy);
    let effects = update(
        &mut m,
        key_with(KeyCode::Char(']'), CTRL | KeyModifiers::ALT),
    );
    assert_eq!(pty_bytes(&effects).len(), 1);
    assert!(in_agent(&m));
}

#[test]
fn keys_other_than_nav_reach_the_agent_as_the_bytes_they_encode_to() {
    let mut m = attached("ctrl-]", Keyboard::Legacy);
    let sent = [
        (key(KeyCode::Char('x')), b"x".to_vec()),
        (key(KeyCode::Char(']')), b"]".to_vec()),
        (key(KeyCode::Enter), b"\r".to_vec()),
        (key_with(KeyCode::Char('c'), CTRL), vec![0x03]),
    ];
    for (event, bytes) in sent {
        let effects = update(&mut m, event);
        assert_eq!(pty_bytes(&effects), [bytes]);
        assert!(in_agent(&m));
    }
}

#[test]
fn ctrl_backslash_is_the_agents_own_key_unless_it_is_the_configured_nav_key() {
    let backslash = || key_with(KeyCode::Char('\\'), CTRL);

    let mut default = attached("ctrl-]", Keyboard::Legacy);
    let effects = update(&mut default, backslash());
    assert_eq!(pty_bytes(&effects), [vec![0x1c]]);
    assert!(in_agent(&default));

    let mut old = attached("ctrl-\\", Keyboard::Legacy);
    assert!(leaves(&mut old, backslash()));
}

#[test]
fn a_configured_control_nav_key_leaves_agent_and_the_default_then_reaches_the_agent() {
    let mut m = attached("ctrl-g", Keyboard::Legacy);
    let effects = update(&mut m, key_with(KeyCode::Char('5'), CTRL));
    assert_eq!(pty_bytes(&effects), [vec![0x1d]]);
    assert!(in_agent(&m));
    assert!(leaves(&mut m, key_with(KeyCode::Char('g'), CTRL)));
}

#[test]
fn a_kitty_only_nav_key_on_a_legacy_terminal_leaves_through_the_default_instead_of_stranding() {
    let mut m = attached("ctrl-shift-j", Keyboard::Legacy);
    assert!(leaves(&mut m, key_with(KeyCode::Char('5'), CTRL)));
}

#[test]
fn a_kitty_only_nav_key_leaves_agent_when_the_terminal_reports_the_protocol() {
    let mut m = attached("ctrl-shift-j", Keyboard::Disambiguated);
    assert!(leaves(&mut m, key_with(KeyCode::Char('J'), CTRL)));
}

#[test]
fn a_nav_key_that_never_arrives_still_leaves_through_ctrl_close_bracket() {
    for nav in ["ctrl-shift-t", "super-c", "alt-x", "f5"] {
        let mut m = attached(nav, Keyboard::Disambiguated);
        let swallowed = update(&mut m, key(KeyCode::Char('z')));
        assert_eq!(
            pty_bytes(&swallowed).len(),
            1,
            "{nav}: the agent keeps its own keys"
        );
        assert!(in_agent(&m), "{nav}");
        assert!(leaves(&mut m, key_with(KeyCode::Char(']'), CTRL)), "{nav}");
    }
}

#[test]
fn a_control_byte_nav_key_leaves_ctrl_close_bracket_to_the_agent() {
    let mut m = attached("ctrl-\\", Keyboard::Legacy);
    let effects = update(&mut m, key_with(KeyCode::Char('5'), CTRL));
    assert_eq!(pty_bytes(&effects), [vec![0x1d]]);
    assert!(in_agent(&m));
}

#[test]
fn the_fallback_warning_set_at_startup_survives_keys_and_the_nav_key_until_its_time_is_up() {
    let mut m = fleet(3);
    crate::use_keys(&mut m, Keyboard::Legacy, config_keys("ctrl-shift-t"));
    connect(&mut m, WHO, Mode::Agent);
    let said = "keys.nav ⌃⇧T is not available in this terminal, using ⌃]";
    let status = |m: &Model| m.status.as_ref().map(|s| (s.kind, s.text.clone()));
    let warned = Some((crate::model::StatusKind::Warning, said.to_string()));
    assert_eq!(status(&m), warned);
    update(&mut m, key_with(KeyCode::Char('5'), CTRL));
    assert!(!in_agent(&m), "the default nav key leaves");
    update(&mut m, key(KeyCode::Char('j')));
    assert_eq!(status(&m), warned, "keys don't clear it");
    let almost = later(&m, crate::model::WARNING_TTL);
    tick_at(&mut m, almost);
    assert_eq!(status(&m), warned, "kept for exactly its time to live");
    let over = later(&m, Duration::from_millis(1));
    tick_at(&mut m, over);
    assert_eq!(status(&m), None);
}

#[test]
fn an_info_message_replaces_the_warning_and_the_fallback_stays_in_the_help_notes() {
    let mut m = fleet(3);
    crate::use_keys(&mut m, Keyboard::Legacy, config_keys("ctrl-shift-t"));
    m.info("send is s now");
    let status = m.status.as_ref().expect("a status");
    assert_eq!(status.kind, crate::model::StatusKind::Info);
    assert_eq!(status.text, "send is s now");
    assert_eq!(
        m.keys.notes(),
        ["keys.nav ⌃⇧T is not available in this terminal, using ⌃]"]
    );
}

#[test]
fn an_error_replaces_the_warning() {
    let mut m = fleet(3);
    crate::use_keys(&mut m, Keyboard::Legacy, config_keys("ctrl-shift-t"));
    update(
        &mut m,
        Msg::Snapshot(Ok((snapshot(Vec::new(), &[]), vec!["bad repo".into()]))),
    );
    let status = m.status.as_ref().expect("a status");
    assert_eq!(status.kind, crate::model::StatusKind::Error);
    assert!(status.text.contains("bad repo"), "{}", status.text);
    assert_eq!(
        m.keys.notes(),
        ["keys.nav ⌃⇧T is not available in this terminal, using ⌃]"]
    );
}
