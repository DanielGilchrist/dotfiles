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
            (c, _) if c.is_control() || agent_core::visible::is_hidden(c) => None,
            (c, _) => Some(c),
        })
        .collect()
}

/// What a program's output shows as printable lines, for output that was
/// never a screen: escape sequences are removed first, so one can't hide a
/// carriage return. A carriage return rewinds its line and only the last
/// visible text after one shows, so `50%\r100%` reads `100%` and a rewind
/// that ends in an erase (`done\r\x1b[K`) keeps `done`. A backspace deletes
/// the character before it, so `a\x08b` reads `b`. Everything else is as
/// [`printable`] says.
pub fn output_lines(text: &str) -> Vec<String> {
    let stripped = strip_escapes(text);
    stripped
        .strip_suffix('\n')
        .unwrap_or(&stripped)
        .split('\n')
        .map(|line| {
            let shown = line
                .rsplit('\r')
                .map(overwritten)
                .find(|segment| !segment.trim().is_empty())
                .unwrap_or_default();
            printable(&shown, Lines::Flatten)
        })
        .collect()
}

/// `segment` with each backspace removing the character before it.
fn overwritten(segment: &str) -> String {
    let mut kept = String::with_capacity(segment.len());
    for c in segment.chars() {
        if c == '\u{8}' {
            kept.pop();
        } else {
            kept.push(c);
        }
    }
    kept
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
    use super::{Lines, output_lines, printable, strip_escapes};

    #[test]
    fn printable_text_keeps_or_flattens_its_lines() {
        let text = "a\x1b[1m\tb\r\nc\u{2028}d\u{85}e\x07f";
        assert_eq!(printable(text, Lines::Keep), "a b\nc\nd\nef");
        assert_eq!(printable(text, Lines::Flatten), "a b c d ef");
    }

    #[test]
    fn bidi_and_other_format_characters_are_dropped() {
        for dropped in [
            '\u{202a}',
            '\u{202b}',
            '\u{202c}',
            '\u{202d}',
            '\u{202e}',
            '\u{2066}',
            '\u{2067}',
            '\u{2068}',
            '\u{2069}',
            '\u{61c}',
            '\u{200b}',
            '\u{200e}',
            '\u{200f}',
            '\u{feff}',
            '\u{2060}',
            '\u{2064}',
            '\u{e0000}',
            '\u{e0041}',
            '\u{e007f}',
            '\u{ad}',
            '\u{180e}',
            '\u{fff9}',
            '\u{fffb}',
            '\u{3164}',
        ] {
            assert_eq!(
                printable(&format!("a{dropped}b"), Lines::Keep),
                "ab",
                "{dropped:?}"
            );
            assert!(!agent_core::visible::is_visible(dropped), "{dropped:?}");
        }
    }

    #[test]
    fn blanks_that_are_part_of_real_text_stay_though_they_leave_no_mark() {
        let plot = "\u{2800}\u{28ff}\u{2800}\u{17b4}\u{180b}";
        assert_eq!(printable(plot, Lines::Keep), plot);
        assert_eq!(output_lines(plot), [plot]);
        assert!(!agent_core::visible::has_visible("\u{2800}\u{2800}"));
    }

    #[test]
    fn the_joiners_that_emoji_and_persian_need_stay() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        assert_eq!(printable(family, Lines::Keep), family);
        let persian = "\u{645}\u{6cc}\u{200c}\u{62e}\u{648}\u{627}\u{647}\u{645}";
        assert_eq!(printable(persian, Lines::Keep), persian);
    }

    #[test]
    fn output_lines_keep_what_a_carriage_return_leaves_visible() {
        assert_eq!(output_lines("10%\r50%\r100%\nnext\n"), ["100%", "next"]);
        assert_eq!(output_lines("progress 50%\r\x1b[K\n"), ["progress 50%"]);
        assert_eq!(output_lines("a\r\nb\r\n"), ["a", "b"]);
    }

    #[test]
    fn output_lines_apply_a_backspace_to_the_character_before_it() {
        assert_eq!(output_lines("a\x08b"), ["b"]);
        assert_eq!(output_lines("\x08x"), ["x"]);
    }

    #[test]
    fn output_lines_strip_strings_c1_starts_and_lone_escapes() {
        let text = "a\x1bPq;data\x1b\\b\u{9d}0;t\x07c\u{90}x\u{9c}d\x1b";
        assert_eq!(output_lines(text), ["abcd"]);
    }

    #[test]
    fn output_lines_keep_blank_lines_and_drop_the_last_line_break() {
        assert_eq!(output_lines("one\n\ntwo\n"), ["one", "", "two"]);
        assert_eq!(output_lines(""), [""]);
    }

    #[test]
    fn output_lines_drop_bidi_overrides() {
        assert_eq!(output_lines("evil\u{202e}txt"), ["eviltxt"]);
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
