use std::collections::BTreeSet;

use vte::{Params, Parser, Perform};

use crate::egress::Mode;

/// The private modes that switch to the alternate screen, as `vt100` sees
/// them. `?1047` is not one of them, since `vt100` ignores it.
pub const ALTERNATE_SCREEN_MODES: [Mode; 2] = [Mode::AlternateScreenLegacy, Mode::AlternateScreen];

const KITTY_STACK_LIMIT: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorSave {
    /// `CSI s`.
    Csi,
    /// `ESC 7`.
    Dec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    Enter,
    Leave,
}

/// What the byte just stepped did that the screen has to act on at that very
/// byte, not at the end of the chunk it arrived in. The ledger doesn't know
/// which screen `vt100` is showing, so it only says that something that may
/// change it, or that depends on it, happened. The screen looks at `vt100`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The final byte of a sequence that sets or resets an alternate screen mode.
    Switched(Switch),
    /// `DECSTBM` as written, with 0 for a missing number.
    Region {
        top: u16,
        bottom: u16,
    },
    Saved(CursorSave),
    /// `CSI u`, which `vt100` ignores.
    RestoredCsi,
    /// A full or soft reset.
    Reset,
}

/// What a byte stream has set that `vt100` doesn't serialise: private modes
/// that differ from a terminal's defaults,
/// the kitty keyboard stack, `modifyOtherKeys` and the title. It reads the
/// stream the egress parser forwarded, so every sequence in it is whole and
/// one the host lists.
/// The host replays it to a new client, and a client undoes it on exit.
pub struct Ledger {
    parser: Parser,
    state: State,
}

#[derive(Debug)]
struct State {
    modes: BTreeSet<Mode>,
    kitty: Vec<u32>,
    modify_other_keys: Option<u16>,
    application_keypad: bool,
    margins: bool,
    title: Option<Vec<u8>>,
    event: Option<Event>,
}

impl State {
    fn new() -> Self {
        Self {
            modes: BTreeSet::new(),
            kitty: Vec::new(),
            modify_other_keys: None,
            application_keypad: false,
            margins: false,
            title: None,
            event: None,
        }
    }

    fn set_mode(&mut self, mode: Mode, on: bool) {
        if on == mode.starts_on() {
            self.modes.remove(&mode);
        } else {
            self.modes.insert(mode);
        }
        if ALTERNATE_SCREEN_MODES.contains(&mode) {
            self.event = Some(Event::Switched(if on {
                Switch::Enter
            } else {
                Switch::Leave
            }));
        }
    }

    fn push_kitty(&mut self, flags: u32) {
        if self.kitty.len() == KITTY_STACK_LIMIT {
            self.kitty.remove(0);
        }
        self.kitty.push(flags);
    }

    fn pop_kitty(&mut self, count: usize) {
        let kept = self.kitty.len().saturating_sub(count);
        self.kitty.truncate(kept);
    }

    fn set_kitty(&mut self, flags: u32, how: u16) {
        let Some(top) = self.kitty.last_mut() else {
            self.push_kitty(flags);
            return;
        };
        *top = match how {
            2 => *top | flags,
            3 => *top & !flags,
            _ => flags,
        };
    }
}

/// `CSI s` and `CSI u` take no parameter. With two they are other
/// sequences, and with one they are not the cursor's.
fn bare(params: &Params) -> bool {
    params.len() <= 1 && first(params).unwrap_or(0) == 0
}

fn first(params: &Params) -> Option<u16> {
    params
        .iter()
        .next()
        .and_then(|param| param.first().copied())
}

fn second(params: &Params) -> Option<u16> {
    params
        .iter()
        .nth(1)
        .and_then(|param| param.first().copied())
}

impl Perform for State {
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let number = |default: u16| first(params).filter(|n| *n != 0).unwrap_or(default);
        match (intermediates, action) {
            ([b'?'], 'h' | 'l') => {
                for param in params {
                    if let Some(mode) = param.first().and_then(|number| Mode::from_number(*number))
                    {
                        self.set_mode(mode, action == 'h');
                    }
                }
            }
            ([], 's') if bare(params) => self.event = Some(Event::Saved(CursorSave::Csi)),
            ([], 'u') if bare(params) => self.event = Some(Event::RestoredCsi),
            ([b'>'], 'u') => self.push_kitty(u32::from(first(params).unwrap_or(0))),
            ([b'<'], 'u') => self.pop_kitty(usize::from(number(1))),
            ([b'='], 'u') => self.set_kitty(
                u32::from(first(params).unwrap_or(0)),
                second(params).unwrap_or(1),
            ),
            ([b'>'], 'm') if first(params) == Some(4) => {
                self.modify_other_keys = second(params).filter(|level| *level != 0);
            }
            ([b'>'], 'n') if first(params) == Some(4) => self.modify_other_keys = None,
            ([], 'r') => {
                let (top, bottom) = (first(params).unwrap_or(0), second(params).unwrap_or(0));
                self.margins = top > 1 || bottom != 0;
                self.event = Some(Event::Region { top, bottom });
            }
            ([b'!'], 'p') => {
                self.modes.clear();
                self.margins = false;
                self.event = Some(Event::Reset);
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.event = Some(Event::Saved(CursorSave::Dec)),
            b'=' => self.application_keypad = true,
            b'>' => self.application_keypad = false,
            b'c' => {
                *self = Self::new();
                self.event = Some(Event::Reset);
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if let [b"0" | b"2", title @ ..] = params {
            self.title = Some(title.join(&b';'));
        }
    }
}

impl Ledger {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            state: State::new(),
        }
    }

    /// Takes one byte and says what it did that can't wait for the end of
    /// the chunk it came in.
    pub fn step(&mut self, byte: u8) -> Option<Event> {
        self.parser.advance(&mut self.state, &[byte]);
        self.state.event.take()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.state, bytes);
        self.state.event = None;
    }

    /// The private modes that are not as a terminal starts: the ones set that
    /// start off and the ones reset that start on ([`Mode::starts_on`]).
    pub fn modes(&self) -> &BTreeSet<Mode> {
        &self.state.modes
    }

    pub fn synchronized(&self) -> bool {
        self.state.modes.contains(&Mode::SynchronizedOutput)
    }

    /// Whether a scroll region other than the whole screen was set.
    pub fn margins_set(&self) -> bool {
        self.state.margins
    }

    /// The kitty keyboard flags pushed and not yet popped, oldest first.
    pub fn kitty_stack(&self) -> &[u32] {
        &self.state.kitty
    }

    pub fn modify_other_keys(&self) -> Option<u16> {
        self.state.modify_other_keys
    }

    pub fn application_keypad(&self) -> bool {
        self.state.application_keypad
    }

    pub fn title(&self) -> Option<&[u8]> {
        self.state.title.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(bytes: &[u8]) -> Ledger {
        let mut ledger = Ledger::new();
        ledger.feed(bytes);
        ledger
    }

    #[test]
    fn modes_are_the_ones_set_and_not_reset() {
        let ledger = fed(b"\x1b[?1004h\x1b[?1000;1006h\x1b[?1000l\x1b[?2004h");
        assert_eq!(
            ledger.modes().iter().copied().collect::<Vec<_>>(),
            [Mode::FocusReports, Mode::MouseSgr, Mode::BracketedPaste]
        );
    }

    fn events(bytes: &[u8]) -> Vec<Event> {
        let mut ledger = Ledger::new();
        bytes.iter().filter_map(|byte| ledger.step(*byte)).collect()
    }

    #[test]
    fn setting_or_resetting_an_alternate_screen_mode_is_reported_at_its_final_byte() {
        assert_eq!(
            events(b"a\x1b[?1049h\x1b[?47l\x1b[?1047h"),
            [
                Event::Switched(Switch::Enter),
                Event::Switched(Switch::Leave)
            ]
        );
        let mut ledger = Ledger::new();
        let at = b"ab\x1b[?1049hc"
            .iter()
            .position(|byte| ledger.step(*byte).is_some());
        assert_eq!(at, Some(9));
    }

    #[test]
    fn a_scroll_region_is_reported_as_written_and_noted_when_it_isnt_the_whole_screen() {
        assert_eq!(
            events(b"\x1b[3;20r\x1b[r\x1b[5r"),
            [
                Event::Region { top: 3, bottom: 20 },
                Event::Region { top: 0, bottom: 0 },
                Event::Region { top: 5, bottom: 0 }
            ]
        );
        assert!(fed(b"\x1b[3;20r").margins_set());
        assert!(!fed(b"\x1b[3;20r\x1b[r").margins_set());
    }

    #[test]
    fn the_kitty_stack_pushes_pops_and_changes_its_top() {
        assert_eq!(fed(b"\x1b[>1u\x1b[>5u").kitty_stack(), [1, 5]);
        assert_eq!(fed(b"\x1b[>1u\x1b[>5u\x1b[<u").kitty_stack(), [1]);
        assert_eq!(
            fed(b"\x1b[>1u\x1b[>5u\x1b[<9u").kitty_stack(),
            Vec::<u32>::new()
        );
        assert_eq!(fed(b"\x1b[>1u\x1b[=4;2u").kitty_stack(), [5]);
        assert_eq!(fed(b"\x1b[>5u\x1b[=4;3u").kitty_stack(), [1]);
        assert_eq!(fed(b"\x1b[>1u\x1b[=8u").kitty_stack(), [8]);
    }

    #[test]
    fn the_kitty_stack_has_a_limit() {
        let mut bytes = Vec::new();
        for flags in 0..40 {
            bytes.extend(format!("\x1b[>{flags}u").bytes());
        }
        let ledger = fed(&bytes);
        assert_eq!(ledger.kitty_stack().len(), KITTY_STACK_LIMIT);
        assert_eq!(ledger.kitty_stack().last(), Some(&39));
    }

    #[test]
    fn modify_other_keys_is_set_and_reset() {
        assert_eq!(fed(b"\x1b[>4;2m").modify_other_keys(), Some(2));
        assert_eq!(fed(b"\x1b[>4;2m\x1b[>4m").modify_other_keys(), None);
        assert_eq!(fed(b"\x1b[>4;2m\x1b[>4;m").modify_other_keys(), None);
        assert_eq!(fed(b"\x1b[>4;2m\x1b[>4n").modify_other_keys(), None);
    }

    #[test]
    fn the_title_is_the_last_one_set_with_its_semicolons() {
        assert_eq!(
            fed(b"\x1b]0;first\x07\x1b]2;a;b\x1b\\").title(),
            Some(&b"a;b"[..])
        );
    }

    #[test]
    fn saving_and_restoring_the_cursor_are_events() {
        let mut ledger = Ledger::new();
        let events: Vec<_> = b"\x1b[s\x1b7\x1b[u\x1b[1;2s"
            .iter()
            .filter_map(|byte| ledger.step(*byte))
            .collect();
        assert_eq!(
            events,
            [
                Event::Saved(CursorSave::Csi),
                Event::Saved(CursorSave::Dec),
                Event::RestoredCsi
            ]
        );
    }

    #[test]
    fn a_full_reset_forgets_everything() {
        let ledger = fed(b"\x1b[?1004h\x1b[>1u\x1b[3;9r\x1b=\x1bc");
        assert!(ledger.modes().is_empty());
        assert_eq!(ledger.kitty_stack(), [0u32; 0]);
        assert!(!ledger.margins_set());
        assert!(!ledger.application_keypad());
    }

    #[test]
    fn a_mode_reset_that_starts_on_is_tracked_until_it_is_set_again() {
        assert!(fed(b"\x1b[?7l\x1b[?25l").modes().contains(&Mode::Autowrap));
        assert!(
            fed(b"\x1b[?7l\x1b[?25l")
                .modes()
                .contains(&Mode::CursorVisible)
        );
        assert!(fed(b"\x1b[?7l\x1b[?7h").modes().is_empty());
        assert!(fed(b"\x1b[?7h").modes().is_empty());
    }

    #[test]
    fn synchronized_output_is_on_between_its_set_and_reset() {
        assert!(fed(b"\x1b[?2026h").synchronized());
        assert!(!fed(b"\x1b[?2026h\x1b[?2026l").synchronized());
    }

    #[test]
    fn a_sequence_split_across_feeds_counts_once() {
        let mut ledger = Ledger::new();
        ledger.feed(b"\x1b[?10");
        ledger.feed(b"04h");
        assert!(ledger.modes().contains(&Mode::FocusReports));
    }
}
