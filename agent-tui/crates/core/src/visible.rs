//! What counts as a mark on the screen in text a harness sends, so a name
//! taken from it is never one nobody can see.

/// A run of characters that print nothing, or that only modify another one.
struct Unmarked {
    from: char,
    to: char,
}

const fn run(from: char, to: char) -> Unmarked {
    Unmarked { from, to }
}

/// Formatting characters, fillers and combining marks. Whitespace and
/// control characters are left to `char`. Combining marks are listed by
/// block, so one that is alone in a line leaves nothing in it.
const UNMARKED: &[Unmarked] = &[
    run('\u{AD}', '\u{AD}'),       // soft hyphen
    run('\u{300}', '\u{36F}'),     // combining marks
    run('\u{600}', '\u{605}'),     // arabic number signs
    run('\u{61C}', '\u{61C}'),     // arabic letter mark
    run('\u{6DD}', '\u{6DD}'),     // arabic end of ayah
    run('\u{70F}', '\u{70F}'),     // syriac abbreviation mark
    run('\u{890}', '\u{891}'),     // arabic pound signs
    run('\u{8E2}', '\u{8E2}'),     // arabic disputed end of ayah
    run('\u{115F}', '\u{1160}'),   // hangul fillers
    run('\u{17B4}', '\u{17B5}'),   // khmer inherent vowels
    run('\u{180B}', '\u{180F}'),   // mongolian selectors
    run('\u{1AB0}', '\u{1AFF}'),   // combining marks extended
    run('\u{1DC0}', '\u{1DFF}'),   // combining marks supplement
    run('\u{200B}', '\u{200F}'),   // zero width and direction marks
    run('\u{202A}', '\u{202E}'),   // bidi embeddings
    run('\u{2060}', '\u{2064}'),   // word joiner and invisible operators
    run('\u{2066}', '\u{206F}'),   // bidi isolates and deprecated format
    run('\u{20D0}', '\u{20FF}'),   // combining marks for symbols
    run('\u{2800}', '\u{2800}'),   // braille blank
    run('\u{3164}', '\u{3164}'),   // hangul filler
    run('\u{FE00}', '\u{FE0F}'),   // variation selectors
    run('\u{FE20}', '\u{FE2F}'),   // combining half marks
    run('\u{FEFF}', '\u{FEFF}'),   // byte order mark
    run('\u{FFA0}', '\u{FFA0}'),   // halfwidth hangul filler
    run('\u{FFF9}', '\u{FFFB}'),   // interlinear annotation
    run('\u{E0000}', '\u{E007F}'), // tag characters
    run('\u{E0100}', '\u{E01EF}'), // variation selectors supplement
];

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
