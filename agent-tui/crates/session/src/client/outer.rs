use std::io::{self, Write};
use std::sync::{Mutex, PoisonError, TryLockError};

use crate::ledger::{ALTERNATE_SCREEN_MODES, Ledger};

/// What undoes everything `ledger` saw a program turn on, so a terminal that
/// showed it is left as it was found: keyboard modes popped, mouse, paste
/// and focus reports off, the scroll region cleared, the alternate screen
/// left, then drawing attributes reset and the cursor shown.
fn restoration(ledger: &Ledger) -> Vec<u8> {
    let mut out = Vec::new();
    if !ledger.kitty_stack().is_empty() {
        out.extend(format!("\x1b[<{}u", ledger.kitty_stack().len()).bytes());
    }
    if ledger.modify_other_keys().is_some() {
        out.extend(b"\x1b[>4m");
    }
    if ledger.application_keypad() {
        out.extend(b"\x1b>");
    }
    for mode in ledger
        .modes()
        .iter()
        .filter(|mode| !ALTERNATE_SCREEN_MODES.contains(mode))
    {
        let direction = if mode.starts_on() { 'h' } else { 'l' };
        out.extend(format!("\x1b[?{}{direction}", mode.number()).bytes());
    }
    if ledger.margins_set() {
        out.extend(b"\x1b[r");
    }
    for mode in ALTERNATE_SCREEN_MODES.iter().rev() {
        if ledger.modes().contains(mode) {
            out.extend(format!("\x1b[?{}l", mode.number()).bytes());
        }
    }
    out.extend(b"\x1b[0m\x1b[?25h");
    out
}

struct State {
    ledger: Ledger,
    sink: Box<dyn Write + Send>,
    /// Whether anything was drawn, so a client that never attached leaves
    /// the terminal exactly as it was.
    drawn: bool,
    restored: bool,
}

/// The terminal the client draws on. It sees what is written to it, so it
/// knows what the program and the host's replay turned on, and
/// [`Outer::restore`] turns exactly that off.
pub struct Outer(Mutex<State>);

impl Outer {
    pub fn new(sink: impl Write + Send + 'static) -> Self {
        Self(Mutex::new(State {
            ledger: Ledger::new(),
            sink: Box::new(sink),
            drawn: false,
            restored: false,
        }))
    }

    pub fn show(&self, bytes: &[u8]) -> io::Result<()> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.drawn |= !bytes.is_empty();
        state.ledger.feed(bytes);
        state.sink.write_all(bytes)?;
        state.sink.flush()
    }

    /// Writes the restoration once. A second call, from a panic hook after
    /// the guard or the other way round, writes nothing.
    pub fn restore(&self) {
        let mut state = match self.0.try_lock() {
            Ok(state) => state,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        if state.restored || !state.drawn {
            return;
        }
        state.restored = true;
        let bytes = restoration(&state.ledger);
        let _ = state.sink.write_all(&bytes);
        let _ = state.sink.flush();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn restoring(output: &[u8]) -> String {
        let mut ledger = Ledger::new();
        ledger.feed(output);
        String::from_utf8(restoration(&ledger)).expect("utf-8")
    }

    #[test]
    fn a_terminal_nothing_was_changed_on_only_gets_attributes_reset_and_the_cursor_shown() {
        assert_eq!(restoring(b"plain text\r\n"), "\x1b[0m\x1b[?25h");
    }

    #[test]
    fn everything_turned_on_is_turned_off_and_the_alternate_screen_is_left_last() {
        let restored = restoring(
            b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?1004h\x1b[>1u\x1b[>5u\x1b[>4;2m\x1b=\x1b[3;9r\x1b[?25l",
        );
        assert_eq!(
            restored,
            "\x1b[<2u\x1b[>4m\x1b>\x1b[?25h\x1b[?1000l\x1b[?1004l\x1b[?1006l\x1b[?2004l\x1b[r\x1b[?1049l\x1b[0m\x1b[?25h"
        );
    }

    #[test]
    fn a_mode_the_program_turned_off_again_is_not_touched() {
        let restored = restoring(b"\x1b[?1000h\x1b[?1000l\x1b[>1u\x1b[<u\x1b[?1049h\x1b[?1049l");
        assert_eq!(restored, "\x1b[0m\x1b[?25h");
    }

    #[test]
    fn autowrap_the_program_turned_off_is_turned_back_on() {
        assert!(restoring(b"\x1b[?7l").contains("\x1b[?7h"));
    }

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Sink {
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().map(|bytes| bytes.clone()).unwrap_or_default()
        }
    }

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| io::Error::other("poisoned"))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_terminal_nothing_was_drawn_on_gets_nothing_written_on_restore() {
        let written = Sink::default();
        let outer = Outer::new(written.clone());
        outer.restore();
        assert_eq!(written.bytes(), Vec::<u8>::new());
    }

    #[test]
    fn restoring_writes_once_however_often_it_is_called() {
        let written = Sink::default();
        let outer = Outer::new(written.clone());
        outer.show(b"\x1b[?1049h").expect("shows");
        outer.restore();
        outer.restore();
        let written = written.bytes();
        assert_eq!(
            String::from_utf8(written).expect("utf-8"),
            "\x1b[?1049h\x1b[?1049l\x1b[0m\x1b[?25h"
        );
    }
}
