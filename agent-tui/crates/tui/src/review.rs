//! Reading a diff line by line: what each shown line is in its file, the
//! cursor over them, and the comments a reviewer drafts for an agent.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// A hunk header or a `\ No newline` note: shown, never selected.
    Header,
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
    pub edit: usize,
}

impl LineInfo {
    pub fn selectable(&self) -> bool {
        self.kind != LineKind::Header
    }

    fn is_hunk(&self) -> bool {
        self.kind == LineKind::Header && self.text.starts_with("@@")
    }
}

/// `@@ -a,b +c,d @@` as `(a, c)`.
fn hunk_start(header: &str) -> Option<(usize, usize)> {
    let rest = header.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(' ')?;
    let new = rest.strip_prefix('+')?.split(' ').next()?;
    let number = |range: &str| range.split(',').next()?.parse().ok();
    Some((number(old)?, number(new)?))
}

/// The kind and file positions of each line of one file's diff text.
pub fn classify(lines: &[String]) -> Vec<(LineKind, Option<usize>, Option<usize>, usize)> {
    let (mut old, mut new) = (1, 1);
    lines
        .iter()
        .map(|line| {
            if line.starts_with("@@") {
                if let Some((o, n)) = hunk_start(line) {
                    (old, new) = (o, n);
                }
                return (LineKind::Header, None, None, new);
            }
            match line.chars().next() {
                Some('+') => {
                    new += 1;
                    (LineKind::Added, None, Some(new - 1), new - 1)
                }
                Some('-') => {
                    old += 1;
                    (LineKind::Removed, Some(old - 1), None, new.max(1))
                }
                Some('\\') => (LineKind::Header, None, None, new),
                _ => {
                    old += 1;
                    new += 1;
                    (LineKind::Context, Some(old - 1), Some(new - 1), new - 1)
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
#[allow(dead_code)]
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

/// `path:42`, `path:10-14`, or for removed lines `path:7 (removed)`.
#[allow(dead_code)]
pub fn label(path: &Path, selected: &[&LineInfo]) -> String {
    let edits: Vec<usize> = selected.iter().map(|l| l.edit).collect();
    let (Some(first), Some(last)) = (edits.first(), edits.last()) else {
        return path.display().to_string();
    };
    let removed = selected.iter().all(|l| l.kind == LineKind::Removed);
    let range = if first == last {
        first.to_string()
    } else {
        format!("{first}-{last}")
    };
    let note = if removed { " (removed)" } else { "" };
    format!("{}:{range}{note}", path.display())
}

/// A comment on lines of a diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub path: PathBuf,
    /// The first line's place, which names the comment.
    pub at: (Option<usize>, Option<usize>),
    pub label: String,
    pub quoted: Vec<String>,
    pub text: String,
}

/// Quoted lines kept per comment.
#[allow(dead_code)]
const QUOTED: usize = 6;

#[allow(dead_code)]
impl Comment {
    pub fn on(selected: &[&LineInfo], text: String) -> Option<Self> {
        let first = selected.first()?;
        let mut quoted: Vec<String> = selected.iter().map(|l| l.text.clone()).collect();
        if quoted.len() > QUOTED {
            quoted.truncate(QUOTED);
            quoted.push("...".into());
        }
        Some(Self {
            path: first.path.clone(),
            at: (first.old, first.new),
            label: label(&first.path, selected),
            quoted,
            text,
        })
    }

    pub fn covers(&self, line: &LineInfo) -> bool {
        self.path == line.path && self.at == (line.old, line.new)
    }
}

/// The message sent to an agent for a draft: the nvim review format, with
/// each comment's quoted lines so it finds the code after the lines drift.
#[allow(dead_code)]
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
        assert_eq!(got[1], (LineKind::Context, Some(10), Some(20), 20));
        assert_eq!(got[2], (LineKind::Removed, Some(11), None, 21));
        assert_eq!(got[3], (LineKind::Added, None, Some(21), 21));
        assert_eq!(got[4], (LineKind::Added, None, Some(22), 22));
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
        assert_eq!(label(Path::new("a.rs"), &pick(3..4)), "a.rs:5");
        assert_eq!(label(Path::new("a.rs"), &pick(3..5)), "a.rs:5-6");
        assert_eq!(label(Path::new("a.rs"), &pick(1..2)), "a.rs:5 (removed)");
    }

    #[test]
    fn a_review_message_has_the_overview_then_each_comment_with_its_quote() {
        let lines = infos(&[("a.rs", &["@@ -1 +1 @@", "+let x = 3;"])]);
        let comment = Comment::on(&[&lines[1]], "Use the limit.".into()).expect("a line");
        assert!(comment.covers(&lines[1]));
        assert_eq!(
            review_message("", &[comment.clone()]),
            "Review comments. Work through each one.\n\na.rs:1\n> +let x = 3;\nUse the limit."
        );
        let with = review_message("Mostly fine.", &[comment]);
        assert!(with.contains("\n\nMostly fine.\n\na.rs:1"), "{with}");
    }

    #[test]
    fn a_long_selection_is_quoted_in_part() {
        let lines: Vec<String> = (0..10).map(|i| format!("+l{i}")).collect();
        let mut all = infos(&[("a", &[])]);
        all.extend(lines.iter().enumerate().map(|(i, l)| LineInfo {
            row: i,
            height: 1,
            path: "a".into(),
            kind: LineKind::Added,
            old: None,
            new: Some(i + 1),
            text: l.clone(),
            edit: i + 1,
        }));
        let refs: Vec<&LineInfo> = all.iter().collect();
        let comment = Comment::on(&refs, "x".into()).expect("lines");
        assert_eq!(comment.quoted.len(), QUOTED + 1);
        assert_eq!(comment.quoted.last().map(String::as_str), Some("..."));
    }
}
