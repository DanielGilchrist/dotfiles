use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::unpoisoned;

/// The most the log grows to.
const CAP_BYTES: usize = 256 * 1024;
/// Lines written in a window, so a flood of one kind of event can't be what
/// fills the log or what the host spends its time on.
const LINES_PER_WINDOW: usize = 20;
const WINDOW: Duration = Duration::from_secs(1);

struct State {
    file: File,
    written: usize,
    window_began: Instant,
    in_window: usize,
    suppressed: usize,
}

/// The host's log: warnings and protocol errors, one line each, mode 0600,
/// truncated at every start, rate limited and capped in size.
pub(super) struct Log(Mutex<State>);

impl Log {
    pub(super) fn create(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        Ok(Self(Mutex::new(State {
            file,
            written: 0,
            window_began: Instant::now(),
            in_window: 0,
            suppressed: 0,
        })))
    }

    pub(super) fn warn(&self, message: impl Display) {
        let now = Instant::now();
        let mut state = unpoisoned(self.0.lock());
        if now.duration_since(state.window_began) >= WINDOW {
            let suppressed = state.suppressed;
            state.window_began = now;
            state.in_window = 0;
            state.suppressed = 0;
            if suppressed > 0 {
                state.append(&format!("({suppressed} lines suppressed)\n"));
            }
        }
        if state.in_window >= LINES_PER_WINDOW {
            state.suppressed += 1;
            return;
        }
        state.in_window += 1;
        state.append(&format!("{message}\n"));
    }

    /// Writes panics, which the host's missing standard error would lose.
    pub(super) fn install_panic_hook(self: &Arc<Self>) {
        let log = Arc::clone(self);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log.warn(format_args!("panic: {info}"));
            previous(info);
        }));
    }
}

impl State {
    fn append(&mut self, line: &str) {
        if self.written >= CAP_BYTES {
            return;
        }
        if self.written + line.len() > CAP_BYTES {
            let _ = self.file.write_all(b"(log full)\n");
            self.written = CAP_BYTES;
            return;
        }
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.written += line.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("reads")
    }

    #[test]
    fn a_flood_of_one_event_is_cut_to_a_window_and_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("l.log");
        let log = Log::create(&path).expect("creates");
        for n in 0..500 {
            log.warn(format_args!("refused {n}"));
        }
        assert_eq!(read(&path).lines().count(), LINES_PER_WINDOW);
        std::thread::sleep(WINDOW + Duration::from_millis(50));
        log.warn("after");
        let text = read(&path);
        assert!(text.contains("(480 lines suppressed)"), "{text}");
        assert!(text.ends_with("after\n"));
    }

    #[test]
    fn the_log_stops_growing_at_its_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("l.log");
        let log = Log::create(&path).expect("creates");
        let long = "x".repeat(10_000);
        for round in 0..40 {
            if round > 0 {
                let mut state = unpoisoned(log.0.lock());
                state.window_began = Instant::now()
                    .checked_sub(WINDOW)
                    .unwrap_or(state.window_began);
            }
            for _ in 0..LINES_PER_WINDOW {
                log.warn(&long);
            }
        }
        let size = std::fs::metadata(&path).expect("stat").len();
        assert!(
            size <= u64::try_from(CAP_BYTES).expect("small") + 64,
            "{size}"
        );
        assert!(read(&path).ends_with("(log full)\n"));
    }

    #[test]
    fn a_panic_on_any_thread_is_written_to_the_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("l.log");
        let log = Arc::new(Log::create(&path).expect("creates"));
        log.install_panic_hook();
        let panicked = std::thread::spawn(|| panic!("a panic on purpose")).join();
        assert!(panicked.is_err());
        assert!(read(&path).contains("a panic on purpose"));
    }
}
