//! Splits the bytes a terminal sent into keys, the way crossterm's parser
//! does for a terminal in raw mode.
//!
//! crossterm hands out events and keeps the bytes behind them to itself, and
//! its parser isn't public, so the probe feeds bytes to this port of it, one
//! at a time as crossterm does, and keeps the bytes of each key. It is a
//! copy of crossterm 0.29's logic, not shared code, so the differential test
//! in this file runs both on the same bytes. What it doesn't read (the
//! Linux console's `ESC [ [ A`, keypad and media codes, modifier bits
//! beyond Shift, Alt, Ctrl and Super) is a key with no event.
//!
//! Where crossterm answers a lone Esc at once if nothing else has arrived,
//! the probe waits for `ESC_WAIT` first, so a key split across reads by ssh
//! or a multiplexer is still one key.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// How long a lone Esc waits for the rest of its sequence.
pub const ESC_WAIT: Duration = Duration::from_millis(25);

/// The bytes one key press sent, and the event the TUI would see for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawKey {
    pub bytes: Vec<u8>,
    pub event: Option<KeyEvent>,
}

enum Parsed {
    Wait,
    Done(Option<KeyEvent>),
}

/// Bytes read so far that aren't a whole key yet.
#[derive(Debug, Default)]
pub struct Decoder {
    buffer: Vec<u8>,
}

impl Decoder {
    /// Whether a partial key is waiting for more bytes, and so for a flush.
    pub fn waiting(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// The keys `bytes` completes. A lone Esc stays until more bytes come or
    /// `flush`.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<RawKey> {
        let mut keys = Vec::new();
        for byte in bytes {
            self.buffer.push(*byte);
            if let Parsed::Done(event) = parse(&self.buffer, true) {
                keys.push(RawKey {
                    bytes: std::mem::take(&mut self.buffer),
                    event,
                });
            }
        }
        keys
    }

    /// Nothing more is coming: a lone Esc is Esc, and any other partial
    /// sequence is a key no one can read.
    pub fn flush(&mut self) -> Option<RawKey> {
        if self.buffer.is_empty() {
            return None;
        }
        let event = match parse(&self.buffer, false) {
            Parsed::Done(event) => event,
            Parsed::Wait => None,
        };
        Some(RawKey {
            bytes: std::mem::take(&mut self.buffer),
            event,
        })
    }
}

/// Every key in `bytes`, sent as one read with nothing after it.
#[cfg(test)]
pub fn all(bytes: &[u8]) -> Vec<RawKey> {
    let mut decoder = Decoder::default();
    let mut keys = decoder.feed(bytes);
    keys.extend(decoder.flush());
    keys
}

fn parse(buffer: &[u8], more: bool) -> Parsed {
    match buffer {
        [] | [0x1b, b'O'] => Parsed::Wait,
        [0x1b] if more => Parsed::Wait,
        [0x1b] | [0x1b, 0x1b, ..] => {
            Parsed::Done(Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        }
        [0x1b, b'O', third, ..] => Parsed::Done(ss3(*third)),
        [0x1b, b'[', ..] => csi(buffer),
        [0x1b, rest @ ..] => match parse(rest, more) {
            Parsed::Wait => Parsed::Wait,
            Parsed::Done(event) => Parsed::Done(
                event.map(|event| KeyEvent::new(event.code, event.modifiers | KeyModifiers::ALT)),
            ),
        },
        [first, ..] => single(*first, buffer),
    }
}

fn single(first: u8, buffer: &[u8]) -> Parsed {
    let control = KeyModifiers::CONTROL;
    Parsed::Done(match first {
        b'\r' => Some(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        b'\n' => Some(KeyEvent::new(KeyCode::Char('j'), control)),
        b'\t' => Some(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        0x7f => Some(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        0x01..=0x1a => Some(KeyEvent::new(
            KeyCode::Char(char::from(first - 1 + b'a')),
            control,
        )),
        0x1c..=0x1f => Some(KeyEvent::new(
            KeyCode::Char(char::from(first - 0x1c + b'4')),
            control,
        )),
        0 => Some(KeyEvent::new(KeyCode::Char(' '), control)),
        _ => return character(buffer),
    })
}

fn character(buffer: &[u8]) -> Parsed {
    match std::str::from_utf8(buffer) {
        Ok(text) => Parsed::Done(text.chars().next().map(|c| {
            let shift = if c.is_uppercase() {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };
            KeyEvent::new(KeyCode::Char(c), shift)
        })),
        Err(error) if error.error_len().is_none() => Parsed::Wait,
        Err(_) => Parsed::Done(None),
    }
}

fn ss3(third: u8) -> Option<KeyEvent> {
    match third {
        b'D' => Some(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
        b'C' => Some(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
        b'A' => Some(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
        b'B' => Some(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        b'H' => Some(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
        b'F' => Some(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
        b'P'..=b'S' => Some(KeyEvent::new(
            KeyCode::F(1 + third - b'P'),
            KeyModifiers::NONE,
        )),
        _ => None,
    }
}

/// The modifiers a mask names, or none if it names one a spec can't
/// (Hyper, Meta), so that the key is unreadable rather than a different key.
fn modifiers(mask: u8) -> Option<KeyModifiers> {
    let bits = mask.saturating_sub(1);
    if bits & (16 | 32) != 0 {
        return None;
    }
    Some(
        [
            (1, KeyModifiers::SHIFT),
            (2, KeyModifiers::ALT),
            (4, KeyModifiers::CONTROL),
            (8, KeyModifiers::SUPER),
        ]
        .into_iter()
        .filter(|(bit, _)| bits & bit != 0)
        .fold(KeyModifiers::NONE, |all, (_, modifier)| all | modifier),
    )
}

/// `modifiers:event-type` as one field, as crossterm reads it.
fn mask(field: Option<&str>) -> Option<u8> {
    field?.split(':').next()?.parse().ok()
}

fn last_is_final(buffer: &[u8]) -> bool {
    buffer.last().is_some_and(|byte| (64..=126).contains(byte))
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

fn csi(buffer: &[u8]) -> Parsed {
    if buffer.starts_with(PASTE_START) {
        return if buffer.ends_with(PASTE_END) {
            Parsed::Done(None)
        } else {
            Parsed::Wait
        };
    }
    let Some(&third) = buffer.get(2) else {
        return Parsed::Wait;
    };
    let named = |code| Parsed::Done(Some(KeyEvent::new(code, KeyModifiers::NONE)));
    match third {
        b'[' if buffer.len() == 3 => Parsed::Wait,
        b'M' if buffer.len() < 6 => Parsed::Wait,
        b'<' if !(buffer.len() > 3 && matches!(buffer[buffer.len() - 1], b'M' | b'm')) => {
            Parsed::Wait
        }
        b'D' => named(KeyCode::Left),
        b'C' => named(KeyCode::Right),
        b'A' => named(KeyCode::Up),
        b'B' => named(KeyCode::Down),
        b'H' => named(KeyCode::Home),
        b'F' => named(KeyCode::End),
        b'P' => named(KeyCode::F(1)),
        b'Q' => named(KeyCode::F(2)),
        b'S' => named(KeyCode::F(4)),
        b'Z' => Parsed::Done(Some(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))),
        b';' => Parsed::Done(modified(buffer)),
        b'?' if last_is_final(buffer) => Parsed::Done(None),
        b'0'..=b'9' if buffer.len() > 3 && last_is_final(buffer) => {
            Parsed::Done(match buffer[buffer.len() - 1] {
                b'M' | b'R' => None,
                b'~' => special(buffer),
                b'u' => kitty(buffer),
                _ => modified(buffer),
            })
        }
        b'?' | b'0'..=b'9' => Parsed::Wait,
        _ => Parsed::Done(None),
    }
}

fn params(buffer: &[u8]) -> Option<&str> {
    std::str::from_utf8(&buffer[2..buffer.len() - 1]).ok()
}

fn modified(buffer: &[u8]) -> Option<KeyEvent> {
    let mut fields = params(buffer)?.split(';');
    fields.next();
    let held = match mask(fields.next()) {
        Some(mask) => modifiers(mask)?,
        None if buffer.len() > 3 => {
            let digit = char::from(buffer[buffer.len() - 2]).to_digit(10)?;
            modifiers(u8::try_from(digit).ok()?)?
        }
        None => KeyModifiers::NONE,
    };
    let code = match buffer[buffer.len() - 1] {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'F' => KeyCode::End,
        b'H' => KeyCode::Home,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return None,
    };
    Some(KeyEvent::new(code, held))
}

/// What the kitty protocol sends for a key that has no character: F13 to
/// F35 are the only ones a spec can name.
fn functional(codepoint: u32) -> Option<KeyCode> {
    let number = codepoint.checked_sub(57376)?;
    (number <= 22).then(|| KeyCode::F(13 + u8::try_from(number).unwrap_or(0)))
}

/// Private-use codepoints are functional keys, never characters.
fn is_functional(codepoint: u32) -> bool {
    (57344..=63743).contains(&codepoint)
}

fn kitty(buffer: &[u8]) -> Option<KeyEvent> {
    let mut fields = params(buffer)?.split(';');
    let mut codepoints = fields.next()?.split(':');
    let codepoint: u32 = codepoints.next()?.parse().ok()?;
    let mut held = match mask(fields.next()) {
        Some(mask) => modifiers(mask)?,
        None => KeyModifiers::NONE,
    };
    let mut code = if is_functional(codepoint) {
        functional(codepoint)?
    } else {
        match char::from_u32(codepoint)? {
            '\x1b' => KeyCode::Esc,
            '\r' => KeyCode::Enter,
            '\t' if held.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
            '\t' => KeyCode::Tab,
            '\x7f' => KeyCode::Backspace,
            other => KeyCode::Char(other),
        }
    };
    if held.contains(KeyModifiers::SHIFT)
        && let Some(shifted) = codepoints
            .next()
            .and_then(|digits| digits.parse().ok())
            .and_then(char::from_u32)
    {
        code = KeyCode::Char(shifted);
        held.remove(KeyModifiers::SHIFT);
    }
    Some(KeyEvent::new(code, held))
}

fn special(buffer: &[u8]) -> Option<KeyEvent> {
    let mut fields = params(buffer)?.split(';');
    let number: u8 = fields.next()?.parse().ok()?;
    let held = match mask(fields.next()) {
        Some(mask) => modifiers(mask)?,
        None => KeyModifiers::NONE,
    };
    let code = match number {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        11..=15 => KeyCode::F(number - 10),
        17..=21 => KeyCode::F(number - 11),
        23..=26 => KeyCode::F(number - 12),
        28..=29 => KeyCode::F(number - 15),
        31..=34 => KeyCode::F(number - 17),
        _ => return None,
    };
    Some(KeyEvent::new(code, held))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key in `chunks`, each read arriving as its own chunk and
    /// nothing more coming after the last.
    fn decoded(chunks: &[&[u8]]) -> Vec<RawKey> {
        let mut decoder = Decoder::default();
        let mut keys = Vec::new();
        for chunk in chunks {
            keys.extend(decoder.feed(chunk));
        }
        keys.extend(decoder.flush());
        keys
    }

    fn only(bytes: &[u8]) -> RawKey {
        let mut keys = decoded(&[bytes]);
        assert_eq!(keys.len(), 1, "{bytes:?} is one key");
        keys.remove(0)
    }

    fn event(bytes: &[u8]) -> Option<(KeyCode, KeyModifiers)> {
        only(bytes).event.map(|event| (event.code, event.modifiers))
    }

    const CTRL: KeyModifiers = KeyModifiers::CONTROL;
    const ALT: KeyModifiers = KeyModifiers::ALT;
    const NONE: KeyModifiers = KeyModifiers::NONE;

    #[test]
    fn a_key_keeps_the_bytes_it_was_sent_as() {
        assert_eq!(only(b"\x1b[24;3~").bytes, b"\x1b[24;3~");
        assert_eq!(only("≤".as_bytes()).bytes, "≤".as_bytes());
    }

    #[test]
    fn a_read_with_several_keys_splits_them() {
        let bytes: Vec<Vec<u8>> = decoded(&[b"a\x1b,\x1b[24~"])
            .into_iter()
            .map(|key| key.bytes)
            .collect();
        assert_eq!(
            bytes,
            [b"a".to_vec(), b"\x1b,".to_vec(), b"\x1b[24~".to_vec()]
        );
    }

    #[test]
    fn a_key_split_across_reads_is_one_key() {
        let keys = decoded(&[b"\x1b[2", b"4~"]);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].bytes, b"\x1b[24~");
        let keys = decoded(&[b"\x1b", b"[24~"]);
        assert_eq!(keys.len(), 1, "a lone Esc waits for the rest");
        assert_eq!(keys[0].bytes, b"\x1b[24~");
    }

    #[test]
    fn a_lone_escape_is_escape_once_nothing_more_comes_and_escape_escape_is_one_key() {
        assert_eq!(event(&[0x1b]), Some((KeyCode::Esc, NONE)));
        let keys = decoded(&[&[0x1b, 0x1b]]);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].event.map(|event| event.code), Some(KeyCode::Esc));
    }

    #[test]
    fn a_sequence_that_never_finishes_is_one_unreadable_key_after_the_flush() {
        let keys = decoded(&[b"\x1b[24"]);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].event, None);
        assert_eq!(keys[0].bytes, b"\x1b[24");
    }

    #[test]
    fn what_a_spec_cannot_name_is_a_key_with_no_event() {
        for bytes in [
            &b"\x1b[[A"[..],
            b"\x1b[57441u",
            b"\x1b[57399u",
            b"\x1b[97;17u",
            b"\x1b[1;33A",
        ] {
            let keys = decoded(&[bytes]);
            assert_eq!(keys.len(), 1, "{bytes:?}");
            assert_eq!(keys[0].event, None, "{bytes:?}");
        }
    }

    #[test]
    fn a_mouse_report_or_a_paste_is_one_unreadable_key_and_never_keys() {
        for bytes in [
            &b"\x1b[<0;12;5M"[..],
            b"\x1b[<0;12;5m",
            b"\x1b[M #!",
            b"\x1b[200~123\x1b[201~",
        ] {
            let keys = decoded(&[bytes]);
            assert_eq!(keys.len(), 1, "{bytes:?}");
            assert_eq!(keys[0].event, None, "{bytes:?}");
            assert_eq!(keys[0].bytes, bytes);
        }
        let split = decoded(&[b"\x1b[<0;1", b"2;5M1"]);
        assert_eq!(split.len(), 2);
        assert_eq!(split[1].bytes, b"1");
    }

    #[test]
    fn function_keys_past_twelve_are_function_keys_not_characters() {
        assert_eq!(event(b"\x1b[57376u"), Some((KeyCode::F(13), NONE)));
        assert_eq!(event(b"\x1b[57398;5u"), Some((KeyCode::F(35), CTRL)));
    }

    #[test]
    fn kitty_sends_f1_f2_and_f4_as_bare_csi_finals() {
        assert_eq!(event(b"\x1b[P"), Some((KeyCode::F(1), NONE)));
        assert_eq!(event(b"\x1b[Q"), Some((KeyCode::F(2), NONE)));
        assert_eq!(event(b"\x1b[S"), Some((KeyCode::F(4), NONE)));
        assert_eq!(event(b"\x1b[1;3S"), Some((KeyCode::F(4), ALT)));
    }

    /// One case: the reads a terminal delivered, and whether the decoder is
    /// expected to read it as crossterm does.
    struct Case {
        reads: &'static [&'static [u8]],
        readable: bool,
    }

    const fn read(reads: &'static [&'static [u8]]) -> Case {
        Case {
            reads,
            readable: true,
        }
    }

    const fn unread(reads: &'static [&'static [u8]]) -> Case {
        Case {
            reads,
            readable: false,
        }
    }

    const CASES: &[Case] = &[
        read(&[&[0x01]]),
        read(&[&[0x08]]),
        read(&[&[0x09]]),
        read(&[&[0x0a]]),
        read(&[&[0x0d]]),
        read(&[&[0x1a]]),
        read(&[&[0x1c]]),
        read(&[&[0x1d]]),
        read(&[&[0x1e]]),
        read(&[&[0x1f]]),
        read(&[&[0x00]]),
        read(&[&[0x7f]]),
        read(&[&[0x1b]]),
        read(&[b"x"]),
        read(&[b"X"]),
        read(&["≤".as_bytes()]),
        read(&[b"\x1b,"]),
        read(&[b"\x1bX"]),
        read(&[b"\x1b\x7f"]),
        read(&[b"\x1b\x01"]),
        read(&[b"\x1b\x1b"]),
        read(&["\x1b≤".as_bytes()]),
        read(&[b"\x1bOP"]),
        read(&[b"\x1bOS"]),
        read(&[b"\x1bOA"]),
        read(&[b"\x1b[11~"]),
        read(&[b"\x1b[15~"]),
        read(&[b"\x1b[17~"]),
        read(&[b"\x1b[24~"]),
        read(&[b"\x1b[24;3~"]),
        read(&[b"\x1b[24;5~"]),
        read(&[b"\x1b[1~"]),
        read(&[b"\x1b[3;5~"]),
        read(&[b"\x1b[5~"]),
        read(&[b"\x1b[P"]),
        read(&[b"\x1b[Q"]),
        read(&[b"\x1b[S"]),
        read(&[b"\x1b[1;5P"]),
        read(&[b"\x1b[1;3S"]),
        read(&[b"\x1b[A"]),
        read(&[b"\x1b[H"]),
        read(&[b"\x1b[F"]),
        read(&[b"\x1b[Z"]),
        read(&[b"\x1b[1;5A"]),
        read(&[b"\x1b[1;2D"]),
        read(&[b"\x1b[105;5u"]),
        read(&[b"\x1b[44;3u"]),
        read(&[b"\x1b[106:74;6u"]),
        read(&[b"\x1b[27u"]),
        read(&[b"\x1b[99;5u"]),
        read(&[b"\x1b[13;2u"]),
        read(&[b"\x1b[9;2u"]),
        read(&[b"\x1b[107;9u"]),
        read(&[b"\x1b[57376u"]),
        read(&[b"\x1b[57377;5u"]),
        read(&[b"\x1b[24", b"~"]),
        read(&[b"\x1b[", b"24~"]),
        read(&[b"\x1b[105;", b"5u"]),
        read(&[b"\x1b", b"[24~"]),
        unread(&[b"\x1b[[A"]),
        unread(&[b"\x1b[57441u"]),
        unread(&[b"\x1b[57399u"]),
        unread(&[b"\x1b[97;17u"]),
        unread(&[b"\x1b[1;33A"]),
    ];

    const CHILD_ENV: &str = "AGENT_DECODE_DIFFERENTIAL_CHILD";
    const SENTINEL: u8 = 0x11;
    const SENTINEL_EVENT: &str = "EV Char('q') 2";
    const GAP: Duration = Duration::from_millis(30);

    /// The other half of `crossterm_and_the_decoder_agree`: run in a pty of
    /// its own, it prints each key event crossterm reads.
    #[test]
    fn crossterm_child() {
        use std::io::Write;
        if std::env::var_os(CHILD_ENV).is_none() {
            return;
        }
        crossterm::terminal::enable_raw_mode().expect("raw mode");
        let mut out = std::io::stdout();
        writeln!(out, "READY\r").expect("writes");
        out.flush().expect("flushes");
        loop {
            if let crossterm::event::Event::Key(key) = crossterm::event::read().expect("reads")
                && key.kind == crossterm::event::KeyEventKind::Press
            {
                writeln!(out, "EV {:?} {}\r", key.code, key.modifiers.bits()).expect("writes");
                out.flush().expect("flushes");
            }
        }
    }

    struct Crossterm {
        writer: Box<dyn std::io::Write + Send>,
        heard: std::sync::Arc<std::sync::Mutex<String>>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
        _master: Box<dyn portable_pty::MasterPty + Send>,
    }

    impl Crossterm {
        fn start() -> Self {
            use std::io::Read;
            let pair = portable_pty::native_pty_system()
                .openpty(portable_pty::PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .expect("a pty");
            let mut command = portable_pty::CommandBuilder::new(
                std::env::current_exe().expect("this test binary"),
            );
            command.args([
                "--exact",
                "probe::decode::tests::crossterm_child",
                "--nocapture",
                "--test-threads=1",
            ]);
            command.env(CHILD_ENV, "1");
            let child = pair.slave.spawn_command(command).expect("spawns");
            drop(pair.slave);
            let heard = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let mut reader = pair.master.try_clone_reader().expect("a reader");
            let sink = std::sync::Arc::clone(&heard);
            std::thread::spawn(move || {
                let mut buffer = [0u8; 4096];
                while let Ok(read @ 1..) = reader.read(&mut buffer) {
                    sink.lock()
                        .expect("lock")
                        .push_str(&String::from_utf8_lossy(&buffer[..read]));
                }
            });
            let this = Self {
                writer: pair.master.take_writer().expect("a writer"),
                _master: pair.master,
                heard,
                child,
            };
            this.wait_for_line("READY");
            this
        }

        fn wait_for_line(&self, line: &str) {
            let started = std::time::Instant::now();
            while started.elapsed() < Duration::from_secs(10) {
                if self.heard.lock().expect("lock").contains(line) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("never heard {line:?}");
        }

        /// The key events crossterm read for `reads`, each sent a moment
        /// apart so none is joined to the next.
        fn events(&mut self, reads: &[&[u8]]) -> Vec<String> {
            use std::io::Write;
            self.heard.lock().expect("lock").clear();
            for chunk in reads {
                self.writer.write_all(chunk).expect("writes");
                self.writer.flush().expect("flushes");
                std::thread::sleep(GAP);
            }
            self.writer.write_all(&[SENTINEL]).expect("writes");
            self.writer.flush().expect("flushes");
            self.wait_for_line(SENTINEL_EVENT);
            let heard = self.heard.lock().expect("lock").clone();
            heard
                .lines()
                .map(str::trim)
                .filter(|line| line.starts_with("EV ") && *line != SENTINEL_EVENT)
                .map(str::to_string)
                .collect()
        }
    }

    impl Drop for Crossterm {
        fn drop(&mut self) {
            let _ = self.child.kill();
        }
    }

    fn shown(event: &KeyEvent) -> String {
        format!("EV {:?} {}", event.code, event.modifiers.bits())
    }

    /// What the decoder reads for `reads`, with a lone Esc settled between
    /// reads the way crossterm does, since nothing else has arrived by then.
    fn decoded_as_crossterm_reads(reads: &[&[u8]]) -> Vec<RawKey> {
        let mut decoder = Decoder::default();
        let mut keys = Vec::new();
        for chunk in reads {
            keys.extend(decoder.feed(chunk));
            if decoder.buffer == [0x1b] {
                keys.extend(decoder.flush());
            }
        }
        keys.extend(decoder.flush());
        keys
    }

    #[test]
    fn crossterm_and_the_decoder_agree_on_every_key_the_decoder_reads_and_it_reports_the_rest_as_unread()
     {
        let mut crossterm = Crossterm::start();
        for case in CASES {
            let mine = decoded_as_crossterm_reads(case.reads);
            if !case.readable {
                assert!(
                    mine.iter().any(|key| key.event.is_none()),
                    "{:?} is reported as unread",
                    case.reads
                );
                continue;
            }
            let ours: Vec<String> = mine
                .iter()
                .map(|key| {
                    key.event
                        .as_ref()
                        .map_or_else(|| "unread".to_string(), shown)
                })
                .collect();
            assert_eq!(ours, crossterm.events(case.reads), "{:?}", case.reads);
        }
    }
}
