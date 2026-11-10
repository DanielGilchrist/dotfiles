//! Multi-line text with a caret, and how it wraps into rows of a box.

use std::num::NonZeroUsize;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// How many terminal columns a row of text may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Columns(NonZeroUsize);

impl Columns {
    pub const ONE: Self = Self(NonZeroUsize::MIN);

    pub fn new(columns: usize) -> Option<Self> {
        NonZeroUsize::new(columns).map(Self)
    }

    pub fn get(self) -> usize {
        self.0.get()
    }
}

/// The room a message has: its row width and how many rows show at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Room {
    pub columns: Columns,
    pub rows: NonZeroUsize,
}

fn width(text: &str) -> usize {
    text.graphemes(true).map(UnicodeWidthStr::width).sum()
}

fn is_blank_cluster(cluster: &str) -> bool {
    cluster.chars().all(char::is_whitespace)
}

/// The first row to show so the caret's row is visible, moving `top` only
/// when it must.
pub fn scrolled(top: usize, caret_row: usize, rows: usize, height: usize) -> usize {
    let top = top.min(rows.saturating_sub(height));
    if caret_row < top {
        caret_row
    } else if caret_row >= top + height {
        caret_row + 1 - height
    } else {
        top
    }
}

/// Text being written, with the caret between two of its grapheme clusters.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    text: String,
    /// Always on a cluster boundary of `text`.
    caret: usize,
    top: usize,
}

impl From<&str> for Message {
    fn from(text: &str) -> Self {
        let mut message = Self::default();
        message.insert_str(text);
        message
    }
}

#[derive(Clone, Copy)]
enum Step {
    Up,
    Down,
}

impl Message {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The first row shown, as last moved by [`Message::reveal`].
    pub fn top(&self) -> usize {
        self.top
    }

    /// Scrolls the least that shows the caret's row in `room`.
    pub fn reveal(&mut self, room: Room) {
        let wrapped = Wrapped::of(&self.text, room.columns);
        self.top = scrolled(
            self.top,
            wrapped.row_of(self.caret),
            wrapped.rows().len(),
            room.rows.get(),
        );
    }

    pub fn insert(&mut self, c: char) {
        self.insert_str(c.encode_utf8(&mut [0; 4]));
    }

    /// Tabs become spaces, so every character has one width.
    pub fn insert_str(&mut self, text: &str) {
        let text = text.replace('\t', "    ");
        self.text.insert_str(self.caret, &text);
        self.caret += text.len();
        self.caret = self.boundary_from(self.caret);
    }

    pub fn backspace(&mut self) {
        let start = self.before(self.caret);
        self.text.replace_range(start..self.caret, "");
        self.caret = self.boundary_from(start);
    }

    pub fn delete(&mut self) {
        let end = self.after(self.caret);
        self.text.replace_range(self.caret..end, "");
        self.caret = self.boundary_from(self.caret);
    }

    /// Deletes the blanks before the caret and the word before them.
    pub fn delete_word(&mut self) {
        let mut start = self.caret;
        for blank in [true, false] {
            while start > 0 {
                let before = self.before(start);
                if is_blank_cluster(&self.text[before..start]) != blank {
                    break;
                }
                start = before;
            }
        }
        self.text.replace_range(start..self.caret, "");
        self.caret = self.boundary_from(start);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn left(&mut self) {
        self.caret = self.before(self.caret);
    }

    pub fn right(&mut self) {
        self.caret = self.after(self.caret);
    }

    pub fn home(&mut self) {
        self.caret = self.text[..self.caret].rfind('\n').map_or(0, |i| i + 1);
    }

    pub fn end(&mut self) {
        self.caret = self.text[self.caret..]
            .find('\n')
            .map_or(self.text.len(), |i| self.caret + i);
    }

    /// Up from the first row goes to the start.
    pub fn up(&mut self, columns: Columns) {
        self.step_row(columns, Step::Up);
    }

    /// Down from the last row goes to the end.
    pub fn down(&mut self, columns: Columns) {
        self.step_row(columns, Step::Down);
    }

    fn before(&self, at: usize) -> usize {
        self.text[..at]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn after(&self, at: usize) -> usize {
        self.text[at..]
            .graphemes(true)
            .next()
            .map_or(at, |g| at + g.len())
    }

    /// The first cluster boundary at or after `at`: an insert can merge
    /// with the text after it.
    fn boundary_from(&self, at: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([self.text.len()])
            .find(|i| *i >= at)
            .unwrap_or(self.text.len())
    }

    fn step_row(&mut self, columns: Columns, step: Step) {
        let wrapped = Wrapped::of(&self.text, columns);
        let here = wrapped.row_of(self.caret);
        let target = match step {
            Step::Up => here.checked_sub(1),
            Step::Down => Some(here + 1).filter(|row| *row < wrapped.rows().len()),
        };
        let Some(target) = target else {
            self.caret = match step {
                Step::Up => 0,
                Step::Down => self.text.len(),
            };
            return;
        };
        let column = width(&self.text[wrapped.rows()[here].start..self.caret]);
        let row = wrapped.rows()[target].clone();
        let mut used = 0;
        let mut at = row.start;
        for (offset, cluster) in self.text[row.clone()].grapheme_indices(true) {
            used += UnicodeWidthStr::width(cluster);
            if used > column {
                break;
            }
            at = row.start + offset + cluster.len();
        }
        if continues(&self.text, &row) {
            at = at.min(self.before(row.end));
        }
        self.caret = at;
    }
}

/// The rows `text` wraps into: byte ranges that never include a newline.
/// Lines break after the last space that fits, or inside a word longer than
/// the row. A space that ends a row stays in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapped {
    rows: Vec<Range<usize>>,
}

impl Wrapped {
    pub fn of(text: &str, columns: Columns) -> Self {
        let mut rows = Vec::new();
        let mut line_start = 0;
        for line in text.split('\n') {
            let line_end = line_start + line.len();
            wrap_line(text, line_start..line_end, columns, &mut rows);
            line_start = line_end + 1;
        }
        Self { rows }
    }

    pub fn rows(&self) -> &[Range<usize>] {
        &self.rows
    }

    /// The row a caret at byte `caret` is on. At the start of a wrapped row
    /// it is the later one.
    pub fn row_of(&self, caret: usize) -> usize {
        self.rows
            .iter()
            .rposition(|row| row.start <= caret)
            .unwrap_or(0)
    }
}

/// Whether `row` ends where the same line goes on in the next row, so a
/// caret at its end is shown at the start of that one.
fn continues(text: &str, row: &Range<usize>) -> bool {
    row.end < text.len() && !text[row.end..].starts_with('\n')
}

fn wrap_line(text: &str, line: Range<usize>, columns: Columns, rows: &mut Vec<Range<usize>>) {
    let mut start = line.start;
    loop {
        let fits = fitting_end(text, start..line.end, columns);
        if fits == line.end {
            rows.push(start..line.end);
            return;
        }
        let end = if text[fits..].graphemes(true).next() == Some(" ") {
            fits + 1
        } else if let Some((space, _)) = text[start..fits]
            .grapheme_indices(true)
            .rev()
            .find(|(_, g)| *g == " ")
        {
            start + space + 1
        } else {
            fits
        };
        rows.push(start..end);
        start = end;
    }
}

/// Where the longest prefix of `span` that fits `columns` ends, taking at
/// least one cluster so a wide one in a narrow row still advances.
fn fitting_end(text: &str, span: Range<usize>, columns: Columns) -> usize {
    let mut used = 0;
    let mut end = span.start;
    for cluster in text[span.clone()].graphemes(true) {
        used += UnicodeWidthStr::width(cluster);
        if used > columns.get() && end > span.start {
            break;
        }
        end += cluster.len();
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(n: usize) -> Columns {
        Columns::new(n).expect("a column")
    }

    fn wrapped(text: &str, n: usize) -> Vec<&str> {
        Wrapped::of(text, columns(n))
            .rows()
            .iter()
            .map(|row| &text[row.clone()])
            .collect()
    }

    const FAMILY: &str = "👨\u{200d}👩\u{200d}👧";
    const HEART: &str = "❤\u{fe0f}";

    #[test]
    fn text_that_fits_stays_on_one_row_and_empty_text_has_one_empty_row() {
        assert_eq!(wrapped("hello", 5), ["hello"]);
        assert_eq!(wrapped("", 5), [""]);
    }

    #[test]
    fn rows_break_after_the_last_space_that_fits() {
        assert_eq!(wrapped("one two three", 8), ["one two ", "three"]);
        assert_eq!(wrapped("one two three", 5), ["one ", "two ", "three"]);
    }

    #[test]
    fn a_word_longer_than_the_row_is_broken_inside_it() {
        assert_eq!(wrapped("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrapped("a abcdefgh", 4), ["a ", "abcd", "efgh"]);
    }

    #[test]
    fn explicit_newlines_always_start_a_row_and_a_trailing_one_leaves_an_empty_row() {
        assert_eq!(wrapped("a\n\nb", 9), ["a", "", "b"]);
        assert_eq!(wrapped("a\n", 9), ["a", ""]);
    }

    #[test]
    fn wide_characters_count_two_columns_and_never_split() {
        assert_eq!(wrapped("日本語日本語", 4), ["日本", "語日", "本語"]);
        assert_eq!(wrapped("日本語", 5), ["日本", "語"]);
        assert_eq!(wrapped("日", 1), ["日"]);
    }

    #[test]
    fn a_cluster_is_one_cell_group_wide_and_never_split_across_rows() {
        assert_eq!(width(FAMILY), 2);
        assert_eq!(width(HEART), 2);
        assert_eq!(width("e\u{301}"), 1);
        let text = format!("a{FAMILY}{HEART}e\u{301}b");
        assert_eq!(
            wrapped(&text, 3),
            [
                format!("a{FAMILY}"),
                format!("{HEART}e\u{301}"),
                "b".to_string()
            ]
        );
    }

    #[test]
    fn rows_cover_the_text_exactly_at_every_width() {
        let text =
            format!("the quick  brown fox\njumps over   a lazy dog 日本語 {FAMILY} end\n\nx");
        for n in 1..=24 {
            let rows = Wrapped::of(&text, columns(n));
            let joined: String = rows.rows().iter().map(|r| &text[r.clone()]).collect();
            assert_eq!(joined, text.replace('\n', ""), "at {n}");
        }
    }

    #[test]
    fn the_caret_at_a_wrapped_rows_start_belongs_to_the_later_row() {
        let w = Wrapped::of("one two three", columns(5));
        assert_eq!(w.row_of(0), 0);
        assert_eq!(w.row_of(3), 0);
        assert_eq!(w.row_of(4), 1);
        assert_eq!(w.row_of(8), 2);
        assert_eq!(w.row_of(13), 2);
    }

    #[test]
    fn editing_moves_the_caret_by_characters_not_bytes() {
        let mut t = Message::from("aé日");
        t.left();
        t.insert('x');
        assert_eq!(t.as_str(), "aéx日");
        t.backspace();
        t.backspace();
        assert_eq!(t.as_str(), "a日");
        t.right();
        t.right();
        t.insert_str("!\n");
        assert_eq!(t.as_str(), "a日!\n");
        t.backspace();
        t.backspace();
        assert_eq!(t.as_str(), "a日");
    }

    #[test]
    fn the_caret_steps_over_a_whole_cluster_and_never_sits_inside_one() {
        let mut t = Message::from(format!("a{FAMILY}{HEART}e\u{301}").as_str());
        let (family, heart) = (FAMILY.len(), HEART.len());
        for at in [1 + family + heart, 1 + family, 1, 0] {
            t.left();
            assert_eq!(t.caret(), at);
        }
        let mut t = Message::from(format!("{FAMILY}b").as_str());
        t.left();
        t.left();
        assert_eq!(t.caret(), 0);
        t.right();
        assert_eq!(t.caret(), FAMILY.len());
        t.left();
        t.delete();
        assert_eq!(t.as_str(), "b");
        let mut t = Message::from(format!("a{HEART}").as_str());
        t.backspace();
        assert_eq!(t.as_str(), "a");
    }

    #[test]
    fn a_character_typed_before_a_combining_mark_leaves_the_caret_after_the_merged_cluster() {
        let mut t = Message::from("\u{301}");
        t.left();
        t.insert('e');
        assert_eq!(t.caret(), t.as_str().len());
        t.backspace();
        assert_eq!(t.as_str(), "");
    }

    #[test]
    fn deleting_between_two_regional_indicators_leaves_the_caret_on_a_boundary() {
        let flag = "\u{1f1fa}x\u{1f1f8}";
        let mut t = Message::from(flag);
        t.left();
        t.backspace();
        assert_eq!(t.as_str(), "\u{1f1fa}\u{1f1f8}");
        assert_eq!(t.caret(), t.as_str().len());
        let mut t = Message::from(flag);
        t.left();
        t.left();
        t.delete();
        assert_eq!(t.caret(), t.as_str().len());
        let mut t = Message::from(flag);
        t.left();
        t.delete_word();
        assert_eq!((t.as_str(), t.caret()), ("\u{1f1f8}", 0));
    }

    #[test]
    fn tabs_are_four_spaces_so_every_character_has_a_width() {
        let t = Message::from("a\tb");
        assert_eq!(t.as_str(), "a    b");
    }

    #[test]
    fn delete_removes_after_the_caret_and_home_and_end_go_to_the_ends_of_the_line() {
        let mut t = Message::from("ab\ncd");
        t.left();
        t.delete();
        assert_eq!(t.as_str(), "ab\nc");
        t.delete();
        t.home();
        assert_eq!(t.caret(), 3);
        t.end();
        assert_eq!(t.caret(), 4);
        t.home();
        t.left();
        t.end();
        assert_eq!(t.caret(), 2);
        t.home();
        assert_eq!(t.caret(), 0);
    }

    #[test]
    fn delete_word_takes_the_blanks_and_the_word_before_the_caret() {
        let mut t = Message::from("one two  ");
        t.delete_word();
        assert_eq!(t.as_str(), "one ");
        t.delete_word();
        assert_eq!(t.as_str(), "");
        let mut t = Message::from("a\nbc");
        t.delete_word();
        t.delete_word();
        assert_eq!(t.as_str(), "");
    }

    #[test]
    fn up_and_down_keep_the_column_across_wrapped_rows_and_the_ends_go_to_the_text_ends() {
        let mut t = Message::from("abcdefghij");
        t.up(columns(4));
        assert_eq!(t.caret(), 6);
        t.up(columns(4));
        t.up(columns(4));
        assert_eq!(t.caret(), 0);
        t.right();
        t.right();
        t.down(columns(4));
        assert_eq!(t.caret(), 6);
        t.down(columns(4));
        t.down(columns(4));
        assert_eq!(t.caret(), 10);
    }

    #[test]
    fn up_from_a_longer_row_lands_at_the_end_of_a_shorter_one() {
        let mut t = Message::from("ab\ncdefg");
        t.up(columns(20));
        assert_eq!(t.caret(), 2);
    }

    #[test]
    fn up_onto_a_soft_wrapped_row_that_ends_in_a_space_stays_on_that_row() {
        let mut t = Message::from("one two three");
        t.up(columns(5));
        assert_eq!(t.caret(), 7);
        let wrapped = Wrapped::of(t.as_str(), columns(5));
        assert_eq!(wrapped.row_of(t.caret()), 1);
        t.down(columns(5));
        assert_eq!(t.caret(), 11);
    }

    #[test]
    fn the_viewport_moves_only_when_the_caret_leaves_it() {
        assert_eq!(scrolled(0, 3, 10, 4), 0);
        assert_eq!(scrolled(0, 4, 10, 4), 1);
        assert_eq!(scrolled(1, 2, 10, 4), 1);
        assert_eq!(scrolled(1, 0, 10, 4), 0);
        assert_eq!(scrolled(9, 9, 10, 4), 6);
        assert_eq!(scrolled(5, 2, 3, 4), 0);
    }

    #[test]
    fn reveal_scrolls_a_message_to_its_caret_and_back_up_one_row_at_a_time() {
        let room = Room {
            columns: columns(10),
            rows: NonZeroUsize::new(3).expect("rows"),
        };
        let mut t = Message::from("1\n2\n3\n4\n5\n6");
        t.reveal(room);
        assert_eq!(t.top(), 3);
        t.up(room.columns);
        t.reveal(room);
        assert_eq!(t.top(), 3, "still in view");
        t.up(room.columns);
        t.up(room.columns);
        t.reveal(room);
        assert_eq!(t.top(), 2, "scrolls at the edge");
    }
}
