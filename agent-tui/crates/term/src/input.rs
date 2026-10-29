use std::num::NonZeroU16;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::{Modes, MouseReporting};

/// A 1-based cell inside a terminal area, as mouse protocols report it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellPos {
    pub col: NonZeroU16,
    pub row: NonZeroU16,
}

impl CellPos {
    pub fn within(area: Rect, at: Position) -> Option<Self> {
        if !area.contains(at) {
            return None;
        }
        Some(Self {
            col: NonZeroU16::new(at.x - area.x + 1)?,
            row: NonZeroU16::new(at.y - area.y + 1)?,
        })
    }
}

fn modifier_param(mods: KeyModifiers) -> u8 {
    1 + u8::from(mods.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(mods.contains(KeyModifiers::ALT))
        + 4 * u8::from(mods.contains(KeyModifiers::CONTROL))
}

fn control_byte(c: char) -> Option<u8> {
    match c.to_ascii_lowercase() {
        c @ 'a'..='z' => Some(c as u8 & 0x1f),
        ' ' | '@' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        _ => None,
    }
}

/// The bytes an xterm-compatible terminal sends for `key`.
pub fn encode_key(key: KeyEvent, modes: Modes) -> Vec<u8> {
    let mods = key.modifiers;
    let param = modifier_param(mods);
    let cursor = |final_byte: char| -> Vec<u8> {
        match (param, modes.application_cursor) {
            (2.., _) => format!("\x1b[1;{param}{final_byte}"),
            (_, true) => format!("\x1bO{final_byte}"),
            (_, false) => format!("\x1b[{final_byte}"),
        }
        .into_bytes()
    };
    let tilde = |n: u8| -> Vec<u8> {
        if param > 1 {
            format!("\x1b[{n};{param}~")
        } else {
            format!("\x1b[{n}~")
        }
        .into_bytes()
    };

    let mut bytes = match key.code {
        KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => {
            control_byte(c).map_or_else(|| c.to_string().into_bytes(), |b| vec![b])
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter if mods.contains(KeyModifiers::SHIFT) => b"\x1b\r".to_vec(),
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => return cursor('A'),
        KeyCode::Down => return cursor('B'),
        KeyCode::Right => return cursor('C'),
        KeyCode::Left => return cursor('D'),
        KeyCode::Home => return cursor('H'),
        KeyCode::End => return cursor('F'),
        KeyCode::Insert => return tilde(2),
        KeyCode::Delete => return tilde(3),
        KeyCode::PageUp => return tilde(5),
        KeyCode::PageDown => return tilde(6),
        KeyCode::F(n @ 1..=4) => format!("\x1bO{}", char::from(b'P' + n - 1)).into_bytes(),
        KeyCode::F(n @ 5..=12) => {
            return tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]);
        }
        _ => return Vec::new(),
    };
    if mods.contains(KeyModifiers::ALT) {
        bytes.insert(0, 0x1b);
    }
    bytes
}

/// SGR mouse report for `kind` at `at`, if the program asked for mouse input.
pub fn encode_mouse(kind: MouseEventKind, at: CellPos, modes: Modes) -> Option<Vec<u8>> {
    if modes.mouse != MouseReporting::Sgr {
        return None;
    }
    let button = |b: MouseButton| match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (code, suffix) = match kind {
        MouseEventKind::Down(b) => (button(b), 'M'),
        MouseEventKind::Up(b) => (button(b), 'm'),
        MouseEventKind::Drag(b) => (button(b) + 32, 'M'),
        MouseEventKind::ScrollUp => (64, 'M'),
        MouseEventKind::ScrollDown => (65, 'M'),
        _ => return None,
    };
    Some(format!("\x1b[<{code};{};{}{suffix}", at.col, at.row).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn control_keys_map_to_c0_bytes() {
        assert_eq!(
            encode_key(
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                Modes::default()
            ),
            [0x03]
        );
        assert_eq!(
            encode_key(
                key(KeyCode::Char('4'), KeyModifiers::CONTROL),
                Modes::default()
            ),
            [0x1c]
        );
    }

    #[test]
    fn arrows_respect_application_cursor_and_modifiers() {
        let app = Modes {
            application_cursor: true,
            ..Modes::default()
        };
        assert_eq!(
            encode_key(key(KeyCode::Up, KeyModifiers::NONE), Modes::default()),
            b"\x1b[A"
        );
        assert_eq!(
            encode_key(key(KeyCode::Up, KeyModifiers::NONE), app),
            b"\x1bOA"
        );
        assert_eq!(
            encode_key(key(KeyCode::Up, KeyModifiers::SHIFT), app),
            b"\x1b[1;2A"
        );
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(
            encode_key(key(KeyCode::Char('x'), KeyModifiers::ALT), Modes::default()),
            b"\x1bx"
        );
    }

    #[test]
    fn cell_positions_are_one_based_and_bounded() {
        let area = Rect::new(10, 5, 4, 2);
        let at = CellPos::within(area, Position::new(10, 5)).expect("succeeds");
        assert_eq!((at.col.get(), at.row.get()), (1, 1));
        assert!(CellPos::within(area, Position::new(9, 5)).is_none());
        assert!(CellPos::within(area, Position::new(14, 5)).is_none());
    }

    #[test]
    fn mouse_reports_only_when_requested() {
        let at = CellPos::within(Rect::new(0, 0, 10, 10), Position::new(2, 3)).expect("succeeds");
        assert!(encode_mouse(MouseEventKind::ScrollUp, at, Modes::default()).is_none());
        let sgr = Modes {
            mouse: MouseReporting::Sgr,
            ..Modes::default()
        };
        assert_eq!(
            encode_mouse(MouseEventKind::ScrollUp, at, sgr).expect("succeeds"),
            b"\x1b[<64;3;4M"
        );
    }
}
