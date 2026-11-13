//! What counts as a mark on the screen in text a harness sends, so a name
//! taken from it is never one nobody can see.

/// A run of characters that print nothing, or that only modify another one.
struct Unmarked {
    from: char,
    to: char,
    kind: Kind,
}

#[derive(PartialEq, Eq)]
enum Kind {
    /// Prints nothing and can make text read as something it isn't, so text
    /// shown as it is has them removed.
    Hidden,
    /// Prints nothing but is part of real text, as the braille blank is of a
    /// plot, so text shown as it is keeps it. It still makes no mark.
    Blank,
    /// Prints nothing of its own but joins what is around it, as in emoji
    /// sequences and Persian words.
    Joiner,
    /// Marks another character, as an accent does.
    Modifier,
}

const fn hidden(from: char, to: char) -> Unmarked {
    Unmarked {
        from,
        to,
        kind: Kind::Hidden,
    }
}

const fn blank(from: char, to: char) -> Unmarked {
    Unmarked {
        from,
        to,
        kind: Kind::Blank,
    }
}

const fn joiner(from: char, to: char) -> Unmarked {
    Unmarked {
        from,
        to,
        kind: Kind::Joiner,
    }
}

const fn modifier(from: char, to: char) -> Unmarked {
    Unmarked {
        from,
        to,
        kind: Kind::Modifier,
    }
}

/// Formatting characters, fillers and combining marks. Whitespace and
/// control characters are left to `char`. Combining marks are listed by
/// block, so one that is alone in a line leaves nothing in it.
const UNMARKED: &[Unmarked] = &[
    hidden('\u{AD}', '\u{AD}'),         // soft hyphen
    modifier('\u{300}', '\u{36F}'),     // combining marks
    modifier('\u{600}', '\u{605}'),     // arabic number signs
    hidden('\u{61C}', '\u{61C}'),       // arabic letter mark
    modifier('\u{6DD}', '\u{6DD}'),     // arabic end of ayah
    modifier('\u{70F}', '\u{70F}'),     // syriac abbreviation mark
    modifier('\u{890}', '\u{891}'),     // arabic pound signs
    modifier('\u{8E2}', '\u{8E2}'),     // arabic disputed end of ayah
    hidden('\u{115F}', '\u{1160}'),     // hangul fillers
    blank('\u{17B4}', '\u{17B5}'),      // khmer inherent vowels
    blank('\u{180B}', '\u{180D}'),      // mongolian free variation selectors
    hidden('\u{180E}', '\u{180F}'),     // mongolian vowel separator and selector
    modifier('\u{1AB0}', '\u{1AFF}'),   // combining marks extended
    modifier('\u{1DC0}', '\u{1DFF}'),   // combining marks supplement
    hidden('\u{200B}', '\u{200B}'),     // zero width space
    joiner('\u{200C}', '\u{200D}'),     // zero width non-joiner and joiner
    hidden('\u{200E}', '\u{200F}'),     // direction marks
    hidden('\u{202A}', '\u{202E}'),     // bidi embeddings
    hidden('\u{2060}', '\u{2064}'),     // word joiner and invisible operators
    hidden('\u{2066}', '\u{206F}'),     // bidi isolates and deprecated format
    modifier('\u{20D0}', '\u{20FF}'),   // combining marks for symbols
    blank('\u{2800}', '\u{2800}'),      // braille blank
    hidden('\u{3164}', '\u{3164}'),     // hangul filler
    modifier('\u{FE00}', '\u{FE0F}'),   // variation selectors
    modifier('\u{FE20}', '\u{FE2F}'),   // combining half marks
    hidden('\u{FEFF}', '\u{FEFF}'),     // byte order mark
    hidden('\u{FFA0}', '\u{FFA0}'),     // halfwidth hangul filler
    hidden('\u{FFF9}', '\u{FFFB}'),     // interlinear annotation
    hidden('\u{E0000}', '\u{E007F}'),   // tag characters
    modifier('\u{E0100}', '\u{E01EF}'), // variation selectors supplement
];

/// Whether `c` is one of the characters that print nothing and can disguise
/// text, which text shown as it is drops. Joiners and marks on a letter are
/// not.
pub fn is_hidden(c: char) -> bool {
    UNMARKED
        .iter()
        .any(|unmarked| unmarked.kind == Kind::Hidden && (unmarked.from..=unmarked.to).contains(&c))
}

/// Whether `c` leaves a mark of its own.
pub fn is_visible(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !UNMARKED
            .iter()
            .any(|unmarked| (unmarked.from..=unmarked.to).contains(&c))
}

/// Whether anything in `text` leaves a mark.
pub fn has_visible(text: &str) -> bool {
    text.chars().any(is_visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unmarked_run_is_invisible_at_both_ends_and_in_the_middle() {
        for unmarked in UNMARKED {
            let middle = char::from_u32(u32::midpoint(unmarked.from as u32, unmarked.to as u32))
                .unwrap_or(unmarked.from);
            for c in [unmarked.from, middle, unmarked.to] {
                assert!(
                    !is_visible(c),
                    "{c:?} in {:?}..={:?}",
                    unmarked.from,
                    unmarked.to
                );
            }
        }
    }

    #[test]
    fn the_characters_either_side_of_each_run_are_left_to_their_own_class() {
        let neighbour = |c: char, next: bool| {
            let n = if next { c as u32 + 1 } else { c as u32 - 1 };
            char::from_u32(n)
        };
        for unmarked in UNMARKED {
            let inside = |c: char| UNMARKED.iter().any(|u| (u.from..=u.to).contains(&c));
            let before = neighbour(unmarked.from, false).filter(|c| !inside(*c));
            let after = neighbour(unmarked.to, true).filter(|c| !inside(*c));
            for c in before.into_iter().chain(after) {
                let plain = !c.is_whitespace() && !c.is_control();
                assert_eq!(
                    is_visible(c),
                    plain,
                    "{c:?} next to {:?}..={:?}",
                    unmarked.from,
                    unmarked.to
                );
            }
        }
    }

    #[test]
    fn the_runs_are_sorted_and_do_not_overlap() {
        for pair in UNMARKED.windows(2) {
            assert!(pair[0].from <= pair[0].to, "{:?}", pair[0].from);
            assert!(
                pair[0].to < pair[1].from,
                "{:?}..={:?} then {:?}",
                pair[0].from,
                pair[0].to,
                pair[1].from
            );
        }
    }

    #[test]
    fn blanks_are_not_hidden_but_leave_no_mark() {
        for blank in ['\u{2800}', '\u{17B4}', '\u{17B5}', '\u{180B}', '\u{180D}'] {
            assert!(!is_hidden(blank) && !is_visible(blank), "{blank:?}");
        }
        assert!(!has_visible("\u{2800}\u{2800}"));
    }

    #[test]
    fn only_the_hidden_runs_are_hidden_and_the_joiners_and_marks_are_not() {
        for hidden in [
            '\u{115F}',
            '\u{FFA0}',
            '\u{206A}',
            '\u{180E}',
            '\u{180F}',
            '\u{AD}',
            '\u{E0041}',
        ] {
            assert!(is_hidden(hidden) && !is_visible(hidden), "{hidden:?}");
        }
        for kept in ['\u{200C}', '\u{200D}', '\u{301}', '\u{FE0F}', 'a'] {
            assert!(!is_hidden(kept), "{kept:?}");
        }
        assert!(!is_visible('\u{200D}') && !is_visible('\u{301}'));
    }

    #[test]
    fn whitespace_of_every_kind_and_control_characters_are_invisible() {
        for c in [
            ' ', '\t', '\n', '\u{A0}', '\u{1680}', '\u{2003}', '\u{2028}', '\u{2029}', '\u{202F}',
            '\u{205F}', '\u{3000}', '\u{0}', '\u{1b}', '\u{7f}', '\u{9b}',
        ] {
            assert!(!is_visible(c), "{c:?}");
        }
    }

    #[test]
    fn ordinary_text_is_visible_and_so_is_a_letter_with_a_mark_on_it() {
        assert!(has_visible("ls"));
        assert!(has_visible("e\u{301}"));
        assert!(has_visible("\u{200b}ls"));
        assert!(!has_visible("\u{301}"));
        assert!(!has_visible(" \u{2800}\u{fe0f}\u{e0041}"));
    }
}
