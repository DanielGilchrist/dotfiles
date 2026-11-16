use agent_core::keyspec::{Control, Detachable, KittyKey};

/// The key a person presses to leave a session. A terminal sends one key as
/// different bytes depending on its keyboard protocol, so this holds every
/// byte sequence that means it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachKey {
    patterns: Vec<Vec<u8>>,
}

/// The kitty keyboard protocol's modifier field: 1 plus alt (2), ctrl (4),
/// shift (1) and super (8) as they are held.
const KITTY_ALT: u8 = 3;
const KITTY_CTRL: u8 = 5;
const KITTY_CTRL_SHIFT: u8 = 6;
const KITTY_SUPER: u8 = 9;

fn kitty(character: char, modifiers: u8) -> [Vec<u8>; 2] {
    let code = u32::from(character.to_ascii_lowercase());
    [
        format!("\x1b[{code};{modifiers}u").into_bytes(),
        format!("\x1b[{code};{modifiers}:1u").into_bytes(),
    ]
}

/// A control key as its control byte and as the kitty protocol's `CSI u`.
fn ctrl_patterns(control: Control) -> Vec<Vec<u8>> {
    let character = control.char();
    let legacy = u8::try_from(u32::from(character) & 0x1f).ok();
    let mut patterns: Vec<Vec<u8>> = legacy.map(|byte| vec![byte]).into_iter().collect();
    patterns.extend(kitty(character, KITTY_CTRL));
    patterns
}

impl DetachKey {
    /// `ctrl-\`, which no shell or editor uses and a legacy terminal sends
    /// as one byte. It is not the TUI's nav key (`ctrl-]`), because the TUI
    /// embeds this client without a detach key and keeps `ctrl-\` for the
    /// program in the session.
    pub fn standard() -> Self {
        Self {
            patterns: ctrl_patterns(Control::Backslash),
        }
    }

    pub fn new(key: Detachable) -> Self {
        let patterns = match key {
            Detachable::Ctrl(control) => ctrl_patterns(control),
            Detachable::Alt(glyph) => {
                let character = glyph.char();
                let mut legacy = vec![0x1b];
                legacy.extend(character.to_string().bytes());
                let mut patterns = vec![legacy];
                patterns.extend(kitty(character, KITTY_ALT));
                patterns
            }
            Detachable::Kitty(KittyKey::CtrlShift(letter)) => {
                kitty(letter.char(), KITTY_CTRL_SHIFT).to_vec()
            }
            Detachable::Kitty(KittyKey::Super(glyph)) => kitty(glyph.char(), KITTY_SUPER).to_vec(),
            Detachable::Kitty(KittyKey::Ctrl(shadowed)) => {
                kitty(shadowed.char(), KITTY_CTRL).to_vec()
            }
            Detachable::Kitty(KittyKey::Alt(starter)) => kitty(starter.char(), KITTY_ALT).to_vec(),
        };
        Self { patterns }
    }
}

/// What a read of input holds: the bytes to forward, and whether the detach
/// key was among them, in which case nothing after it is forwarded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Scanned {
    pub forward: Vec<u8>,
    pub detach: bool,
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// The most bytes taken for one paste. A paste that never ends otherwise
/// leaves the detach key dead.
const PASTE_LIMIT: usize = 8 * 1024 * 1024;

/// Whether the input is inside a bracketed paste, which the terminal marks
/// with `CSI 200 ~` and `CSI 201 ~`. A paste is data, so the detach key in it
/// is text and not a request to leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Paste {
    Outside,
    Inside,
}

/// Finds the detach key in a stream of input that arrives in arbitrary
/// pieces. Bytes that may be the start of the key, or of a paste marker, are
/// held back until the next read says what they are, or [`Scanner::flush`]
/// gives them up.
#[derive(Debug)]
pub(crate) struct Scanner {
    key: Option<DetachKey>,
    held: Vec<u8>,
    paste: Paste,
    pasted: usize,
}

impl Scanner {
    pub(crate) fn new(key: Option<DetachKey>) -> Self {
        Self {
            key,
            held: Vec::new(),
            paste: Paste::Outside,
            pasted: 0,
        }
    }

    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Scanned {
        let Some(key) = &self.key else {
            return Scanned {
                forward: bytes.to_vec(),
                detach: false,
            };
        };
        let mut buffer = std::mem::take(&mut self.held);
        buffer.extend_from_slice(bytes);
        let mut forward = Vec::with_capacity(buffer.len());
        let mut at = 0;
        while at < buffer.len() {
            let rest = &buffer[at..];
            let marker = match self.paste {
                Paste::Outside => PASTE_START,
                Paste::Inside => PASTE_END,
            };
            if rest.starts_with(marker) {
                forward.extend_from_slice(marker);
                at += marker.len();
                self.paste = match self.paste {
                    Paste::Outside => Paste::Inside,
                    Paste::Inside => Paste::Outside,
                };
                self.pasted = 0;
                continue;
            }
            if self.paste == Paste::Inside && self.pasted >= PASTE_LIMIT {
                self.paste = Paste::Outside;
                self.pasted = 0;
                continue;
            }
            let leaves = self.paste == Paste::Outside;
            if leaves && key.patterns.iter().any(|pattern| rest.starts_with(pattern)) {
                return Scanned {
                    forward,
                    detach: true,
                };
            }
            let could_start =
                |pattern: &[u8]| pattern.len() > rest.len() && pattern.starts_with(rest);
            if could_start(marker) || (leaves && key.patterns.iter().any(|p| could_start(p))) {
                self.held = rest.to_vec();
                return Scanned {
                    forward,
                    detach: false,
                };
            }
            forward.push(buffer[at]);
            if self.paste == Paste::Inside {
                self.pasted += 1;
            }
            at += 1;
        }
        Scanned {
            forward,
            detach: false,
        }
    }

    /// Whether bytes are held back, so the caller should wait a moment for
    /// the rest of a key and then flush.
    pub(crate) fn holding(&self) -> bool {
        !self.held.is_empty()
    }

    /// Gives up the bytes held back, except in a paste, where they may be the
    /// start of its end marker and are kept until the next read says.
    pub(crate) fn flush(&mut self) -> Vec<u8> {
        match self.paste {
            Paste::Outside => std::mem::take(&mut self.held),
            Paste::Inside => Vec::new(),
        }
    }

    pub(crate) fn pasting(&self) -> bool {
        self.paste == Paste::Inside
    }

    /// Takes a paste for over: what was held is returned as text.
    pub(crate) fn abandon_paste(&mut self) -> Vec<u8> {
        self.paste = Paste::Outside;
        self.pasted = 0;
        std::mem::take(&mut self.held)
    }
}

#[cfg(test)]
mod tests {
    use agent_core::keyspec::KeySpec;

    use super::*;

    fn key(spec: &str) -> DetachKey {
        let spec: KeySpec = spec.parse().expect("a key spec");
        DetachKey::new(spec.try_into().expect("supported"))
    }

    fn scanner(spec: &str) -> Scanner {
        Scanner::new(Some(key(spec)))
    }

    fn detaches(spec: &str, bytes: &[u8]) -> bool {
        scanner(spec).feed(bytes).detach
    }

    #[test]
    fn ctrl_backslash_matches_its_raw_byte_and_both_kitty_encodings() {
        assert!(detaches("ctrl-\\", b"\x1c"));
        assert!(detaches("ctrl-\\", b"\x1b[92;5u"));
        assert!(detaches("ctrl-\\", b"\x1b[92;5:1u"));
    }

    #[test]
    fn a_release_or_repeat_of_the_key_is_not_a_detach() {
        assert!(!detaches("ctrl-\\", b"\x1b[92;5:3u"));
        assert!(!detaches("ctrl-\\", b"\x1b[92;5:2u"));
    }

    #[test]
    fn other_keys_do_not_detach() {
        assert!(!detaches("ctrl-\\", b"\x1d"));
        assert!(!detaches("ctrl-\\", b"\x1b[93;5u"));
        assert!(!detaches("ctrl-\\", b"hello\x1b[A"));
        assert!(!detaches("ctrl-\\", b"\\"));
    }

    #[test]
    fn ctrl_letters_match_their_control_byte_and_csi_u() {
        assert!(detaches("ctrl-g", b"\x07"));
        assert!(detaches("ctrl-g", b"\x1b[103;5u"));
        assert!(detaches("ctrl-space", b"\x00"));
        assert!(detaches("ctrl-space", b"\x1b[32;5u"));
    }

    #[test]
    fn alt_keys_match_escape_then_the_key_and_csi_u() {
        assert!(detaches("alt-;", b"\x1b;"));
        assert!(detaches("alt-;", b"\x1b[59;3u"));
    }

    #[test]
    fn kitty_only_keys_match_only_csi_u() {
        assert!(detaches("ctrl-shift-d", b"\x1b[100;6u"));
        assert!(!detaches("ctrl-shift-d", b"\x04"));
        assert!(detaches("super-j", b"\x1b[106;9u"));
        assert!(detaches("ctrl-h", b"\x1b[104;5u"));
        assert!(!detaches("ctrl-h", b"\x08"));
    }

    #[test]
    fn input_before_the_key_is_forwarded_and_input_after_it_is_not() {
        let scanned = scanner("ctrl-\\").feed(b"abc\x1cdef");
        assert_eq!(scanned.forward, b"abc");
        assert!(scanned.detach);
    }

    #[test]
    fn a_key_split_across_reads_is_found() {
        let mut scanner = scanner("ctrl-\\");
        let first = scanner.feed(b"ab\x1b[9");
        assert_eq!(first.forward, b"ab");
        assert!(!first.detach);
        assert!(scanner.holding());
        let second = scanner.feed(b"2;5u");
        assert!(second.detach);
        assert_eq!(second.forward, Vec::<u8>::new());
    }

    #[test]
    fn held_bytes_that_turn_out_to_be_something_else_are_forwarded_with_the_rest() {
        let mut scanner = scanner("ctrl-\\");
        scanner.feed(b"\x1b[9");
        let scanned = scanner.feed(b"3;5u");
        assert_eq!(scanned.forward, b"\x1b[93;5u");
        assert!(!scanned.detach);
        assert!(!scanner.holding());
    }

    #[test]
    fn a_lone_escape_is_held_until_it_is_flushed() {
        let mut scanner = scanner("ctrl-\\");
        let scanned = scanner.feed(b"\x1b");
        assert_eq!(scanned.forward, Vec::<u8>::new());
        assert!(scanner.holding());
        assert_eq!(scanner.flush(), b"\x1b");
        assert!(!scanner.holding());
    }

    #[test]
    fn the_detach_key_inside_a_bracketed_paste_is_text_and_after_it_is_a_key_again() {
        let mut scanner = scanner("ctrl-\\");
        let pasted = scanner.feed(b"\x1b[200~one\x1ctwo\x1b[92;5uthree\x1b[201~");
        assert!(!pasted.detach);
        assert_eq!(
            pasted.forward,
            b"\x1b[200~one\x1ctwo\x1b[92;5uthree\x1b[201~"
        );
        assert!(scanner.feed(b"\x1c").detach);
    }

    #[test]
    fn a_paste_marker_split_across_reads_is_still_a_marker() {
        let mut scanner = scanner("ctrl-\\");
        assert!(!scanner.feed(b"\x1b[20").detach);
        assert!(!scanner.feed(b"0~a\x1c").detach);
        let end = scanner.feed(b"b\x1b[2");
        assert!(!end.detach);
        assert!(!scanner.feed(b"01~").detach);
        assert!(scanner.feed(b"\x1c").detach);
    }

    #[test]
    fn a_paste_that_never_ends_stops_hiding_the_detach_key_after_its_limit() {
        let mut scanner = scanner("ctrl-\\");
        scanner.feed(b"\x1b[200~");
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..=(PASTE_LIMIT / chunk.len()) {
            assert!(!scanner.feed(&chunk).detach);
        }
        assert!(scanner.feed(b"\x1c").detach);
    }

    #[test]
    fn a_paste_that_the_client_lets_go_of_returns_to_normal_and_gives_back_what_it_held() {
        let mut scanner = scanner("ctrl-\\");
        scanner.feed(b"\x1b[200~text\x1b[20");
        assert!(scanner.pasting());
        assert_eq!(scanner.abandon_paste(), b"\x1b[20");
        assert!(!scanner.pasting());
        assert!(scanner.feed(b"\x1c").detach);
    }

    #[test]
    fn a_held_end_marker_is_not_flushed_as_text_inside_a_paste() {
        let mut scanner = scanner("ctrl-\\");
        scanner.feed(b"\x1b[200~text\x1b[20");
        assert!(scanner.holding());
        assert_eq!(scanner.flush(), Vec::<u8>::new());
        let end = scanner.feed(b"1~");
        assert_eq!(end.forward, b"\x1b[201~");
        assert!(!scanner.pasting());
        assert!(scanner.feed(b"\x1c").detach);
    }

    #[test]
    fn a_detach_key_before_a_paste_in_one_read_still_detaches() {
        let scanned = scanner("ctrl-\\").feed(b"a\x1c\x1b[200~x");
        assert!(scanned.detach);
        assert_eq!(scanned.forward, b"a");
    }

    #[test]
    fn with_no_detach_key_everything_is_forwarded_untouched() {
        let mut scanner = Scanner::new(None);
        let scanned = scanner.feed(b"\x1c\x1b[92;5u\x1b");
        assert_eq!(scanned.forward, b"\x1c\x1b[92;5u\x1b");
        assert!(!scanned.detach);
        assert!(!scanner.holding());
    }
}
