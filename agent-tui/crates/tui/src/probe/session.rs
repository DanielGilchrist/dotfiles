//! The conversation `agent keys` has: which field is being chosen, what
//! each key press is worth and what to print for it. It reads no terminal,
//! so it is tested with keys built from bytes.

use std::path::PathBuf;
use std::str::FromStr;

use agent_core::config::{Config, KeyField};
use agent_core::keyspec::KeySpec;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::classify::{Verdict, classify};
use super::decode::RawKey;
use super::known::{ATTRIBUTION, Collision};
use crate::keyspec::Keyboard;

/// The digit that picks each field.
fn numbered() -> Vec<(u8, KeyField)> {
    (b'1'..).zip(KeyField::all()).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quitter {
    Escape,
    CtrlC,
}

impl Quitter {
    /// Decoded, because a kitty terminal sends Esc and Ctrl-C as escape
    /// sequences and raw mode keeps Ctrl-C from being a signal.
    fn of(key: &RawKey) -> Option<Self> {
        let KeyEvent {
            code, modifiers, ..
        } = key.event?;
        match (code, modifiers) {
            (KeyCode::Esc, KeyModifiers::NONE) => Some(Self::Escape),
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(Self::CtrlC),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Escape => "Esc",
            Self::CtrlC => "Ctrl-C",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Choosing,
    Pressing(KeyField),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quit {
    Idle,
    Armed(Quitter),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Show(Vec<String>),
    Done,
}

pub struct Session {
    keyboard: Keyboard,
    config_path: PathBuf,
    stage: Stage,
    quit: Quit,
}

impl Session {
    pub fn new(keyboard: Keyboard, config_path: PathBuf) -> Self {
        Self {
            keyboard,
            config_path,
            stage: Stage::Choosing,
            quit: Quit::Idle,
        }
    }

    pub fn opening(&self) -> Vec<String> {
        vec![self.keyboard.describe().to_string(), menu()]
    }

    pub fn press(&mut self, key: &RawKey) -> Step {
        let quitter = Quitter::of(key);
        if let (Some(this), Quit::Armed(armed)) = (quitter, self.quit)
            && this == armed
        {
            return Step::Done;
        }
        self.quit = quitter.map_or(Quit::Idle, Quit::Armed);
        let mut lines = match self.stage {
            Stage::Choosing => self.choose(key),
            Stage::Pressing(field) => {
                self.stage = Stage::Choosing;
                let mut lines = self.report(key, field);
                lines.push(menu());
                lines
            }
        };
        if let Some(quitter) = quitter {
            lines.push(format!("{} again to quit", quitter.name()));
        }
        Step::Show(lines)
    }

    fn choose(&mut self, key: &RawKey) -> Vec<String> {
        let chosen = numbered()
            .into_iter()
            .find(|(digit, _)| key.bytes == [*digit])
            .map(|(_, field)| field);
        match chosen {
            Some(field) => {
                self.stage = Stage::Pressing(field);
                vec![format!("press the key for {field}")]
            }
            None => vec![menu()],
        }
    }

    fn report(&self, key: &RawKey, field: KeyField) -> Vec<String> {
        let mut lines = vec![format!("bytes {}", hex(&key.bytes))];
        lines.push(match key.event {
            Some(event) => format!("event {}", shown(&event)),
            None => "event not one the TUI reads".to_string(),
        });
        lines.extend(self.verdict_lines(&classify(key, self.keyboard), field));
        lines
    }

    fn verdict_lines(&self, verdict: &Verdict, field: KeyField) -> Vec<String> {
        match verdict {
            Verdict::Usable { spec, rare } => {
                let mut lines = vec!["usable".to_string()];
                lines.extend(kitty_note(*spec));
                if let Some(rare) = rare {
                    lines.extend(claim_lines("rarely hit, known uses", rare));
                }
                lines.extend(self.assignment(field, *spec));
                lines
            }
            Verdict::Collides { spec, with } => {
                let mut lines = claim_lines("collides, known uses", with);
                lines.extend(kitty_note(*spec));
                lines.push("but if you still want it:".to_string());
                lines.extend(self.assignment(field, *spec));
                lines
            }
            Verdict::Ambiguous { same_as } => vec![format!(
                "the terminal sends the same bytes as {}, so the two can't be told apart",
                same_as.label()
            )],
            Verdict::Composed(c) => vec![format!(
                "Option typed {c} instead of acting as Alt. Turn on Option as Alt in the \
                 terminal's settings, which the docs list for each terminal"
            )],
            Verdict::Unsendable => {
                vec!["can't be used. Try a Ctrl chord, an Alt key or F1 to F12".to_string()]
            }
        }
    }

    fn assignment(&self, field: KeyField, spec: KeySpec) -> Vec<String> {
        let line = field.assignment(spec);
        let table = format!("[keys]\n{line}\n");
        match Config::from_str(&table) {
            Ok(_) => vec![
                format!("add to {}:", self.config_path.display()),
                "[keys]".to_string(),
                line,
            ],
            Err(refused) => vec![format!("the config would refuse it: {refused}")],
        }
    }
}

fn kitty_note(spec: KeySpec) -> Option<String> {
    spec.needs_kitty().then(|| {
        "only the kitty keyboard protocol sends it. A terminal without it falls back to the default"
            .to_string()
    })
}

pub(super) fn claim_lines(heading: &str, collision: &Collision) -> Vec<String> {
    let mut lines = vec![format!("{heading} ({ATTRIBUTION}):")];
    lines.extend(
        collision
            .claims()
            .map(|claim| format!("  {}: {}", claim.program.label(), claim.uses_it_for)),
    );
    lines
}

fn menu() -> String {
    let fields: Vec<String> = numbered()
        .into_iter()
        .map(|(digit, field)| format!("{}) {field}", char::from(digit)))
        .collect();
    format!(
        "choose {}. Esc Esc or Ctrl-C twice quits",
        fields.join("  ")
    )
}

fn hex(bytes: &[u8]) -> String {
    let digits: Vec<String> = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    digits.join(" ")
}

fn shown(event: &KeyEvent) -> String {
    let names: Vec<&str> = event.modifiers.iter_names().map(|(name, _)| name).collect();
    if names.is_empty() {
        format!("{:?}", event.code)
    } else {
        format!("{:?} with {}", event.code, names.join("+"))
    }
}

#[cfg(test)]
mod tests {
    use agent_core::config::Hop;

    use super::super::decode;
    use super::*;

    fn session(keyboard: Keyboard) -> Session {
        Session::new(keyboard, PathBuf::from("/cfg/agent/config.toml"))
    }

    fn keys(bytes: &[u8]) -> RawKey {
        decode::all(bytes).remove(0)
    }

    fn press(session: &mut Session, bytes: &[u8]) -> Vec<String> {
        match session.press(&keys(bytes)) {
            Step::Show(lines) => lines,
            Step::Done => panic!("{bytes:?} ended the session"),
        }
    }

    fn tested(keyboard: Keyboard, digit: &[u8], bytes: &[u8]) -> Vec<String> {
        let mut probe = session(keyboard);
        press(&mut probe, digit);
        press(&mut probe, bytes)
    }

    fn has(lines: &[String], text: &str) -> bool {
        lines.iter().any(|line| line == text)
    }

    fn starts(lines: &[String], text: &str) -> bool {
        lines.iter().any(|line| line.starts_with(text))
    }

    #[test]
    fn it_says_which_keyboard_it_found_and_how_to_choose_and_quit() {
        let legacy = session(Keyboard::Legacy).opening();
        assert_eq!(legacy[0], "legacy key reporting");
        assert_eq!(
            legacy[1],
            "choose 1) nav  2) prev_agent  3) next_agent. Esc Esc or Ctrl-C twice quits"
        );
        let kitty = session(Keyboard::Disambiguated).opening();
        assert_eq!(
            kitty[0],
            "this terminal reports the kitty keyboard protocol"
        );
    }

    #[test]
    fn a_usable_key_prints_the_bytes_the_event_and_the_line_to_paste_for_the_chosen_field() {
        let mut probe = session(Keyboard::Legacy);
        assert_eq!(press(&mut probe, b"2"), ["press the key for prev_agent"]);
        let lines = press(&mut probe, b"\x1b,");
        assert_eq!(
            lines[..6],
            [
                "bytes 1b 2c",
                "event Char(',') with ALT",
                "usable",
                "add to /cfg/agent/config.toml:",
                "[keys]",
                "prev_agent = \"alt-,\"",
            ]
        );
        assert!(lines[6].starts_with("choose 1)"));
    }

    #[test]
    fn the_nav_field_writes_nav_with_the_spec_quoted_so_the_config_reads_it_back() {
        let lines = tested(Keyboard::Legacy, b"1", &[0x1c]);
        assert!(has(&lines, "nav = 'ctrl-\\'"), "{lines:?}");
    }

    #[test]
    fn a_key_only_rarely_used_is_usable_with_its_line_and_the_rare_uses() {
        let lines = tested(Keyboard::Legacy, b"1", &[0x1d]);
        assert!(has(&lines, "usable"), "{lines:?}");
        assert!(starts(&lines, "rarely hit, known uses"), "{lines:?}");
        assert!(has(&lines, "nav = \"ctrl-]\""), "{lines:?}");
    }

    #[test]
    fn a_colliding_key_names_its_uses_and_still_gives_the_line_to_paste() {
        let lines = tested(Keyboard::Legacy, b"1", &[0x12]);
        assert!(starts(
            &lines,
            "collides, known uses (from memory, unchecked):"
        ));
        assert!(has(&lines, "  readline and shells: reverse history search"));
        assert!(has(&lines, "but if you still want it:"));
        assert!(has(&lines, "nav = \"ctrl-r\""), "{lines:?}");
    }

    #[test]
    fn a_colliding_kitty_only_key_says_both() {
        let lines = tested(Keyboard::Disambiguated, b"1", b"\x1b[104;5u");
        assert!(starts(&lines, "collides"), "{lines:?}");
        assert!(starts(&lines, "only the kitty keyboard protocol sends it"));
        assert!(has(&lines, "nav = \"ctrl-h\""), "{lines:?}");
    }

    #[test]
    fn a_kitty_only_key_is_usable_with_the_note() {
        let lines = tested(Keyboard::Disambiguated, b"1", b"\x1b[106:74;6u");
        assert!(has(&lines, "usable"));
        assert!(starts(&lines, "only the kitty keyboard protocol sends it"));
        assert!(has(&lines, "nav = \"ctrl-shift-j\""));
    }

    #[test]
    fn a_line_the_config_would_refuse_is_replaced_by_the_configs_error() {
        let probe = session(Keyboard::Legacy);
        let lines = probe.assignment(KeyField::Hop(Hop::Next), "ctrl-]".parse().expect("parses"));
        assert_eq!(
            lines,
            ["the config would refuse it: keys.next_agent is the same key as keys.nav"]
        );
    }

    #[test]
    fn ambiguous_composed_and_unsendable_keys_each_say_why() {
        let lines = tested(Keyboard::Legacy, b"1", &[0x09]);
        assert!(has(
            &lines,
            "the terminal sends the same bytes as Tab, so the two can't be told apart"
        ));
        let lines = tested(Keyboard::Legacy, b"1", "≤".as_bytes());
        assert!(starts(&lines, "Option typed ≤ instead of acting as Alt"));
        let lines = tested(Keyboard::Legacy, b"1", b"x");
        assert!(has(
            &lines,
            "can't be used. Try a Ctrl chord, an Alt key or F1 to F12"
        ));
    }

    #[test]
    fn an_unrecognised_menu_key_shows_the_menu_again() {
        let mut probe = session(Keyboard::Legacy);
        let lines = press(&mut probe, b"9");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("choose 1)"));
    }

    #[test]
    fn escape_twice_or_ctrl_c_twice_quits_in_either_keyboard_and_anything_between_resets_it() {
        let quits: [(Keyboard, &[u8]); 4] = [
            (Keyboard::Legacy, &[0x1b]),
            (Keyboard::Legacy, &[0x03]),
            (Keyboard::Disambiguated, b"\x1b[27u"),
            (Keyboard::Disambiguated, b"\x1b[99;5u"),
        ];
        for (keyboard, quit) in quits {
            let mut probe = session(keyboard);
            let first = press(&mut probe, quit);
            assert!(first.last().expect("a line").ends_with("again to quit"));
            assert_eq!(probe.press(&keys(quit)), Step::Done, "{quit:?}");

            let mut probe = session(keyboard);
            press(&mut probe, quit);
            press(&mut probe, b"9");
            let again = press(&mut probe, quit);
            assert!(again.last().expect("a line").ends_with("again to quit"));
        }
    }

    #[test]
    fn escape_then_ctrl_c_does_not_quit() {
        let mut probe = session(Keyboard::Legacy);
        press(&mut probe, &[0x1b]);
        assert!(matches!(probe.press(&keys(&[0x03])), Step::Show(_)));
    }
}
