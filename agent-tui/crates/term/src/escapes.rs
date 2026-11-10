use std::str::Chars;

/// What an escape sequence did, once consumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sequence {
    /// It moved the cursor, so what follows is on another line.
    Moves,
    Other,
}

const CSI: char = '\u{9b}';
const STRING_STARTS: [char; 5] = ['\u{9d}', '\u{90}', '\u{98}', '\u{9e}', '\u{9f}'];
const ST: char = '\u{9c}';

/// Consumes the escape sequence `lead` starts from `chars`: CSI, a string
/// (OSC, DCS, SOS, PM, APC) up to a bell or a string terminator, or a short
/// escape such as `ESC 7` or the charset designation `ESC ( B`. Both the
/// 7-bit `ESC` forms and the 8-bit C1 characters start one. `None` when
/// `lead` starts none, which leaves a lone `ESC` to the caller.
pub fn consume(lead: char, chars: &mut Chars<'_>) -> Option<Sequence> {
    match lead {
        '\u{1b}' => match chars.clone().next()? {
            '[' => {
                chars.next();
                Some(csi(chars))
            }
            ']' | 'P' | 'X' | '^' | '_' => {
                chars.next();
                skip_string(chars);
                Some(Sequence::Other)
            }
            ' '..='/' => {
                while take_if(chars, '\u{20}'..='\u{2f}') {}
                take_if(chars, '\u{30}'..='\u{7e}');
                Some(Sequence::Other)
            }
            '\u{30}'..='\u{7e}' => {
                chars.next();
                Some(Sequence::Other)
            }
            _ => None,
        },
        CSI => Some(csi(chars)),
        c if STRING_STARTS.contains(&c) => {
            skip_string(chars);
            Some(Sequence::Other)
        }
        _ => None,
    }
}

/// `text` without terminal escape sequences. A lone `ESC` stays, for the
/// caller to treat as the control character it is.
pub fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if consume(c, &mut chars).is_none() {
            out.push(c);
        }
    }
    out
}

/// What a printable text does with line breaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    /// Newlines stay.
    Keep,
    /// Every kind of line break becomes a space.
    Flatten,
}

/// `text` as printable characters only: no escape sequences, no control
/// characters, tabs as spaces.
pub fn printable(text: &str, lines: Lines) -> String {
    strip_escapes(text)
        .replace("\r\n", "\n")
        .chars()
        .filter_map(|c| match (c, lines) {
            ('\n' | '\u{85}' | '\u{2028}', Lines::Keep) => Some('\n'),
            ('\t', _) | ('\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}', Lines::Flatten) => {
                Some(' ')
            }
            (c, _) if c.is_control() => None,
            (c, _) => Some(c),
        })
        .collect()
}

fn take_if(chars: &mut Chars<'_>, range: std::ops::RangeInclusive<char>) -> bool {
    let next = chars.clone().next().is_some_and(|c| range.contains(&c));
    if next {
        chars.next();
    }
    next
}

/// A sequence ends at its final byte, or at the end of the line when it
/// has none, which leaves the newline for the caller.
fn csi(chars: &mut Chars<'_>) -> Sequence {
    while let Some(c) = chars.clone().next() {
        if c == '\n' {
            return Sequence::Other;
        }
        chars.next();
        if ('@'..='~').contains(&c) {
            return if "HfdABEFJ".contains(c) {
                Sequence::Moves
            } else {
                Sequence::Other
            };
        }
    }
    Sequence::Other
}

/// A string ends at a bell, a string terminator or the end of the line. A
/// C1 start can be a stray character in text that was never a terminal
/// stream, so an unterminated string must not swallow what follows its line.
fn skip_string(chars: &mut Chars<'_>) {
    while let Some(c) = chars.clone().next() {
        if c == '\n' {
            return;
        }
        chars.next();
        let escaped_end = c == '\u{1b}' && chars.clone().next() == Some('\\');
        if c == '\u{7}' || c == ST || escaped_end {
            if escaped_end {
                chars.next();
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Lines, printable, strip_escapes};

    #[test]
    fn printable_text_keeps_or_flattens_its_lines() {
        let text = "a\x1b[1m\tb\r\nc\u{2028}d\u{85}e\x07f";
        assert_eq!(printable(text, Lines::Keep), "a b\nc\nd\nef");
        assert_eq!(printable(text, Lines::Flatten), "a b c d ef");
    }

    #[test]
    fn an_unterminated_csi_ends_with_its_line() {
        assert_eq!(strip_escapes("a\x1b[31\nb"), "a\nb");
        assert_eq!(strip_escapes("a\u{9b}31\nb"), "a\nb");
        assert_eq!(strip_escapes("a\x1b[31"), "a");
    }

    #[test]
    fn csi_is_stripped_up_to_its_final_byte() {
        assert_eq!(strip_escapes("a\x1b[31;1mb\x1b[0mc\x1b[2Jd"), "abcd");
    }

    #[test]
    fn strings_end_at_a_bell_or_a_string_terminator() {
        for start in [']', 'P', 'X', '^', '_'] {
            assert_eq!(
                strip_escapes(&format!("a\x1b{start}x;y\x07b")),
                "ab",
                "{start}"
            );
            assert_eq!(
                strip_escapes(&format!("a\x1b{start}x;y\x1b\\b")),
                "ab",
                "{start}"
            );
        }
        assert_eq!(strip_escapes("a\x1b]0;t\u{9c}b"), "ab");
    }

    #[test]
    fn short_escapes_and_charset_designations_are_stripped() {
        assert_eq!(strip_escapes("a\x1b(Bb\x1b)0c\x1b7d\x1b=e"), "abcde");
    }

    #[test]
    fn eight_bit_csi_and_strings_are_stripped() {
        assert_eq!(
            strip_escapes("a\u{9b}31mb\u{9d}0;t\x07c\u{90}q\u{9c}d"),
            "abcd"
        );
    }

    #[test]
    fn a_lone_escape_is_left_for_the_caller() {
        assert_eq!(strip_escapes("a\x1b"), "a\x1b");
        assert_eq!(strip_escapes("a\x1b\x01b"), "a\x1b\x01b");
    }

    #[test]
    fn an_unterminated_string_ends_with_its_line() {
        assert_eq!(strip_escapes("a\x1b]0;title"), "a");
        assert_eq!(strip_escapes("a\x1b]0;title\nb"), "a\nb");
        assert_eq!(strip_escapes("a\u{9d}b\nc\u{90}d\ne"), "a\nc\ne");
    }
}
