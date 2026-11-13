//! `agent keys`: press a key and learn whether it works as the leave key or
//! an agent chord in this terminal, and what to write in the config for it.

mod classify;
mod decode;
mod known;
mod session;

use std::io::{IsTerminal, Write};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::time::Duration;

use agent_core::keyspec::{InvalidKeySpec, KeySpec};
use anyhow::{Result, bail};
use crossterm::event::{PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::execute;
use crossterm::terminal::disable_raw_mode;
use decode::{Decoder, ESC_WAIT};
use known::ATTRIBUTION;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::unistd::read;
use session::{Session, Step, claim_lines};

use crate::keyspec::Keyboard;
use crate::{REPORTED_KEYS, detect_keyboard, restore_on_panic};

pub struct Options {
    pub config_path: PathBuf,
}

impl Keyboard {
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::Disambiguated => "this terminal reports the kitty keyboard protocol",
            Self::Legacy => "legacy key reporting",
        }
    }
}

/// What a config spec means, for `agent keys --explain`.
pub struct Explained(KeySpec);

pub fn explain(text: &str) -> Result<Explained, InvalidKeySpec> {
    text.parse().map(Explained)
}

impl std::fmt::Display for Explained {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let spec = self.0;
        writeln!(f, "{} shows as {spec}", spec.written())?;
        if spec.needs_kitty() {
            writeln!(
                f,
                "needs the kitty keyboard protocol, so a terminal without it falls back to the default"
            )?;
        } else {
            writeln!(f, "any terminal can send it")?;
        }
        match known::claims_on(spec) {
            None => write!(f, "no known uses ({ATTRIBUTION})"),
            Some(collision) => write!(f, "{}", claim_lines("known uses", &collision).join("\n")),
        }
    }
}

/// Restores the terminal however `agent keys` ends, including by panic.
struct Guard {
    keyboard: Keyboard,
}

impl Guard {
    fn enter() -> Result<Self> {
        let keyboard = detect_keyboard()?;
        restore_on_panic(move || leave(keyboard));
        let guard = Self { keyboard };
        if keyboard == Keyboard::Disambiguated {
            execute!(
                std::io::stdout(),
                PushKeyboardEnhancementFlags(REPORTED_KEYS)
            )?;
        }
        Ok(guard)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        leave(self.keyboard);
    }
}

fn leave(keyboard: Keyboard) {
    if keyboard == Keyboard::Disambiguated {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = disable_raw_mode();
}

fn say(lines: &[String]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    for line in lines {
        for part in line.split('\n') {
            write!(out, "{part}\r\n")?;
        }
    }
    out.flush()?;
    Ok(())
}

/// Reads the keys a terminal sends and prints what each is worth. The raw
/// mode and the kitty flags are the TUI's, so what it hears is what the TUI
/// would. Bytes typed while the keyboard is detected are eaten by that
/// detection.
pub fn run(options: Options) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("agent keys needs a terminal. `agent keys --explain <key>` works without one");
    }
    let guard = Guard::enter()?;
    let mut session = Session::new(guard.keyboard, options.config_path);
    say(&session.opening())?;
    let stdin = std::io::stdin();
    let mut decoder = Decoder::default();
    let mut buffer = [0u8; 256];
    loop {
        let keys = if decoder.waiting() && !readable(&stdin, ESC_WAIT)? {
            decoder.flush().into_iter().collect()
        } else {
            match read(stdin.as_fd(), &mut buffer) {
                Ok(0) => bail!("the terminal closed"),
                Ok(read) => decoder.feed(&buffer[..read]),
                Err(Errno::EINTR) => continue,
                Err(error) => return Err(error.into()),
            }
        };
        for key in keys {
            match session.press(&key) {
                Step::Show(lines) => say(&lines)?,
                Step::Done => return Ok(()),
            }
        }
    }
}

fn readable(stdin: &std::io::Stdin, wait: Duration) -> Result<bool> {
    let timeout = PollTimeout::try_from(wait)?;
    loop {
        let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, timeout) {
            Err(Errno::EINTR) => {}
            result => return Ok(result? > 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_good_spec_says_what_it_means_whether_it_needs_kitty_and_what_is_known_about_it() {
        assert_eq!(
            explain("ALT-,").expect("parses").to_string(),
            "alt-, shows as ⌥,\nany terminal can send it\nno known uses (from memory, unchecked)"
        );
        let backslash = explain("ctrl-\\").expect("parses").to_string();
        assert!(
            backslash.starts_with("ctrl-\\ shows as ⌃\\\n"),
            "{backslash}"
        );
        let kitty = explain("ctrl-shift-j").expect("parses").to_string();
        assert!(
            kitty.contains("needs the kitty keyboard protocol"),
            "{kitty}"
        );
        let claimed = explain("alt-b").expect("parses").to_string();
        assert!(
            claimed.contains("readline and shells: back a word"),
            "{claimed}"
        );
    }

    #[test]
    fn a_bad_spec_is_the_real_parsers_error() {
        let Err(error) = explain("banana") else {
            panic!("rejected");
        };
        assert!(
            error.to_string().starts_with("\"banana\" isn't a key"),
            "{error}"
        );
        let Err(error) = explain("x") else {
            panic!("rejected");
        };
        assert!(
            error.to_string().starts_with("\"x\" is a key on its own"),
            "{error}"
        );
    }
}
