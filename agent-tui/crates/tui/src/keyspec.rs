//! The configured keys resolved against the terminal, and what each one
//! matches among the events crossterm reports in that terminal.

use std::fmt;

use agent_core::config::{Hop, KeyField, Keys};
use agent_core::keyspec::{Binding, Control, KeySpec, KittyKey};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// How the terminal reports keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyboard {
    /// The kitty keyboard protocol tells `⇧⏎` from `⏎`, and reports the
    /// shifted character of `⌥⇧` keys, which crossterm puts in the event.
    Disambiguated,
    Legacy,
}

/// A configured key the terminal can't send, replaced by its default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fallback {
    field: KeyField,
    wanted: KeySpec,
    using: Binding,
}

impl fmt::Display for Fallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "keys.{} {} is not available in this terminal, ",
            self.field, self.wanted
        )?;
        match self.using {
            Binding::On(spec) => write!(f, "using {spec}"),
            Binding::Off | Binding::Taken { .. } => f.write_str("leaving it off"),
        }
    }
}

/// Keys the terminal can send, and the ones that were replaced because it
/// can't. It is the only thing that matches key events against them, so a
/// key that could never arrive can't be one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sendable {
    keys: Keys,
    keyboard: Keyboard,
    fallbacks: Vec<Fallback>,
}

impl Default for Sendable {
    fn default() -> Self {
        Self::resolve(Keys::default(), Keyboard::Legacy)
    }
}

impl Sendable {
    pub fn resolve(configured: Keys, keyboard: Keyboard) -> Self {
        let mut keys = configured;
        let mut fallbacks = Vec::new();
        for field in KeyField::all() {
            let Binding::On(wanted) = configured.binding(field) else {
                continue;
            };
            if wanted.needs_kitty() && keyboard == Keyboard::Legacy {
                keys = keys.reset(field);
                fallbacks.push(Fallback {
                    field,
                    wanted,
                    using: keys.binding(field),
                });
            }
        }
        Self {
            keys,
            keyboard,
            fallbacks,
        }
    }

    pub fn nav(&self) -> KeySpec {
        self.keys.nav()
    }

    pub fn hop(&self, hop: Hop) -> Binding {
        self.keys.hop(hop)
    }

    pub fn escape_hatch(&self) -> Option<KeySpec> {
        self.keys.escape_hatch()
    }

    pub fn hopped(&self, key: &KeyEvent) -> Option<Hop> {
        Hop::ALL
            .into_iter()
            .find(|hop| self.pressed(KeyField::Hop(*hop), key))
    }

    pub fn leaves_agent(&self, key: &KeyEvent) -> bool {
        self.pressed(KeyField::Nav, key)
            || self
                .escape_hatch()
                .is_some_and(|hatch| self.matches(hatch, key))
    }

    pub fn pressed(&self, field: KeyField, key: &KeyEvent) -> bool {
        self.keys
            .binding(field)
            .spec()
            .is_some_and(|spec| self.matches(spec, key))
    }

    fn matches(&self, spec: KeySpec, key: &KeyEvent) -> bool {
        let (code, modifiers) = typed(key);
        let only = |expected| modifiers == expected;
        match spec {
            KeySpec::Ctrl(control) => only(KeyModifiers::CONTROL) && self.ctrl(control, code),
            KeySpec::Alt(glyph) => only(KeyModifiers::ALT) && code == KeyCode::Char(glyph.char()),
            KeySpec::Function(function) => {
                only(KeyModifiers::NONE) && code == KeyCode::F(function.number())
            }
            KeySpec::Kitty(KittyKey::CtrlShift(letter)) => {
                only(KeyModifiers::CONTROL)
                    && code == KeyCode::Char(letter.char().to_ascii_uppercase())
            }
            KeySpec::Kitty(KittyKey::Super(glyph)) => {
                only(KeyModifiers::SUPER) && code == KeyCode::Char(glyph.char())
            }
            KeySpec::Kitty(KittyKey::Ctrl(shadowed)) => {
                only(KeyModifiers::CONTROL) && code == KeyCode::Char(shadowed.char())
            }
            KeySpec::Kitty(KittyKey::Alt(starter)) => {
                only(KeyModifiers::ALT) && code == KeyCode::Char(starter.char())
            }
        }
    }

    fn ctrl(&self, control: Control, code: KeyCode) -> bool {
        let legacy_digit = match self.keyboard {
            Keyboard::Legacy => control.legacy_digit(),
            Keyboard::Disambiguated => None,
        };
        matches!(code, KeyCode::Char(c) if c == control.char() || Some(c) == legacy_digit)
    }

    pub fn warning(&self) -> Option<String> {
        if self.fallbacks.is_empty() {
            return None;
        }
        let lines: Vec<_> = self.fallbacks.iter().map(ToString::to_string).collect();
        Some(lines.join(" · "))
    }

    /// What the help says about keys beyond its table.
    pub fn notes(&self) -> Vec<String> {
        let hatch = self
            .escape_hatch()
            .map(|hatch| format!("{hatch} also leaves AGENT"));
        let taken = Hop::ALL.into_iter().filter_map(|hop| {
            let field = KeyField::Hop(hop);
            match self.keys.binding(field) {
                Binding::Taken { default, by }
                    if !self
                        .fallbacks
                        .iter()
                        .any(|fallback| fallback.field == field) =>
                {
                    Some(format!("{field} default {default} is taken by {by}, off"))
                }
                Binding::Taken { .. } | Binding::Off | Binding::On(_) => None,
            }
        });
        self.fallbacks
            .iter()
            .map(ToString::to_string)
            .chain(taken)
            .chain(hatch)
            .collect()
    }
}

/// What a US keyboard types with Shift held, for the keys whose shifted
/// character isn't a case change.
const SHIFTED: [(char, char); 21] = [
    ('`', '~'),
    ('1', '!'),
    ('2', '@'),
    ('3', '#'),
    ('4', '$'),
    ('5', '%'),
    ('6', '^'),
    ('7', '&'),
    ('8', '*'),
    ('9', '('),
    ('0', ')'),
    ('-', '_'),
    ('=', '+'),
    ('[', '{'),
    (']', '}'),
    ('\\', '|'),
    (';', ':'),
    ('\'', '"'),
    (',', '<'),
    ('.', '>'),
    ('/', '?'),
];

/// The character a key press types and the modifiers left over once Shift
/// has been folded into it, so a spec never has to say "with Shift".
///
/// crossterm reports a shifted key three ways: legacy as the shifted
/// character (upper case also sets Shift), the kitty protocol with
/// `REPORT_ALTERNATE_KEYS` as the shifted character with Shift cleared, and
/// the kitty protocol without an alternate as the base character with Shift.
/// All three come out as the shifted character with no Shift, so
/// `ctrl-shift-j` and `ctrl-j` can't be confused and `alt-<` matches
/// whichever one the terminal sends. Shift on a key with no shifted form
/// stays, and matches no spec.
pub(crate) fn typed(key: &KeyEvent) -> (KeyCode, KeyModifiers) {
    let KeyCode::Char(c) = key.code else {
        return (key.code, key.modifiers);
    };
    if !key.modifiers.contains(KeyModifiers::SHIFT) {
        return (key.code, key.modifiers);
    }
    let shifted = if c.is_ascii_lowercase() {
        Some(c.to_ascii_uppercase())
    } else {
        SHIFTED
            .iter()
            .find(|(base, _)| *base == c)
            .map(|(_, shifted)| *shifted)
    };
    match shifted {
        Some(shifted) => (KeyCode::Char(shifted), key.modifiers - KeyModifiers::SHIFT),
        None if c.is_uppercase() || SHIFTED.iter().any(|(_, shifted)| *shifted == c) => {
            (key.code, key.modifiers - KeyModifiers::SHIFT)
        }
        None => (key.code, key.modifiers),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(nav: &str) -> Keys {
        Keys::default().with_nav(nav.parse().expect("parses"))
    }

    fn terminal(nav: &str, keyboard: Keyboard) -> Sendable {
        Sendable::resolve(keys(nav), keyboard)
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn nav_pressed(reachable: &Sendable, code: KeyCode, modifiers: KeyModifiers) -> bool {
        reachable.pressed(KeyField::Nav, &press(code, modifiers))
    }

    const CTRL: KeyModifiers = KeyModifiers::CONTROL;
    const ALT: KeyModifiers = KeyModifiers::ALT;
    const SHIFT: KeyModifiers = KeyModifiers::SHIFT;
    const NONE: KeyModifiers = KeyModifiers::NONE;

    #[test]
    fn the_legacy_digit_stands_for_the_special_control_keys_only_on_a_legacy_terminal() {
        let digits = [
            ("ctrl-\\", '4'),
            ("ctrl-]", '5'),
            ("ctrl-^", '6'),
            ("ctrl-_", '7'),
        ];
        for (nav, own) in digits {
            for digit in ['4', '5', '6', '7'] {
                let legacy = terminal(nav, Keyboard::Legacy);
                let kitty = terminal(nav, Keyboard::Disambiguated);
                assert_eq!(
                    nav_pressed(&legacy, KeyCode::Char(digit), CTRL),
                    digit == own,
                    "{nav} on legacy against ctrl-{digit}"
                );
                assert!(
                    !nav_pressed(&kitty, KeyCode::Char(digit), CTRL),
                    "{nav} under kitty against ctrl-{digit}"
                );
            }
            let character = nav.chars().last().expect("a character");
            for keyboard in [Keyboard::Legacy, Keyboard::Disambiguated] {
                assert!(nav_pressed(
                    &terminal(nav, keyboard),
                    KeyCode::Char(character),
                    CTRL
                ));
            }
        }
    }

    #[test]
    fn a_control_key_needs_exactly_control() {
        for keyboard in [Keyboard::Legacy, Keyboard::Disambiguated] {
            let nav = terminal("ctrl-g", keyboard);
            assert!(nav_pressed(&nav, KeyCode::Char('g'), CTRL));
            assert!(!nav_pressed(&nav, KeyCode::Char('g'), NONE));
            assert!(!nav_pressed(&nav, KeyCode::Char('g'), CTRL | ALT));
            assert!(!nav_pressed(
                &nav,
                KeyCode::Char('g'),
                CTRL | KeyModifiers::SUPER
            ));
            assert!(!nav_pressed(&nav, KeyCode::Char('h'), CTRL));
            assert!(!nav_pressed(&nav, KeyCode::Char('5'), CTRL));
        }
    }

    #[test]
    fn ctrl_space_is_the_space_character_with_exactly_control() {
        let nav = terminal("ctrl-space", Keyboard::Legacy);
        assert!(nav_pressed(&nav, KeyCode::Char(' '), CTRL));
        assert!(!nav_pressed(&nav, KeyCode::Char(' '), NONE));
        assert!(!nav_pressed(&nav, KeyCode::Char(' '), CTRL | ALT));
    }

    #[test]
    fn alt_needs_exactly_alt_and_its_character_with_its_case() {
        let nav = terminal("alt-,", Keyboard::Legacy);
        assert!(nav_pressed(&nav, KeyCode::Char(','), ALT));
        assert!(!nav_pressed(&nav, KeyCode::Char(','), NONE));
        assert!(!nav_pressed(&nav, KeyCode::Char('.'), ALT));
        assert!(!nav_pressed(&nav, KeyCode::Char(','), CTRL | ALT));
        assert!(!nav_pressed(&nav, KeyCode::Char(','), ALT | SHIFT));
    }

    #[test]
    fn alt_with_an_upper_case_character_matches_it_in_every_encoding_and_not_the_lower_case() {
        for keyboard in [Keyboard::Legacy, Keyboard::Disambiguated] {
            let upper = terminal("alt-X", keyboard);
            assert!(
                nav_pressed(&upper, KeyCode::Char('X'), ALT | SHIFT),
                "legacy"
            );
            assert!(
                nav_pressed(&upper, KeyCode::Char('X'), ALT),
                "kitty alternate key"
            );
            assert!(
                nav_pressed(&upper, KeyCode::Char('x'), ALT | SHIFT),
                "kitty no alternate"
            );
            assert!(!nav_pressed(&upper, KeyCode::Char('x'), ALT));
            let lower = terminal("alt-x", keyboard);
            assert!(nav_pressed(&lower, KeyCode::Char('x'), ALT));
            assert!(!nav_pressed(&lower, KeyCode::Char('X'), ALT | SHIFT));
            assert!(!nav_pressed(&lower, KeyCode::Char('X'), ALT));
        }
    }

    #[test]
    fn a_function_key_matches_only_itself_with_no_modifier() {
        let nav = terminal("f12", Keyboard::Legacy);
        assert!(nav_pressed(&nav, KeyCode::F(12), NONE));
        assert!(!nav_pressed(&nav, KeyCode::F(11), NONE));
        for modifier in [SHIFT, CTRL, ALT] {
            assert!(!nav_pressed(&nav, KeyCode::F(12), modifier));
        }
    }

    #[test]
    fn ctrl_shift_matches_the_events_a_kitty_terminal_reports_for_it_and_not_plain_ctrl() {
        let nav = terminal("ctrl-shift-j", Keyboard::Disambiguated);
        assert!(
            nav_pressed(&nav, KeyCode::Char('J'), CTRL),
            "alternate key, Shift cleared"
        );
        assert!(
            nav_pressed(&nav, KeyCode::Char('j'), CTRL | SHIFT),
            "no alternate key"
        );
        assert!(nav_pressed(&nav, KeyCode::Char('J'), CTRL | SHIFT));
        assert!(!nav_pressed(&nav, KeyCode::Char('j'), CTRL), "plain ctrl-j");
        assert!(!nav_pressed(&nav, KeyCode::Char('J'), CTRL | ALT));
        assert!(!nav_pressed(&nav, KeyCode::Char('K'), CTRL));
    }

    #[test]
    fn super_needs_exactly_super_and_not_super_shift() {
        let nav = terminal("super-k", Keyboard::Disambiguated);
        assert!(nav_pressed(&nav, KeyCode::Char('k'), KeyModifiers::SUPER));
        assert!(
            !nav_pressed(&nav, KeyCode::Char('K'), KeyModifiers::SUPER),
            "alternate key"
        );
        assert!(!nav_pressed(
            &nav,
            KeyCode::Char('k'),
            KeyModifiers::SUPER | SHIFT
        ));
        assert!(!nav_pressed(&nav, KeyCode::Char('k'), NONE));
    }

    #[test]
    fn the_control_keys_that_share_bytes_with_other_keys_match_under_kitty_by_their_character() {
        for letter in ['h', 'i', 'j', 'm', '['] {
            let nav = terminal(&format!("ctrl-{letter}"), Keyboard::Disambiguated);
            assert!(nav_pressed(&nav, KeyCode::Char(letter), CTRL), "{letter}");
            assert!(!nav_pressed(&nav, KeyCode::Char(letter), NONE), "{letter}");
        }
        let tab = terminal("ctrl-i", Keyboard::Disambiguated);
        assert!(!nav_pressed(&tab, KeyCode::Tab, NONE));
    }

    #[test]
    fn alt_open_bracket_matches_under_kitty_and_a_plain_bracket_does_not() {
        let nav = terminal("alt-[", Keyboard::Disambiguated);
        assert!(nav_pressed(&nav, KeyCode::Char('['), ALT));
        assert!(!nav_pressed(&nav, KeyCode::Char('['), NONE));
    }

    #[test]
    fn a_plain_character_never_matches_a_control_key() {
        let nav = terminal("ctrl-]", Keyboard::Legacy);
        assert!(!nav_pressed(&nav, KeyCode::Char(']'), NONE));
        assert!(!nav_pressed(&nav, KeyCode::Char('5'), NONE));
    }

    #[test]
    fn a_kitty_key_on_a_legacy_terminal_falls_back_to_the_default_and_says_which() {
        let sendable = terminal("ctrl-shift-t", Keyboard::Legacy);
        assert_eq!(sendable.nav(), KeySpec::CTRL_CLOSE_BRACKET);
        assert!(nav_pressed(&sendable, KeyCode::Char('5'), CTRL));
        let said = "keys.nav ⌃⇧T is not available in this terminal, using ⌃]";
        assert_eq!(sendable.warning().as_deref(), Some(said));
        assert!(sendable.notes().contains(&said.to_string()));
    }

    #[test]
    fn a_kitty_key_is_kept_when_the_terminal_reports_the_protocol() {
        let sendable = terminal("ctrl-shift-t", Keyboard::Disambiguated);
        assert_eq!(sendable.nav(), keys("ctrl-shift-t").nav());
        assert_eq!(sendable.warning(), None);
    }

    #[test]
    fn keys_every_terminal_sends_are_kept_and_never_fall_back() {
        for nav in ["ctrl-]", "ctrl-space", "alt-,", "f5"] {
            let sendable = terminal(nav, Keyboard::Legacy);
            assert_eq!(sendable.nav(), keys(nav).nav(), "{nav}");
            assert_eq!(sendable.warning(), None, "{nav}");
        }
    }

    #[test]
    fn a_nav_key_that_is_not_a_control_byte_keeps_ctrl_close_bracket_as_a_way_out() {
        for nav in ["alt-x", "f5", "ctrl-shift-t", "super-c", "ctrl-h", "alt-<"] {
            let sendable = terminal(nav, Keyboard::Disambiguated);
            assert_eq!(
                sendable.escape_hatch(),
                Some(KeySpec::CTRL_CLOSE_BRACKET),
                "{nav}"
            );
            assert!(
                sendable
                    .notes()
                    .contains(&"⌃] also leaves AGENT".to_string()),
                "{nav}"
            );
            assert!(
                sendable.leaves_agent(&press(KeyCode::Char(']'), CTRL)),
                "{nav}"
            );
        }
    }

    #[test]
    fn a_control_byte_nav_key_has_no_way_out_but_itself() {
        for nav in ["ctrl-]", "ctrl-\\", "ctrl-g", "ctrl-space", "ctrl-^"] {
            let sendable = terminal(nav, Keyboard::Legacy);
            assert_eq!(sendable.escape_hatch(), None, "{nav}");
            assert_eq!(sendable.notes(), Vec::<String>::new(), "{nav}");
        }
        let backslash = terminal("ctrl-\\", Keyboard::Legacy);
        assert!(!backslash.leaves_agent(&press(KeyCode::Char('5'), CTRL)));
        assert!(backslash.leaves_agent(&press(KeyCode::Char('\\'), CTRL)));
    }

    #[test]
    fn a_fallback_to_the_default_needs_no_way_out() {
        let sendable = terminal("ctrl-shift-t", Keyboard::Legacy);
        assert_eq!(sendable.escape_hatch(), None);
    }

    /// The three shapes crossterm reports a shifted key in: legacy,
    /// kitty with the alternate key, kitty without it.
    fn shapes(
        legacy: (KeyCode, KeyModifiers),
        kitty_alternate: (KeyCode, KeyModifiers),
        kitty_plain: (KeyCode, KeyModifiers),
    ) -> [(Keyboard, KeyEvent); 3] {
        let event = |(code, modifiers)| press(code, modifiers);
        [
            (Keyboard::Legacy, event(legacy)),
            (Keyboard::Disambiguated, event(kitty_alternate)),
            (Keyboard::Disambiguated, event(kitty_plain)),
        ]
    }

    #[test]
    fn a_shifted_symbol_matches_in_every_shape_the_terminal_reports_it() {
        let table = [
            (
                "alt-<",
                shapes(
                    (KeyCode::Char('<'), ALT),
                    (KeyCode::Char('<'), ALT),
                    (KeyCode::Char(','), ALT | SHIFT),
                ),
            ),
            (
                "alt-?",
                shapes(
                    (KeyCode::Char('?'), ALT),
                    (KeyCode::Char('?'), ALT),
                    (KeyCode::Char('/'), ALT | SHIFT),
                ),
            ),
            (
                "ctrl-^",
                shapes(
                    (KeyCode::Char('6'), CTRL),
                    (KeyCode::Char('^'), CTRL),
                    (KeyCode::Char('6'), CTRL | SHIFT),
                ),
            ),
            (
                "ctrl-_",
                shapes(
                    (KeyCode::Char('7'), CTRL),
                    (KeyCode::Char('_'), CTRL),
                    (KeyCode::Char('-'), CTRL | SHIFT),
                ),
            ),
        ];
        for (nav, shapes) in table {
            for (keyboard, event) in shapes {
                assert!(
                    terminal(nav, keyboard).pressed(KeyField::Nav, &event),
                    "{nav} {event:?}"
                );
            }
        }
    }

    #[test]
    fn the_unshifted_key_does_not_match_a_shifted_symbol_spec_and_the_reverse() {
        let lt = terminal("alt-<", Keyboard::Disambiguated);
        assert!(!nav_pressed(&lt, KeyCode::Char(','), ALT));
        let comma = terminal("alt-,", Keyboard::Disambiguated);
        assert!(nav_pressed(&comma, KeyCode::Char(','), ALT));
        assert!(!nav_pressed(&comma, KeyCode::Char(','), ALT | SHIFT));
        assert!(!nav_pressed(&comma, KeyCode::Char('<'), ALT));
        let caret = terminal("ctrl-^", Keyboard::Disambiguated);
        assert!(!nav_pressed(&caret, KeyCode::Char('6'), CTRL));
    }

    use crate::fixtures::sendable as configured;

    #[test]
    fn a_chord_needs_exactly_alt_and_its_unshifted_character_in_every_encoding() {
        for keyboard in [Keyboard::Legacy, Keyboard::Disambiguated] {
            let sendable = Sendable::resolve(Keys::default(), keyboard);
            assert_eq!(sendable.warning(), None);
            let hop = |code, modifiers| sendable.hopped(&press(code, modifiers));
            assert_eq!(hop(KeyCode::Char(','), ALT), Some(Hop::Previous));
            assert_eq!(hop(KeyCode::Char('.'), ALT), Some(Hop::Next));
            assert_eq!(hop(KeyCode::Char(','), NONE), None, "a comma is typed text");
            assert_eq!(hop(KeyCode::Char('.'), NONE), None);
            assert_eq!(hop(KeyCode::Char(','), CTRL | ALT), None);
            assert_eq!(hop(KeyCode::Char('<'), ALT), None, "alt-shift-comma");
            assert_eq!(hop(KeyCode::Char(','), ALT | SHIFT), None);
            assert_eq!(hop(KeyCode::Char('>'), ALT | SHIFT), None);
        }
    }

    #[test]
    fn a_chord_that_is_off_matches_nothing_and_leaves_the_other_alone() {
        let sendable = configured("prev_agent = 'none'", Keyboard::Legacy);
        assert_eq!(sendable.hop(Hop::Previous), Binding::Off);
        assert_eq!(sendable.hopped(&press(KeyCode::Char(','), ALT)), None);
        assert_eq!(
            sendable.hopped(&press(KeyCode::Char('.'), ALT)),
            Some(Hop::Next)
        );
        assert_eq!(sendable.warning(), None, "off is a choice, not a fallback");
    }

    #[test]
    fn a_chord_that_needs_kitty_on_a_legacy_terminal_falls_back_to_its_default_and_says_which() {
        let legacy = configured("next_agent = 'super-j'", Keyboard::Legacy);
        assert_eq!(
            legacy.hop(Hop::Next),
            Keys::default().hop(Hop::Next),
            "back to alt-."
        );
        let said = "keys.next_agent ⌘j is not available in this terminal, using ⌥.";
        assert_eq!(legacy.warning().as_deref(), Some(said));
        assert!(legacy.notes().contains(&said.to_string()));
        let kitty = configured("next_agent = 'super-j'", Keyboard::Disambiguated);
        assert_eq!(kitty.warning(), None);
        assert_eq!(
            kitty.hopped(&press(KeyCode::Char('j'), KeyModifiers::SUPER)),
            Some(Hop::Next)
        );
    }

    #[test]
    fn a_default_the_user_didnt_choose_that_another_key_has_is_listed_as_taken() {
        let sendable = configured("nav = 'alt-,'", Keyboard::Legacy);
        assert_eq!(sendable.warning(), None, "nothing is wrong, so no warning");
        assert!(
            sendable
                .notes()
                .contains(&"prev_agent default ⌥, is taken by nav, off".to_string()),
            "{:?}",
            sendable.notes()
        );
        assert_eq!(sendable.hopped(&press(KeyCode::Char(','), ALT)), None);
        assert!(sendable.pressed(KeyField::Nav, &press(KeyCode::Char(','), ALT)));
    }

    #[test]
    fn a_fallback_that_would_share_a_key_leaves_the_chord_off_and_says_so() {
        let sendable = configured(
            "prev_agent = 'super-j'\nnext_agent = 'f6'\nnav = 'alt-,'",
            Keyboard::Legacy,
        );
        assert!(matches!(sendable.hop(Hop::Previous), Binding::Taken { .. }));
        assert_eq!(
            sendable.notes().len(),
            2,
            "the fallback, once, and the way out"
        );
        assert_eq!(
            sendable.warning().as_deref(),
            Some("keys.prev_agent ⌘j is not available in this terminal, leaving it off")
        );
        assert!(sendable.pressed(KeyField::Nav, &press(KeyCode::Char(','), ALT)));
        assert_eq!(sendable.hopped(&press(KeyCode::Char(','), ALT)), None);
    }

    #[test]
    fn shift_on_a_key_with_no_shifted_form_matches_nothing() {
        let space = terminal("ctrl-space", Keyboard::Disambiguated);
        assert!(!nav_pressed(&space, KeyCode::Char(' '), CTRL | SHIFT));
    }
}
