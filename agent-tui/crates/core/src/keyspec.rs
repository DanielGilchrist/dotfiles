//! A key as the config names it: `ctrl-]`, `alt-,`, `f12`, `super-j`.
//!
//! Parsed once into plain data. Core knows no terminal, so the TUI decides
//! what a spec matches.

use std::fmt;
use std::str::FromStr;

use crate::config::KeyField;
use crate::visible;

const ACCEPTED: &str = "ctrl-<letter>, ctrl-space, ctrl-\\ ctrl-] ctrl-^ ctrl-_, alt-<char>, f1 to f12, or with the kitty keyboard protocol ctrl-shift-<letter>, super-<char>, ctrl-h ctrl-i ctrl-j ctrl-m ctrl-[, alt-[ alt-O";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidKeySpec {
    #[error("is empty, use {ACCEPTED}")]
    Empty,
    #[error("{spec:?} is a key on its own, which the agent needs, use {ACCEPTED}")]
    Bare { spec: String },
    #[error("{spec:?} is a key agents use, use {ACCEPTED}")]
    DoubleEscape { spec: String },
    #[error("{spec:?} isn't a key, use {ACCEPTED}")]
    Unknown { spec: String },
}

/// `a`-`z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Letter(char);

impl Letter {
    pub fn char(self) -> char {
        self.0
    }
}

/// `a`-`z` except `h`, `i`, `j` and `m`, whose control bytes a legacy
/// terminal also sends for Backspace, Tab, newline and Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlainLetter(char);

impl PlainLetter {
    pub fn char(self) -> char {
        self.0
    }
}

/// A control chord a legacy terminal sends as a byte of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Letter(PlainLetter),
    Space,
    Backslash,
    CloseBracket,
    Caret,
    Underscore,
}

impl Control {
    pub fn char(self) -> char {
        match self {
            Self::Letter(letter) => letter.char(),
            Self::Space => ' ',
            Self::Backslash => '\\',
            Self::CloseBracket => ']',
            Self::Caret => '^',
            Self::Underscore => '_',
        }
    }

    /// How legacy terminals report it: `Ctrl+\ ] ^ _` arrive as the bytes
    /// 0x1C-0x1F, which decoders read as `Ctrl+4`-`Ctrl+7`.
    pub fn legacy_digit(self) -> Option<char> {
        match self {
            Self::Backslash => Some('4'),
            Self::CloseBracket => Some('5'),
            Self::Caret => Some('6'),
            Self::Underscore => Some('7'),
            Self::Letter(_) | Self::Space => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionKey(u8);

impl FunctionKey {
    pub fn number(self) -> u8 {
        self.0
    }
}

/// A character that leaves a mark: not blank, not a control character such
/// as Esc or Del, not one of `visible`'s invisible or combining ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph(char);

impl Glyph {
    pub const COMMA: Self = Self(',');
    pub const PERIOD: Self = Self('.');

    pub fn char(self) -> char {
        self.0
    }
}

/// A control chord whose byte is also another key's, so only the kitty
/// keyboard protocol tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shadowed {
    H,
    I,
    J,
    M,
    OpenBracket,
}

impl Shadowed {
    pub fn char(self) -> char {
        match self {
            Self::H => 'h',
            Self::I => 'i',
            Self::J => 'j',
            Self::M => 'm',
            Self::OpenBracket => '[',
        }
    }
}

/// The character after Esc that starts an escape sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Starter {
    Csi,
    Ss3,
}

impl Starter {
    pub fn char(self) -> char {
        match self {
            Self::Csi => '[',
            Self::Ss3 => 'O',
        }
    }
}

/// Keys that arrive only through the kitty keyboard protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyKey {
    CtrlShift(Letter),
    Super(Glyph),
    Ctrl(Shadowed),
    Alt(Starter),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySpec {
    Ctrl(Control),
    Alt(Glyph),
    Function(FunctionKey),
    Kitty(KittyKey),
}

impl KeySpec {
    pub const CTRL_CLOSE_BRACKET: Self = Self::Ctrl(Control::CloseBracket);

    pub fn needs_kitty(self) -> bool {
        matches!(self, Self::Kitty(_))
    }

    pub fn written(self) -> Written {
        Written(self)
    }

    /// Every spec a key press that types `c` could be: the ones named with
    /// it, and the control keys a legacy terminal reports as it (`ctrl-]`
    /// arrives as `Ctrl+5`). Which one a terminal would match is the
    /// caller's to say.
    pub fn on_character(c: char) -> Vec<Self> {
        let text = c.to_string();
        let control = if c == ' ' { "space" } else { text.as_str() };
        let legacy = match c {
            '4' => Some("\\"),
            '5' => Some("]"),
            '6' => Some("^"),
            '7' => Some("_"),
            _ => None,
        };
        let lower = c.to_ascii_lowercase();
        let mut specs: Vec<Self> = Vec::new();
        specs.extend(ctrl(control, control).ok());
        specs.extend(legacy.and_then(|legacy| ctrl(legacy, legacy).ok()));
        specs.extend(alt(&text, &text).ok());
        specs.extend(glyph(&text).map(|glyph| Self::Kitty(KittyKey::Super(glyph))));
        if lower.is_ascii_lowercase() {
            specs.push(Self::Kitty(KittyKey::CtrlShift(Letter(lower))));
        }
        specs
    }

    pub fn function(number: u8) -> Option<Self> {
        function(&number.to_string()).map(Self::Function)
    }
}

/// A spec as the config writes it, which parses back to the same spec.
pub struct Written(KeySpec);

impl fmt::Display for Written {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            KeySpec::Ctrl(Control::Space) => f.write_str("ctrl-space"),
            KeySpec::Ctrl(control) => write!(f, "ctrl-{}", control.char()),
            KeySpec::Alt(glyph) => write!(f, "alt-{}", glyph.char()),
            KeySpec::Function(function) => write!(f, "f{}", function.number()),
            KeySpec::Kitty(KittyKey::CtrlShift(letter)) => {
                write!(f, "ctrl-shift-{}", letter.char())
            }
            KeySpec::Kitty(KittyKey::Super(glyph)) => write!(f, "super-{}", glyph.char()),
            KeySpec::Kitty(KittyKey::Ctrl(shadowed)) => write!(f, "ctrl-{}", shadowed.char()),
            KeySpec::Kitty(KittyKey::Alt(starter)) => write!(f, "alt-{}", starter.char()),
        }
    }
}

/// A key the user may turn off: `none` or `off` in the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    Off,
    On(KeySpec),
    /// Left off because `by` has this field's default, which nobody asked
    /// for. Two keys the user set to one key is an error instead.
    Taken {
        default: KeySpec,
        by: KeyField,
    },
}

impl Binding {
    pub fn spec(self) -> Option<KeySpec> {
        match self {
            Self::Off | Self::Taken { .. } => None,
            Self::On(spec) => Some(spec),
        }
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off | Self::Taken { .. } => f.write_str("off"),
            Self::On(spec) => spec.fmt(f),
        }
    }
}

impl FromStr for Binding {
    type Err = InvalidKeySpec;

    fn from_str(raw: &str) -> Result<Self, InvalidKeySpec> {
        if is_word(raw.trim(), &["none", "off"]) {
            return Ok(Self::Off);
        }
        raw.parse().map(Self::On)
    }
}

/// The glyphs a footer or help shows. They are for reading, so `⌃G` isn't
/// config text: a spec is written `ctrl-g`.
impl fmt::Display for KeySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ctrl(Control::Space) => f.write_str("⌃Space"),
            Self::Ctrl(control) => write!(f, "⌃{}", control.char().to_ascii_uppercase()),
            Self::Alt(glyph) => write!(f, "⌥{}", glyph.char()),
            Self::Function(function) => write!(f, "F{}", function.number()),
            Self::Kitty(KittyKey::CtrlShift(letter)) => {
                write!(f, "⌃⇧{}", letter.char().to_ascii_uppercase())
            }
            Self::Kitty(KittyKey::Super(glyph)) => write!(f, "⌘{}", glyph.char()),
            Self::Kitty(KittyKey::Ctrl(shadowed)) => {
                write!(f, "⌃{}", shadowed.char().to_ascii_uppercase())
            }
            Self::Kitty(KittyKey::Alt(starter)) => write!(f, "⌥{}", starter.char()),
        }
    }
}

fn unknown(spec: &str) -> InvalidKeySpec {
    InvalidKeySpec::Unknown {
        spec: spec.to_string(),
    }
}

fn single(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

fn glyph(text: &str) -> Option<Glyph> {
    single(text).filter(|c| visible::is_visible(*c)).map(Glyph)
}

/// What follows `word` at the start of `text`, whatever the case of `word`.
fn after<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let (head, rest) = (text.get(..word.len())?, text.get(word.len()..)?);
    head.eq_ignore_ascii_case(word).then_some(rest)
}

fn is_word(text: &str, words: &[&str]) -> bool {
    words.iter().any(|word| text.eq_ignore_ascii_case(word))
}

fn ctrl(rest: &str, spec: &str) -> Result<KeySpec, InvalidKeySpec> {
    if rest.eq_ignore_ascii_case("space") {
        return Ok(KeySpec::Ctrl(Control::Space));
    }
    let shadowed = |key| Ok(KeySpec::Kitty(KittyKey::Ctrl(key)));
    match single(rest).map(|c| c.to_ascii_lowercase()) {
        Some('\\') => Ok(KeySpec::Ctrl(Control::Backslash)),
        Some(']') => Ok(KeySpec::Ctrl(Control::CloseBracket)),
        Some('^') => Ok(KeySpec::Ctrl(Control::Caret)),
        Some('_') => Ok(KeySpec::Ctrl(Control::Underscore)),
        Some('[') => shadowed(Shadowed::OpenBracket),
        Some('h') => shadowed(Shadowed::H),
        Some('i') => shadowed(Shadowed::I),
        Some('j') => shadowed(Shadowed::J),
        Some('m') => shadowed(Shadowed::M),
        Some(c @ 'a'..='z') => Ok(KeySpec::Ctrl(Control::Letter(PlainLetter(c)))),
        _ => Err(unknown(spec)),
    }
}

fn alt(rest: &str, spec: &str) -> Result<KeySpec, InvalidKeySpec> {
    match glyph(rest).ok_or_else(|| unknown(spec))? {
        Glyph('[') => Ok(KeySpec::Kitty(KittyKey::Alt(Starter::Csi))),
        Glyph('O') => Ok(KeySpec::Kitty(KittyKey::Alt(Starter::Ss3))),
        other => Ok(KeySpec::Alt(other)),
    }
}

fn function(digits: &str) -> Option<FunctionKey> {
    let canonical = digits.bytes().all(|b| b.is_ascii_digit()) && !digits.starts_with('0');
    canonical
        .then(|| digits.parse::<u8>().ok())
        .flatten()
        .filter(|number| (1..=12).contains(number))
        .map(FunctionKey)
}

impl FromStr for KeySpec {
    type Err = InvalidKeySpec;

    fn from_str(raw: &str) -> Result<Self, InvalidKeySpec> {
        let spec = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if spec.is_empty() {
            return Err(InvalidKeySpec::Empty);
        }
        if let Some(rest) = after(&spec, "ctrl-shift-") {
            return match single(rest).map(|c| c.to_ascii_lowercase()) {
                Some(c @ 'a'..='z') => Ok(Self::Kitty(KittyKey::CtrlShift(Letter(c)))),
                _ => Err(unknown(&spec)),
            };
        }
        if let Some(rest) = after(&spec, "super-") {
            return glyph(rest)
                .map(|key| Self::Kitty(KittyKey::Super(key)))
                .ok_or_else(|| unknown(&spec));
        }
        if let Some(rest) = after(&spec, "ctrl-") {
            return ctrl(rest, &spec);
        }
        if let Some(rest) = after(&spec, "alt-") {
            return alt(rest, &spec);
        }
        if let Some(digits) = after(&spec, "f")
            .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return function(digits)
                .map(Self::Function)
                .ok_or_else(|| unknown(&spec));
        }
        let escapes = spec.split(' ').collect::<Vec<_>>();
        if escapes.len() == 2 && escapes.iter().all(|word| is_word(word, &["esc", "escape"])) {
            return Err(InvalidKeySpec::DoubleEscape { spec });
        }
        let named = is_word(
            &spec,
            &[
                "esc",
                "escape",
                "tab",
                "enter",
                "return",
                "backspace",
                "space",
            ],
        );
        if named || single(&spec).is_some() {
            return Err(InvalidKeySpec::Bare { spec });
        }
        Err(unknown(&spec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> KeySpec {
        text.parse().expect("parses")
    }

    fn error(text: &str) -> String {
        text.parse::<KeySpec>()
            .expect_err("is rejected")
            .to_string()
    }

    #[test]
    fn every_accepted_form_parses_and_shows_as_the_glyphs_a_footer_uses() {
        let table = [
            ("ctrl-]", "⌃]"),
            ("ctrl-\\", "⌃\\"),
            ("ctrl-^", "⌃^"),
            ("ctrl-_", "⌃_"),
            ("ctrl-g", "⌃G"),
            ("ctrl-space", "⌃Space"),
            ("alt-,", "⌥,"),
            ("alt-x", "⌥x"),
            ("f1", "F1"),
            ("f12", "F12"),
            ("ctrl-shift-j", "⌃⇧J"),
            ("super-k", "⌘k"),
            ("ctrl-h", "⌃H"),
            ("ctrl-i", "⌃I"),
            ("ctrl-j", "⌃J"),
            ("ctrl-m", "⌃M"),
            ("ctrl-[", "⌃["),
            ("alt-[", "⌥["),
            ("alt-O", "⌥O"),
        ];
        for (text, expected) in table {
            assert_eq!(parsed(text).to_string(), expected, "{text}");
        }
    }

    #[test]
    fn a_spec_is_written_in_the_form_that_parses_back_to_it() {
        for text in [
            "ctrl-]",
            "ctrl-\\",
            "ctrl-^",
            "ctrl-_",
            "ctrl-g",
            "ctrl-space",
            "alt-,",
            "alt-X",
            "f1",
            "f12",
            "ctrl-shift-j",
            "super-k",
            "ctrl-h",
            "ctrl-i",
            "ctrl-j",
            "ctrl-m",
            "ctrl-[",
            "alt-[",
            "alt-O",
        ] {
            assert_eq!(parsed(text).written().to_string(), text);
        }
    }

    #[test]
    fn the_specs_on_a_character_include_the_legacy_control_keys_that_arrive_as_it() {
        let on = |c| KeySpec::on_character(c);
        assert!(on('5').contains(&KeySpec::CTRL_CLOSE_BRACKET));
        assert!(on('5').contains(&parsed("alt-5")));
        assert!(on(' ').contains(&parsed("ctrl-space")));
        assert!(on('J').contains(&parsed("ctrl-shift-j")));
        assert!(on(',').contains(&parsed("alt-,")));
        assert_eq!(KeySpec::function(12), Some(parsed("f12")));
        assert_eq!(KeySpec::function(13), None);
    }

    #[test]
    fn modifier_and_key_names_ignore_case_and_whitespace_but_a_character_keeps_its_case() {
        assert_eq!(parsed("  CTRL-G ").to_string(), "⌃G");
        assert_eq!(parsed("F12"), parsed("f12"));
        assert_eq!(parsed("Ctrl-Space"), parsed("ctrl-space"));
        assert_eq!(parsed("Ctrl-Shift-J"), parsed("ctrl-shift-j"));
        assert_eq!(parsed("Alt-X").to_string(), "⌥X");
        assert_ne!(parsed("alt-X"), parsed("alt-x"));
        assert_eq!(parsed("SUPER-K").to_string(), "⌘K");
        assert_eq!(parsed("ctrl-]"), KeySpec::CTRL_CLOSE_BRACKET);
    }

    #[test]
    fn only_keys_a_legacy_terminal_cannot_tell_apart_need_kitty() {
        let kitty = [
            "ctrl-shift-j",
            "super-k",
            "ctrl-h",
            "ctrl-i",
            "ctrl-j",
            "ctrl-m",
            "ctrl-[",
            "alt-[",
            "alt-O",
        ];
        for text in kitty {
            assert!(parsed(text).needs_kitty(), "{text}");
        }
        for text in ["ctrl-]", "ctrl-space", "ctrl-g", "alt-,", "alt-o", "f5"] {
            assert!(!parsed(text).needs_kitty(), "{text}");
        }
    }

    #[test]
    fn a_key_with_no_modifier_is_refused() {
        for text in [
            "esc",
            "tab",
            "enter",
            "Return",
            "backspace",
            "x",
            "]",
            "space",
            "5",
            "f",
        ] {
            assert_eq!(
                error(text),
                format!(
                    "{:?} is a key on its own, which the agent needs, use {ACCEPTED}",
                    text.split_whitespace().collect::<String>()
                ),
            );
        }
    }

    #[test]
    fn double_escape_is_refused_whatever_its_spacing_or_case() {
        for text in ["esc esc", "Esc  Esc", "escape esc"] {
            assert!(error(text).contains("is a key agents use"), "{text}");
        }
    }

    #[test]
    fn unknown_forms_are_refused_listing_what_is_accepted() {
        for text in [
            "ctrl-1",
            "ctrl-ab",
            "ctrl-",
            "alt-",
            "alt-ab",
            "alt-space",
            "f0",
            "f13",
            "f01",
            "f012",
            "f+1",
            "f1x",
            "meta-x",
            "ctrl-shift-",
            "ctrl-shift-1",
            "ctrl-shift-ab",
            "super-ab",
            "esc x",
            "banana",
        ] {
            let message = error(text);
            assert!(message.contains("isn't a key"), "{text}: {message}");
            assert!(message.ends_with(ACCEPTED), "{text}: {message}");
        }
    }

    #[test]
    fn a_character_that_leaves_no_mark_is_not_a_key() {
        for text in [
            "alt-\u{200d}",
            "alt-\u{0301}",
            "alt-\u{feff}",
            "alt-\u{1b}",
            "alt-\u{7f}",
            "super-\u{200b}",
            "alt-\u{a0}",
        ] {
            assert!(error(text).contains("isn't a key"), "{text:?}");
        }
    }

    #[test]
    fn an_empty_spec_is_refused() {
        assert_eq!(error("  "), format!("is empty, use {ACCEPTED}"));
    }

    #[test]
    fn only_the_specials_have_legacy_digits() {
        let control = |text: &str| match parsed(text) {
            KeySpec::Ctrl(control) => control,
            other => panic!("{other:?} isn't a plain control key"),
        };
        let digits = [
            ("ctrl-\\", '4'),
            ("ctrl-]", '5'),
            ("ctrl-^", '6'),
            ("ctrl-_", '7'),
        ];
        for (text, digit) in digits {
            assert_eq!(control(text).legacy_digit(), Some(digit), "{text}");
        }
        assert_eq!(control("ctrl-g").legacy_digit(), None);
        assert_eq!(control("ctrl-space").legacy_digit(), None);
        assert_eq!(control("ctrl-space").char(), ' ');
    }

    #[test]
    fn none_and_off_turn_a_binding_off_whatever_their_case() {
        for text in ["none", "off", " None ", "OFF"] {
            assert_eq!(text.parse::<Binding>(), Ok(Binding::Off), "{text}");
        }
        assert_eq!("alt-,".parse::<Binding>(), Ok(Binding::On(parsed("alt-,"))));
        assert_eq!(Binding::On(parsed("alt-,")).spec(), Some(parsed("alt-,")));
        assert_eq!(Binding::Off.spec(), None);
        assert!("banana".parse::<Binding>().is_err());
        assert!("".parse::<Binding>().is_err());
    }

    #[test]
    fn none_is_not_a_key_spec_so_nav_cannot_be_turned_off() {
        let err = "[keys]\nnav = 'none'"
            .parse::<crate::config::Config>()
            .expect_err("fails")
            .to_string();
        assert!(err.starts_with("keys.nav: \"none\" isn't a key"), "{err}");
    }

    #[test]
    fn display_is_for_reading_and_is_not_config_text() {
        for text in ["ctrl-]", "alt-,"] {
            assert!(
                parsed(text).to_string().parse::<KeySpec>().is_err(),
                "{text}"
            );
        }
    }
}
