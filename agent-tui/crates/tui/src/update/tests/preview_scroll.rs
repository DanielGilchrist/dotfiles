use super::*;
use crate::fixtures::{connect, connect_mouse};
use crate::model::Mode;

fn ctrl(c: char) -> Msg {
    key_with(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn shown_with(connect: impl Fn(&mut Model, &str, Mode)) -> Model {
    let mut m = model(&[]);
    with_activity(&mut m);
    connect(&mut m, "fix", Mode::Nav);
    m
}

fn press(m: &mut Model, msg: Msg) -> Vec<String> {
    m.areas.terminal = ratatui::layout::Rect::new(30, 0, 80, 24);
    sent(&update(m, msg))
}

fn sent(effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Pty { bytes, .. } => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        })
        .collect()
}

#[test]
fn ctrl_d_and_u_scroll_the_previewed_agent_by_wheel_notches() {
    let mut m = shown_with(connect_mouse);
    let up = press(&mut m, ctrl('u'));
    let down = press(&mut m, ctrl('d'));
    eprintln!(
        "panel {:?} focus-nav {} up {up:?}",
        m.panel,
        m.pane.connection().is_some()
    );
    assert_eq!(down.len(), 1);
    assert!(down[0].starts_with("\x1b[<65;"), "{down:?}");
    assert_eq!(down[0].matches("\x1b[<65;").count(), 4);
    assert!(up[0].starts_with("\x1b[<64;"), "{up:?}");
}

#[test]
fn an_agent_that_did_not_ask_for_the_mouse_hears_nothing() {
    let mut m = shown_with(connect);
    assert_eq!(press(&mut m, ctrl('d')).len(), 0);
}
