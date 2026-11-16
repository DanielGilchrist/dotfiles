use std::io::{self, Write};
use std::num::NonZeroU16;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;

use agent_core::size::TermSize;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

use super::{ClientError, DetachKey, Ended, External, Options, Outer, Seat, Wiring, attach};
use crate::paths::SocketPath;

/// Where the client's keys and screen are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Console {
    /// The controlling terminal: raw mode, its own size, and its resizes.
    /// A terminal that reports no size has `fallback`.
    Tty { fallback: TermSize },
    /// Pipes, as in a script or a test: no raw mode, and this size.
    Piped(TermSize),
}

fn terminal_size() -> io::Result<TermSize> {
    let (cols, rows) = crossterm::terminal::size()?;
    let size = NonZeroU16::new(rows).zip(NonZeroU16::new(cols));
    size.map(|(rows, cols)| TermSize { rows, cols })
        .ok_or_else(|| io::Error::other("the terminal reports no size"))
}

/// Raw mode, off again when dropped.
struct RawMode;

impl RawMode {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

/// Restores the terminal before a panic's message is printed, so the message
/// isn't printed onto a screen the terminal still holds.
fn restore_on_panic(outer: &Arc<Outer>) {
    let outer = Arc::clone(outer);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        outer.restore();
        let _ = disable_raw_mode();
        previous(info);
    }));
}

/// Reports resizes, and the signals that end a process, which would
/// otherwise end it with the terminal still in raw mode and on the program's
/// screen. Registering them here is how they are caught without `unsafe`.
fn forward_signals(outside: Sender<External>) -> io::Result<signal_hook::iterator::Handle> {
    let mut signals = Signals::new([SIGWINCH, SIGTERM, SIGHUP, SIGINT, SIGQUIT])?;
    let handle = signals.handle();
    thread::spawn(move || {
        for signal in signals.forever() {
            let happened = if signal == SIGWINCH {
                match terminal_size() {
                    Ok(size) => External::Resized(size),
                    Err(_) => continue,
                }
            } else {
                External::Terminated(signal)
            };
            if outside.send(happened).is_err() {
                return;
            }
        }
    });
    Ok(handle)
}

/// Runs a client on `console` until the attachment ends. Whatever ends it,
/// including a panic, the outer terminal is put back as it was.
pub fn run(
    socket: &SocketPath,
    seat: Seat,
    detach: Option<DetachKey>,
    console: Console,
) -> Result<Ended, ClientError> {
    let outer = Arc::new(Outer::new(io::stdout()));
    let (outside, external) = mpsc::channel();
    let (size, raw, signals) = match console {
        Console::Tty { fallback } => {
            let size = terminal_size().unwrap_or(fallback);
            restore_on_panic(&outer);
            let raw = RawMode::enter().map_err(ClientError::Terminal)?;
            let signals = forward_signals(outside).map_err(ClientError::Terminal)?;
            (size, Some(raw), Some(signals))
        }
        Console::Piped(size) => (size, None, None),
    };
    let ended = attach(
        socket,
        Options {
            role: seat.role(size),
            detach,
        },
        Wiring {
            input: Box::new(io::stdin()),
            outer: Arc::clone(&outer),
            external,
        },
    );
    outer.restore();
    if let Some(signals) = signals {
        signals.close();
    }
    drop(raw);
    let _ = io::stdout().flush();
    ended
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Mutex;

    use super::*;

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

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
    fn a_panic_puts_the_outer_terminal_back_before_its_message_is_printed() {
        let sink = Sink::default();
        let outer = Arc::new(Outer::new(sink.clone()));
        outer.show(b"\x1b[?1049h\x1b[?1000h").expect("shows");
        restore_on_panic(&outer);
        let panicked = thread::spawn(|| panic!("a panic on purpose")).join();
        assert!(panicked.is_err());
        let written = sink.0.lock().map(|bytes| bytes.clone()).unwrap_or_default();
        let text = String::from_utf8_lossy(&written);
        assert!(
            text.ends_with("\x1b[?1000l\x1b[?1049l\x1b[0m\x1b[?25h"),
            "{text:?}"
        );
    }
}
