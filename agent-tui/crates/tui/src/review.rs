//! Reading a diff line by line: what each shown line is in its file, the
//! cursor over them, and the comments a reviewer drafts for an agent.

use agent_core::names::{CarriageReturn, clean};
use agent_core::ports::FileState;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// A hunk header or a `\ No newline` note: shown, never selected.
    Header,
    /// A commit in the branch view; its `text` is the log line.
    Commit,
    /// An uncommitted file in the branch view; its `text` is its status code.
    Entry(FileState),
    Added,
    Removed,
    Context,
}

/// One line of a diff as drawn: where its rows are and where it sits in its
/// file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineInfo {
    /// The first row it occupies in the document, and how many.
    pub row: usize,
    pub height: usize,
    pub path: PathBuf,
    pub kind: LineKind,
    pub old: Option<usize>,
    pub new: Option<usize>,
    /// The line as git printed it, with its `+`, `-` or space.
    pub text: String,
    /// Where an editor should open for it: its line in the new file, or for
    /// a removed line the line that now follows it.
    pub edit: NonZeroUsize,
}

impl LineInfo {
    pub fn selectable(&self) -> bool {
        self.kind != LineKind::Header
    }

    fn is_hunk(&self) -> bool {
        self.kind == LineKind::Header && self.text.starts_with("@@")
    }
}

/// A hunk's side of its header: where it starts and how many lines it covers.
#[derive(Clone, Copy)]
struct Range {
    start: usize,
    count: usize,
}

impl Range {
    fn parse(range: &str) -> Option<Self> {
        let (start, count) = match range.split_once(',') {
            Some((start, count)) => (start, count.parse().ok()?),
            None => (range, 1),
        };
        Some(Self {
            start: start.parse().ok()?,
            count,
        })
    }

    /// The first line the hunk's lines count from: git names the line
    /// before an empty range.
    fn first(self) -> usize {
        if self.count == 0 {
            self.start + 1
        } else {
            self.start
        }
    }
}

/// `@@ -a,b +c,d @@` as the line each side's counting starts from.
fn hunk_start(header: &str) -> Option<(usize, usize)> {
    let rest = header.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(' ')?;
    let new = rest.strip_prefix('+')?.split(' ').next()?;
    Some((Range::parse(old)?.first(), Range::parse(new)?.first()))
}

fn line_number(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).unwrap_or(NonZeroUsize::MIN)
}

/// The kind and file positions of each line of one file's diff text.
pub fn classify(lines: &[String]) -> Vec<(LineKind, Option<usize>, Option<usize>, NonZeroUsize)> {
    let (mut old, mut new) = (1, 1);
    lines
        .iter()
        .map(|line| {
            if line.starts_with("@@") {
                if let Some((o, n)) = hunk_start(line) {
                    (old, new) = (o, n);
                }
                return (LineKind::Header, None, None, line_number(new));
            }
            match line.chars().next() {
                Some('+') => {
                    new += 1;
                    (LineKind::Added, None, Some(new - 1), line_number(new - 1))
                }
                Some('-') => {
                    old += 1;
                    (LineKind::Removed, Some(old - 1), None, line_number(new))
                }
                Some('\\') => (LineKind::Header, None, None, line_number(new)),
                _ => {
                    old += 1;
                    new += 1;
                    (
                        LineKind::Context,
                        Some(old - 1),
                        Some(new - 1),
                        line_number(new - 1),
                    )
                }
            }
        })
        .collect()
}

/// The cursor over a diff document's lines.
#[derive(Debug, Default)]
pub struct DiffView {
    /// What the cursor belongs to: another document starts afresh.
    pub owner: Option<crate::document::Slot>,
    pub cursor: usize,
    pub anchor: Option<usize>,
    /// Scroll to show the cursor at the next draw.
    pub reveal: bool,
    /// Move the cursor onto the page at the next draw.
    pub follow_page: bool,
}

impl DiffView {
    /// The lines the cursor or its selection covers, first to last.
    pub fn span(&self) -> (usize, usize) {
        match self.anchor {
            Some(anchor) => (anchor.min(self.cursor), anchor.max(self.cursor)),
            None => (self.cursor, self.cursor),
        }
    }
}

/// The nearest selectable line from `from` going `step` (1 or -1), if any.
pub fn step(lines: &[LineInfo], from: usize, step: isize) -> Option<usize> {
    let mut at = from.checked_add_signed(step)?;
    while let Some(line) = lines.get(at) {
        if line.selectable() {
            return Some(at);
        }
        at = at.checked_add_signed(step)?;
    }
    None
}

pub fn first(lines: &[LineInfo]) -> Option<usize> {
    lines.iter().position(LineInfo::selectable)
}

pub fn last(lines: &[LineInfo]) -> Option<usize> {
    lines.iter().rposition(LineInfo::selectable)
}

/// The first line of the next hunk after `from`, or of the one `from` is in
/// when going back from past its first line.
pub fn hunk(lines: &[LineInfo], from: usize, forward: bool) -> Option<usize> {
    let after = |header: usize| step(lines, header, 1);
    if forward {
        let next = (from + 1..lines.len()).find(|&i| lines[i].is_hunk())?;
        return after(next);
    }
    let current = (0..=from.min(lines.len().saturating_sub(1))).rfind(|&i| lines[i].is_hunk())?;
    let start = after(current)?;
    if from > start {
        return Some(start);
    }
    let previous = (0..current).rfind(|&i| lines[i].is_hunk())?;
    after(previous)
}

/// The first line of the next file after `from`, or of the one `from` is
/// in when going back.
pub fn file(lines: &[LineInfo], from: usize, forward: bool) -> Option<usize> {
    let mut starts = Vec::new();
    let mut previous: Option<&Path> = None;
    for (i, line) in lines.iter().enumerate().filter(|(_, l)| l.selectable()) {
        if previous != Some(line.path.as_path()) {
            starts.push(i);
        }
        previous = Some(&line.path);
    }
    let current = starts.iter().rposition(|&s| s <= from)?;
    if forward {
        starts.get(current + 1).copied()
    } else if starts[current] < from {
        Some(starts[current])
    } else {
        current.checked_sub(1).map(|i| starts[i])
    }
}

/// `path:42`, `path:10-14`, or for removed lines `path:7 (removed)`. In a
/// commit's diff the path says which commit: `path@abc1234:42`.
pub fn label(path: &Path, rev: Option<&str>, selected: &[&LineInfo]) -> String {
    let at = match rev {
        Some(rev) => format!("{}@{rev}", path.display()),
        None => path.display().to_string(),
    };
    let (Some(first), Some(last)) = (selected.first(), selected.last()) else {
        return at;
    };
    let (first, last) = (first.edit, last.edit);
    let removed = selected.iter().all(|l| l.kind == LineKind::Removed);
    let range = if first == last {
        first.to_string()
    } else {
        format!("{first}-{last}")
    };
    let note = if removed { " (removed)" } else { "" };
    format!("{at}:{range}{note}")
}

/// Text from outside, as part of a message to an agent: `\r\n` becomes
/// `\n` and other control characters, which would end the paste early,
/// become `?`.
fn outside_text(text: &str) -> String {
    clean(text, CarriageReturn::Newline, Some('?'))
}

/// A comment on lines of a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub path: PathBuf,
    /// The commit the lines are from, when they aren't the checkout's.
    pub rev: Option<String>,
    /// The first line's place, which names the comment.
    pub at: (Option<usize>, Option<usize>),
    pub label: String,
    pub quoted: Vec<String>,
    pub text: String,
}

/// Quoted lines kept per comment.
const QUOTED: usize = 6;

impl Comment {
    pub fn on(selected: &[&LineInfo], rev: Option<&str>, text: &str) -> Option<Self> {
        let first = selected.first()?;
        let mut quoted: Vec<String> = selected
            .iter()
            .map(|l| outside_text(l.text.trim_end_matches('\r')))
            .collect();
        if quoted.len() > QUOTED {
            quoted.truncate(QUOTED);
            quoted.push("...".into());
        }
        Some(Self {
            path: first.path.clone(),
            rev: rev.map(str::to_string),
            at: (first.old, first.new),
            label: outside_text(&label(&first.path, rev, selected)),
            quoted,
            text: text.to_string(),
        })
    }

    pub fn same_place(&self, other: &Comment) -> bool {
        self.path == other.path && self.rev == other.rev && self.at == other.at
    }

    pub fn covers(&self, line: &LineInfo, rev: Option<&str>) -> bool {
        self.path == line.path && self.rev.as_deref() == rev && self.at == (line.old, line.new)
    }
}

/// The message sent to an agent for a draft: the nvim review format, with
/// each comment's quoted lines so it finds the code after the lines drift.
pub fn review_message(overview: &str, comments: &[Comment]) -> String {
    let mut out = String::from("Review comments. Work through each one.\n");
    let overview = overview.trim();
    if !overview.is_empty() {
        out.push('\n');
        out.push_str(overview);
        out.push('\n');
    }
    for comment in comments {
        out.push('\n');
        out.push_str(&comment.label);
        out.push('\n');
        for line in &comment.quoted {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(comment.text.trim());
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{line_info, nz};

    fn text(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|l| (*l).to_string()).collect()
    }

    fn infos(files: &[(&str, &[&str])]) -> Vec<LineInfo> {
        let mut out = Vec::new();
        for (path, lines) in files {
            let lines = text(lines);
            for (line, (kind, old, new, edit)) in lines.iter().zip(classify(&lines)) {
                out.push(LineInfo {
                    row: out.len(),
                    height: 1,
                    path: (*path).into(),
                    kind,
                    old,
                    new,
                    text: line.clone(),
                    edit,
                });
            }
        }
        out
    }

    #[test]
    fn lines_know_their_place_in_the_new_file_from_the_hunk_header() {
        let lines = text(&[
            "@@ -10,3 +20,4 @@ fn x",
            " ctx",
            "-gone",
            "+one",
            "+two",
            "\\ No newline",
        ]);
        let got = classify(&lines);
        assert_eq!(got[0].0, LineKind::Header);
        assert_eq!(got[1], (LineKind::Context, Some(10), Some(20), nz(20)));
        assert_eq!(got[2], (LineKind::Removed, Some(11), None, nz(21)));
        assert_eq!(got[3], (LineKind::Added, None, Some(21), nz(21)));
        assert_eq!(got[4], (LineKind::Added, None, Some(22), nz(22)));
        assert_eq!(got[5].0, LineKind::Header);
    }

    #[test]
    fn a_diff_without_a_header_counts_from_line_one() {
        let got = classify(&text(&["+a", "+b"]));
        assert_eq!(got[1].2, Some(2));
    }

    #[test]
    fn the_cursor_skips_headers_and_hunks_and_files_are_jumps() {
        let lines = infos(&[
            ("a", &["@@ -1 +1 @@", "-x", "+y", "@@ -9 +9 @@", " z", "+w"]),
            ("b", &["@@ -1 +1 @@", "+q"]),
        ]);
        assert_eq!(first(&lines), Some(1));
        assert_eq!(last(&lines), Some(7));
        assert_eq!(step(&lines, 2, 1), Some(4), "over the second hunk's header");
        assert_eq!(step(&lines, 1, -1), None);
        assert_eq!(hunk(&lines, 1, true), Some(4));
        assert_eq!(
            hunk(&lines, 4, true),
            Some(7),
            "into the next file's first hunk"
        );
        assert_eq!(hunk(&lines, 5, false), Some(4), "back to the hunk's start");
        assert_eq!(hunk(&lines, 4, false), Some(1), "then the one before");
        assert_eq!(hunk(&lines, 1, false), None);
        assert_eq!(file(&lines, 2, true), Some(7));
        assert_eq!(file(&lines, 7, false), Some(1));
        assert_eq!(file(&lines, 2, false), Some(1));
        assert_eq!(file(&lines, 7, true), None);
    }

    #[test]
    fn labels_name_the_line_or_range_and_say_when_removed() {
        let lines = infos(&[(
            "a.rs",
            &["@@ -5,2 +5,2 @@", "-gone", "-gone2", "+one", " ctx"],
        )]);
        let pick = |r: std::ops::Range<usize>| lines[r].iter().collect::<Vec<_>>();
        assert_eq!(label(Path::new("a.rs"), None, &pick(3..4)), "a.rs:5");
        assert_eq!(label(Path::new("a.rs"), None, &pick(3..5)), "a.rs:5-6");
        assert_eq!(
            label(Path::new("a.rs"), None, &pick(1..2)),
            "a.rs:5 (removed)"
        );
    }

    #[test]
    fn a_review_message_has_the_overview_then_each_comment_with_its_quote() {
        let lines = infos(&[("a.rs", &["@@ -1 +1 @@", "+let x = 3;"])]);
        let comment = Comment::on(&[&lines[1]], None, "Use the limit.").expect("a line");
        assert!(comment.covers(&lines[1], None));
        assert!(!comment.covers(&lines[1], Some("abc1234")));
        assert_eq!(
            review_message("", std::slice::from_ref(&comment)),
            "Review comments. Work through each one.\n\na.rs:1\n> +let x = 3;\nUse the limit."
        );
        let with = review_message("Mostly fine.", &[comment]);
        assert!(with.contains("\n\nMostly fine.\n\na.rs:1"), "{with}");
    }

    #[test]
    fn a_long_selection_is_quoted_in_part() {
        let lines: Vec<String> = (0..10).map(|i| format!("+l{i}")).collect();
        let mut all = infos(&[("a", &[])]);
        all.extend(
            lines
                .iter()
                .enumerate()
                .map(|(i, l)| line_info("a", l, i + 1)),
        );
        let refs: Vec<&LineInfo> = all.iter().collect();
        let comment = Comment::on(&refs, None, "x").expect("lines");
        assert_eq!(comment.quoted.len(), QUOTED + 1);
        assert_eq!(comment.quoted.last().map(String::as_str), Some("..."));
    }

    #[test]
    fn a_pure_deletion_names_the_line_that_now_follows_it() {
        let lines = infos(&[(
            "a.rs",
            &[
                "@@ -5,3 +4,0 @@",
                "-a",
                "-b",
                "-c",
                "@@ -9,0 +8,2 @@",
                "+x",
                "+y",
            ],
        )]);
        assert_eq!(lines[1].edit, nz(5));
        assert_eq!(lines[3].edit, nz(5));
        assert_eq!(lines[5].edit, nz(8));
        assert_eq!(lines[6].edit, nz(9));
        let pick = |r: std::ops::Range<usize>| lines[r].iter().collect::<Vec<_>>();
        assert_eq!(
            label(Path::new("a.rs"), None, &pick(1..4)),
            "a.rs:5 (removed)"
        );
    }

    #[test]
    fn a_comment_in_a_commit_names_it_and_only_covers_lines_of_it() {
        let lines = infos(&[("a.rs", &["@@ -1 +1 @@", "+x"])]);
        let comment = Comment::on(&[&lines[1]], Some("abc1234"), "No.").expect("a line");
        assert_eq!(comment.label, "a.rs@abc1234:1");
        assert!(comment.covers(&lines[1], Some("abc1234")));
        assert!(!comment.covers(&lines[1], None));
    }

    #[test]
    fn quoted_lines_lose_carriage_returns_and_control_characters() {
        let lines = infos(&[("a.rs", &["@@ -1 +1 @@", "+let x = 1;\r", "+a\x1b[201~b\tc"])]);
        let comment = Comment::on(&[&lines[1], &lines[2]], None, "Fix.").expect("lines");
        assert_eq!(comment.quoted, ["+let x = 1;", "+a?[201~b\tc"]);
        let message = review_message("", &[comment]);
        assert!(
            message.parse::<agent_core::names::Prompt>().is_ok(),
            "{message:?}"
        );
    }
}
