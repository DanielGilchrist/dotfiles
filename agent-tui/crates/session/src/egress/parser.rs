use super::classify::{self, MAX_CSI, Reading, Route};
use super::table::{Class, Refusal, Terminator};

/// Takes what the parser read, one item at a time, in the order the program
/// printed them. `bytes` are what is forwarded if the class is, the item as
/// the program printed it unless the parser trimmed it, and are empty for a
/// string that was only skipped.
pub trait Sink {
    fn handle(&mut self, bytes: &[u8], class: Class);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringKind {
    /// Read for its number, kept if listed and skipped if not.
    Osc,
    /// Always skipped, for this reason.
    Opaque(Refusal),
}

/// A string being kept is bounded by its limit. One being skipped holds
/// nothing, so it runs to its terminator however long it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// An OSC's number, which says whether to keep what follows.
    Number,
    Collecting {
        limit: usize,
    },
    Skipping(Refusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Str {
    kind: StringKind,
    stage: Stage,
}

impl Str {
    /// What a string that is cut short is counted as: the reason it was being
    /// skipped, or an abort when it was being kept.
    fn cut_as(self) -> Refusal {
        match self.stage {
            Stage::Skipping(refusal) => refusal,
            Stage::Number | Stage::Collecting { .. } => Refusal::Aborted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Csi {
    Parameters,
    Intermediates,
    /// Swallowing a sequence that is malformed or too long, up to its final
    /// byte, for this reason.
    Ignoring(Refusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    /// In the middle of a multi-byte character, the next byte of which is in
    /// `low..=high`.
    Utf8 {
        remaining: u8,
        low: u8,
        high: u8,
    },
    Escape,
    EscapeIntermediate,
    /// Swallowing an escape with too many intermediates, up to its final byte.
    EscapeIgnoring,
    Csi(Csi),
    Str(Str),
    StrEscape(Str),
}

/// A byte-level reader of what a program prints to its terminal, which
/// survives a sequence split across any number of reads. Every item it hands
/// the sink is whole and self-contained: text of valid UTF-8, one allowed
/// control, a complete CSI, escape or OSC. So whatever it forwards leaves a
/// terminal that reads it in its ground state, and nothing the program
/// printed can leave one waiting in the middle of a sequence.
///
/// It is the only reader of the program's output: the queries the host
/// answers, the allowlist and what the screen and ledger are fed all come
/// from what it read, so none of them can disagree about where a sequence
/// ends.
pub struct Parser {
    state: State,
    held: Vec<u8>,
}

fn printable(byte: u8) -> bool {
    (0x20..=0x7e).contains(&byte)
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            held: Vec::new(),
        }
    }

    /// Whether a sequence or character is half read.
    #[cfg(test)]
    pub fn is_mid_sequence(&self) -> bool {
        self.state != State::Ground
    }

    /// How many bytes of a sequence are kept while it is read.
    #[cfg(test)]
    pub fn held_len(&self) -> usize {
        self.held.len()
    }

    pub fn feed(&mut self, bytes: &[u8], sink: &mut impl Sink) {
        let mut at = 0;
        while let Some(rest) = bytes.get(at..).filter(|rest| !rest.is_empty()) {
            if self.state == State::Ground {
                let run = rest.iter().take_while(|byte| printable(**byte)).count();
                if run > 0 {
                    sink.handle(&rest[..run], Class::Text);
                    at += run;
                    continue;
                }
            }
            if let Some(byte) = rest.first() {
                self.step(*byte, sink);
            }
            at += 1;
        }
    }

    fn step(&mut self, byte: u8, sink: &mut impl Sink) {
        match self.state {
            State::Ground => self.ground(byte, sink),
            State::Utf8 {
                remaining,
                low,
                high,
            } => self.utf8(remaining, low, high, byte, sink),
            State::Escape => self.escape(byte, sink),
            State::EscapeIntermediate => self.escape_intermediate(byte, sink),
            State::EscapeIgnoring => self.escape_ignoring(byte, sink),
            State::Csi(structure) => self.csi(structure, byte, sink),
            State::Str(string) => self.string(string, byte, sink),
            State::StrEscape(string) => self.string_escape(string, byte, sink),
        }
    }

    fn emit(&mut self, class: Class, sink: &mut impl Sink) {
        sink.handle(&self.held, class);
        self.held.clear();
        self.state = State::Ground;
    }

    fn refuse(&mut self, refusal: Refusal, sink: &mut impl Sink) {
        self.emit(Class::Refused(refusal), sink);
    }

    /// Gives up what was half read, and reads `byte` as if nothing had been.
    fn abort_and_reread(&mut self, byte: u8, sink: &mut impl Sink) {
        self.refuse(Refusal::Aborted, sink);
        self.step(byte, sink);
    }

    /// A control character on its own, which a terminal executes wherever it
    /// is, even in the middle of a sequence. Whatever it does has happened
    /// by the time the sequence it interrupted ends, so it is handed on at
    /// once and the sequence after it.
    fn execute(byte: u8, sink: &mut impl Sink) {
        let class = match byte {
            0x07..=0x0f => Class::Control,
            _ => Class::Refused(Refusal::Control),
        };
        sink.handle(&[byte], class);
    }

    fn ground(&mut self, byte: u8, sink: &mut impl Sink) {
        match byte {
            0x1b => {
                self.held.push(byte);
                self.state = State::Escape;
            }
            0x00..=0x1a | 0x1c..=0x1f | 0x7f => Self::execute(byte, sink),
            0x20..=0x7e => sink.handle(&[byte], Class::Text),
            0x80..=0x9f => sink.handle(&[byte], Class::Refused(Refusal::StrayC1)),
            0xa0..=0xc1 | 0xf5..=0xff => {
                sink.handle(&[byte], Class::Refused(Refusal::InvalidUtf8));
            }
            0xc2..=0xdf => self.begin_character(byte, 1, 0x80, 0xbf),
            0xe0 => self.begin_character(byte, 2, 0xa0, 0xbf),
            0xed => self.begin_character(byte, 2, 0x80, 0x9f),
            0xe1..=0xec | 0xee | 0xef => self.begin_character(byte, 2, 0x80, 0xbf),
            0xf0 => self.begin_character(byte, 3, 0x90, 0xbf),
            0xf1..=0xf3 => self.begin_character(byte, 3, 0x80, 0xbf),
            0xf4 => self.begin_character(byte, 3, 0x80, 0x8f),
        }
    }

    fn begin_character(&mut self, lead: u8, remaining: u8, low: u8, high: u8) {
        self.held.push(lead);
        self.state = State::Utf8 {
            remaining,
            low,
            high,
        };
    }

    fn utf8(&mut self, remaining: u8, low: u8, high: u8, byte: u8, sink: &mut impl Sink) {
        if !(low..=high).contains(&byte) {
            self.refuse(Refusal::InvalidUtf8, sink);
            self.step(byte, sink);
            return;
        }
        self.held.push(byte);
        if remaining > 1 {
            self.state = State::Utf8 {
                remaining: remaining - 1,
                low: 0x80,
                high: 0xbf,
            };
            return;
        }
        let c1 = matches!(self.held.as_slice(), [0xc2, 0x80..=0x9f]);
        let class = if c1 {
            Class::Refused(Refusal::StrayC1)
        } else {
            Class::Text
        };
        self.emit(class, sink);
    }

    fn escape(&mut self, byte: u8, sink: &mut impl Sink) {
        let string = |kind: StringKind, stage: Stage| Str { kind, stage };
        match byte {
            b'[' => {
                self.held.push(byte);
                self.state = State::Csi(Csi::Parameters);
            }
            b']' => {
                self.held.push(byte);
                self.state = State::Str(string(StringKind::Osc, Stage::Number));
            }
            b'P' | b'X' | b'^' | b'_' => {
                let refusal = match byte {
                    b'P' => Refusal::Dcs,
                    b'X' => Refusal::Sos,
                    b'^' => Refusal::Pm,
                    _ => Refusal::Apc,
                };
                self.held.clear();
                self.state = State::Str(string(
                    StringKind::Opaque(refusal),
                    Stage::Skipping(refusal),
                ));
            }
            0x20..=0x2f => {
                self.held.push(byte);
                self.state = State::EscapeIntermediate;
            }
            0x30..=0x7e => {
                self.held.push(byte);
                let class = classify::escape(&self.held);
                self.emit(class, sink);
            }
            0x18 | 0x1a => self.refuse(Refusal::Aborted, sink),
            _ => self.abort_and_reread(byte, sink),
        }
    }

    fn escape_intermediate(&mut self, byte: u8, sink: &mut impl Sink) {
        match byte {
            0x20..=0x2f if self.held.len() >= MAX_CSI => {
                self.held.clear();
                self.state = State::EscapeIgnoring;
            }
            0x20..=0x2f => self.held.push(byte),
            0x30..=0x7e => {
                self.held.push(byte);
                let class = classify::escape(&self.held);
                self.emit(class, sink);
            }
            0x18 | 0x1a => self.refuse(Refusal::Aborted, sink),
            _ => self.abort_and_reread(byte, sink),
        }
    }

    fn escape_ignoring(&mut self, byte: u8, sink: &mut impl Sink) {
        match byte {
            0x20..=0x2f => {}
            0x30..=0x7e => self.refuse(Refusal::Overlong, sink),
            0x18 | 0x1a => self.refuse(Refusal::Aborted, sink),
            _ => self.abort_and_reread(byte, sink),
        }
    }

    fn csi(&mut self, structure: Csi, byte: u8, sink: &mut impl Sink) {
        match (structure, byte) {
            (_, 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f) => Self::execute(byte, sink),
            (_, 0x18 | 0x1a) => self.refuse(Refusal::Aborted, sink),
            (Csi::Parameters, 0x30..=0x3f) => self.collect_csi(byte, Csi::Parameters),
            (Csi::Intermediates, 0x30..=0x3f) => self.ignore_csi(Refusal::MalformedCsi),
            (Csi::Parameters | Csi::Intermediates, 0x20..=0x2f) => {
                self.collect_csi(byte, Csi::Intermediates);
            }
            (Csi::Ignoring(_), 0x20..=0x3f) => {}
            (Csi::Ignoring(refusal), 0x40..=0x7e) => self.refuse(refusal, sink),
            (Csi::Parameters | Csi::Intermediates, 0x40..=0x7e) => {
                self.held.push(byte);
                match classify::csi(&self.held) {
                    Reading::Whole(class) => self.emit(class, sink),
                    Reading::Trimmed(kept) => {
                        sink.handle(&[], Class::Refused(Refusal::UnlistedMode));
                        sink.handle(&kept, Class::Modes);
                        self.held.clear();
                        self.state = State::Ground;
                    }
                }
            }
            _ => self.abort_and_reread(byte, sink),
        }
    }

    fn collect_csi(&mut self, byte: u8, next: Csi) {
        if self.held.len() >= MAX_CSI {
            self.ignore_csi(Refusal::Overlong);
            return;
        }
        self.held.push(byte);
        self.state = State::Csi(next);
    }

    fn ignore_csi(&mut self, refusal: Refusal) {
        self.held.clear();
        self.state = State::Csi(Csi::Ignoring(refusal));
    }

    fn string(&mut self, mut string: Str, byte: u8, sink: &mut impl Sink) {
        if string.stage == Stage::Number {
            if byte.is_ascii_digit() {
                self.held.push(byte);
                string.stage = match self.held.get(2..) {
                    Some(number) if number.len() > 5 => self.skip_osc(),
                    _ => Stage::Number,
                };
                self.state = State::Str(string);
                return;
            }
            string.stage = match self.held.get(2..).map(classify::route) {
                Some(Route::Collect { limit }) => Stage::Collecting { limit },
                _ => self.skip_osc(),
            };
            self.state = State::Str(string);
            self.step(byte, sink);
            return;
        }
        match byte {
            0x18 | 0x1a => self.refuse(string.cut_as(), sink),
            0x1b => {
                if matches!(string.stage, Stage::Collecting { .. }) {
                    self.held.push(byte);
                }
                self.state = State::StrEscape(string);
            }
            0x07 if string.kind == StringKind::Osc => {
                self.finish(string, Terminator::Bel, sink);
            }
            _ => self.payload(string, byte),
        }
    }

    /// Drops what an OSC kept so far, which is not wanted after all.
    fn skip_osc(&mut self) -> Stage {
        self.held.clear();
        Stage::Skipping(Refusal::UnlistedOsc)
    }

    fn payload(&mut self, mut string: Str, byte: u8) {
        if let Stage::Collecting { limit } = string.stage {
            self.held.push(byte);
            if self.held.len() > limit {
                self.held.clear();
                string.stage = Stage::Skipping(Refusal::Overlong);
            }
        }
        self.state = State::Str(string);
    }

    fn string_escape(&mut self, string: Str, byte: u8, sink: &mut impl Sink) {
        if byte == b'\\' {
            self.finish(string, Terminator::St, sink);
            return;
        }
        self.refuse(string.cut_as(), sink);
        self.step(0x1b, sink);
        self.step(byte, sink);
    }

    /// A string ended by `terminator`, of which a `St` has its `ESC` already
    /// and only the `\` to come.
    fn finish(&mut self, string: Str, terminator: Terminator, sink: &mut impl Sink) {
        match string.stage {
            Stage::Collecting { .. } => {
                if let Some(last) = terminator.bytes().last() {
                    self.held.push(*last);
                }
                let class = classify::osc(&self.held, terminator);
                self.emit(class, sink);
            }
            Stage::Skipping(refusal) => self.refuse(refusal, sink),
            Stage::Number => self.refuse(Refusal::Aborted, sink),
        }
    }
}
