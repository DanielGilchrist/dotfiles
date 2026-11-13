//! What a pressed key is worth as a leave key or an agent chord.

use agent_core::config::{KeyField, Keys};
use agent_core::keyspec::KeySpec;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::decode::RawKey;
use super::known::{self, Collision};
use crate::keyspec::{Keyboard, Sendable, typed};

/// A key whose bytes are also another key's, so a terminal can't tell the
/// two apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookalike {
    Backspace,
    Tab,
    Enter,
    Escape,
}

impl Lookalike {
    fn of(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [0x08 | 0x7f] => Some(Self::Backspace),
            [0x09] => Some(Self::Tab),
            [0x0a | 0x0d] => Some(Self::Enter),
            [0x1b] | [0x1b, 0x1b] => Some(Self::Escape),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Backspace => "Backspace",
            Self::Tab => "Tab",
            Self::Enter => "Enter",
            Self::Escape => "Esc",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The TUI would match it in this terminal. `rare` are known uses of the
    /// key nobody is likely to hit.
    Usable {
        spec: KeySpec,
        rare: Option<Collision>,
    },
    /// The TUI would match it, and a program the user likely runs claims it.
    Collides {
        spec: KeySpec,
        with: Collision,
    },
    Ambiguous {
        same_as: Lookalike,
    },
    /// Option made a character instead of an Alt chord.
    Composed(char),
    /// No spec names the key, or the TUI wouldn't match it.
    Unsendable,
}

pub fn classify(key: &RawKey, keyboard: Keyboard) -> Verdict {
    if let Some(same_as) = Lookalike::of(&key.bytes) {
        return Verdict::Ambiguous { same_as };
    }
    let Some(event) = key.event else {
        return Verdict::Unsendable;
    };
    if let Some(c) = composed(&event) {
        return Verdict::Composed(c);
    }
    let Some(spec) = sendable_spec(&event, keyboard) else {
        return Verdict::Unsendable;
    };
    match known::claims_on(spec) {
        Some(with) if with.is_common() => Verdict::Collides { spec, with },
        rare => Verdict::Usable { spec, rare },
    }
}

fn composed(event: &KeyEvent) -> Option<char> {
    let plain = (event.modifiers - KeyModifiers::SHIFT).is_empty();
    match event.code {
        KeyCode::Char(c) if plain && !c.is_ascii() => Some(c),
        _ => None,
    }
}

/// The spec the TUI would match against this very event in this terminal.
fn sendable_spec(event: &KeyEvent, keyboard: Keyboard) -> Option<KeySpec> {
    let candidates = match typed(event).0 {
        KeyCode::Char(c) => KeySpec::on_character(c),
        KeyCode::F(number) => KeySpec::function(number).into_iter().collect(),
        _ => Vec::new(),
    };
    candidates.into_iter().find(|spec| {
        Sendable::resolve(Keys::default().with_nav(*spec), keyboard).pressed(KeyField::Nav, event)
    })
}

#[cfg(test)]
mod tests {
    use super::super::decode;
    use super::*;

    fn pressed(bytes: &[u8]) -> RawKey {
        let mut keys = decode::all(bytes);
        assert_eq!(keys.len(), 1, "{bytes:?} is one key");
        keys.remove(0)
    }

    fn spec(text: &str) -> KeySpec {
        text.parse().expect("parses")
    }

    fn verdict(bytes: &[u8], keyboard: Keyboard) -> Verdict {
        classify(&pressed(bytes), keyboard)
    }

    fn usable(text: &str) -> Verdict {
        Verdict::Usable {
            spec: spec(text),
            rare: None,
        }
    }

    const LEGACY: Keyboard = Keyboard::Legacy;
    const KITTY: Keyboard = Keyboard::Disambiguated;

    #[test]
    fn a_key_nobody_is_known_to_use_that_the_terminal_sends_is_usable() {
        for (bytes, text) in [
            (&b"\x1b,"[..], "alt-,"),
            (b"\x1b;", "alt-;"),
            (b"\x1b[24~", "f12"),
            (b"\x1b[15~", "f5"),
            (&[0x1c], "ctrl-\\"),
        ] {
            let expected = match verdict(bytes, LEGACY) {
                Verdict::Usable { spec: found, .. } => found,
                other => panic!("{text}: {other:?}"),
            };
            assert_eq!(expected, spec(text));
        }
        assert_eq!(verdict(b"\x1b,", LEGACY), usable("alt-,"));
    }

    #[test]
    fn the_four_special_control_keys_are_one_spec_in_either_mode() {
        for (legacy, kitty, text) in [
            (&[0x1c][..], &b"\x1b[92;5u"[..], "ctrl-\\"),
            (&[0x1d], b"\x1b[93;5u", "ctrl-]"),
            (&[0x1e], b"\x1b[94;5u", "ctrl-^"),
            (&[0x1f], b"\x1b[95;5u", "ctrl-_"),
        ] {
            for (bytes, keyboard) in [(legacy, LEGACY), (kitty, KITTY)] {
                let found = match verdict(bytes, keyboard) {
                    Verdict::Usable { spec, .. } | Verdict::Collides { spec, .. } => spec,
                    other => panic!("{text}: {other:?}"),
                };
                assert_eq!(found, spec(text), "{text} in {keyboard:?}");
            }
        }
    }

    #[test]
    fn a_key_only_rarely_used_is_usable_and_carries_the_rare_uses() {
        let Verdict::Usable { spec: found, rare } = verdict(&[0x1d], LEGACY) else {
            panic!("ctrl-] is usable");
        };
        assert_eq!(found, spec("ctrl-]"));
        assert!(rare.is_some());
    }

    #[test]
    fn a_key_other_programs_commonly_use_collides_and_names_them() {
        let found = verdict(&[0x12], LEGACY);
        let Verdict::Collides { spec: key, with } = found else {
            panic!("{found:?}");
        };
        assert_eq!(key, spec("ctrl-r"));
        assert_eq!(with.claims().count(), 1);
        assert!(matches!(
            verdict(b"\x1bb", LEGACY),
            Verdict::Collides { .. }
        ));
        assert!(matches!(
            verdict(&[0x00], LEGACY),
            Verdict::Usable { rare: Some(_), .. }
        ));
    }

    #[test]
    fn a_key_with_the_same_bytes_as_tab_enter_escape_or_backspace_is_ambiguous() {
        for (bytes, same_as) in [
            (&[0x09][..], Lookalike::Tab),
            (&[0x0d], Lookalike::Enter),
            (&[0x0a], Lookalike::Enter),
            (&[0x08], Lookalike::Backspace),
            (&[0x7f], Lookalike::Backspace),
            (&[0x1b], Lookalike::Escape),
            (&[0x1b, 0x1b], Lookalike::Escape),
        ] {
            for keyboard in [LEGACY, KITTY] {
                assert_eq!(
                    verdict(bytes, keyboard),
                    Verdict::Ambiguous { same_as },
                    "{bytes:?} in {keyboard:?}, where Tab and Enter keep their bytes"
                );
            }
        }
    }

    #[test]
    fn a_key_that_shares_bytes_with_another_in_legacy_is_told_apart_under_kitty() {
        assert_eq!(verdict(b"\x1b[105;5u", KITTY), usable("ctrl-i"));
        assert_eq!(verdict(b"\x1b[106;5u", KITTY), usable("ctrl-j"));
        assert!(matches!(
            verdict(b"\x1b[104;5u", KITTY),
            Verdict::Collides { spec: found, .. } if found == spec("ctrl-h")
        ));
    }

    #[test]
    fn ctrl_shift_and_super_chords_are_usable_under_kitty() {
        assert_eq!(verdict(b"\x1b[106:74;6u", KITTY), usable("ctrl-shift-j"));
        assert_eq!(verdict(b"\x1b[107;9u", KITTY), usable("super-k"));
    }

    #[test]
    fn a_composed_character_says_option_was_not_alt() {
        assert_eq!(verdict("≤".as_bytes(), LEGACY), Verdict::Composed('≤'));
        assert_eq!(verdict("≥".as_bytes(), KITTY), Verdict::Composed('≥'));
    }

    #[test]
    fn keys_no_spec_can_name_are_unsendable_and_never_composed() {
        for bytes in [
            &b"x"[..],
            b"\x1b[1;5A",
            b"\x1b[27u",
            b"\x1b[1;2P",
            b"\x1b[57376u",
            b"\x1b[57399u",
            b"\x1b[97;17u",
            b"\x1b[[A",
        ] {
            assert_eq!(verdict(bytes, KITTY), Verdict::Unsendable, "{bytes:?}");
        }
        assert_eq!(verdict(b"\x1b[A", LEGACY), Verdict::Unsendable);
    }

    #[test]
    fn a_function_key_with_a_modifier_is_unsendable() {
        assert_eq!(verdict(b"\x1b[24;5~", LEGACY), Verdict::Unsendable);
    }
}
