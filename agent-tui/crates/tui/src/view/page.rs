//! The main view [0] when it reads a document instead of showing the
//! terminal.

use std::time::Instant;

use agent_core::ports::{FileDiff, Lines};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use super::sidebar::{fits_beside, status_line};
use super::{
    bold, dim, error, justify, line_width, success, truncate_end, truncate_start, warning,
};
use crate::model::{AgentStatus, Model};

/// [0]'s title, what its top border says on the right, its bottom-right
/// corner and its rows.
pub(super) struct Page {
    pub title: Vec<Span<'static>>,
    pub right: Vec<Span<'static>>,
    pub corner: Vec<Span<'static>>,
    pub lines: Vec<Line<'static>>,
}

/// The hovered file's diff in `area`, scrolled as the model says, while the
/// cursor is on a file in [3]. `border` is the corner's style and `width`
/// the whole panel's.
pub(super) fn diff(m: &mut Model, area: Rect, width: usize, border: Style) -> Option<Page> {
    let (hover, _) = m.hovered_diff()?;
    let columns = usize::from(area.width);
    // An agent that asks takes the top row, since it needs an answer and
    // the document doesn't. It never switches [0] to the terminal itself.
    let question = m.selected_entry().and_then(|e| match m.status(e) {
        AgentStatus::Asking { question, .. } => Some(question),
        _ => None,
    });
    let reserved = if question.is_some() { 2 } else { 0 };
    let height = usize::from(area.height).saturating_sub(reserved);
    let rows = m
        .docs
        .shown()
        .map(|(_, loaded)| diff_rows(loaded, columns))
        .unwrap_or_default();
    let top = m.docs.scroll.min(rows.len().saturating_sub(height));
    m.docs.scroll = top;
    m.docs.page = height;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(question) = question {
        let ask = "i answer";
        let room = columns.saturating_sub(ask.len() + 3);
        let text = question.lines().next().unwrap_or_default().trim();
        lines.push(justify(
            vec![
                Span::styled("◆ ", warning().add_modifier(Modifier::BOLD)),
                Span::styled(truncate_end(text, room), warning()),
            ],
            vec![Span::raw(ask)],
            columns,
        ));
        lines.push(Line::raw(""));
    }
    lines.extend(rows.iter().skip(top).take(height).cloned());

    let mut title = vec![
        Span::raw("[0] "),
        Span::styled(
            truncate_start(
                &hover.path.to_string_lossy(),
                width.saturating_sub(16).max(8),
            ),
            bold(),
        ),
    ];
    match hover.lines {
        Lines::Counted { added, removed } => {
            for (count, text, style) in [
                (added, format!("+{added}"), success()),
                (removed, format!("−{removed}"), error()),
            ] {
                if count > 0 {
                    title.push(Span::raw(" "));
                    title.push(Span::styled(text, style));
                }
            }
        }
        Lines::Binary => title.push(Span::styled(" bin", dim())),
        Lines::Untracked => title.push(Span::styled(" new", dim())),
    }

    let status = m
        .selected_entry()
        .map(|entry| status_line(m, entry, width.saturating_sub(line_width(&title) + 9)))
        .unwrap_or_default();
    let right = if fits_beside(&title, line_width(&status), width) {
        status
    } else {
        Vec::new()
    };

    let mut corner = Vec::new();
    if m.docs.loading(Instant::now()) {
        corner.push(Span::styled("loading", dim()));
    } else if rows.len() > height {
        corner.push(Span::styled(
            format!(
                "{}-{} of {}",
                top + 1,
                (top + height).min(rows.len()),
                rows.len()
            ),
            border,
        ));
    }
    Some(Page {
        title,
        right,
        corner,
        lines,
    })
}

fn diff_rows(loaded: &Result<FileDiff, String>, width: usize) -> Vec<Line<'static>> {
    match loaded {
        Err(e) => vec![Line::styled(
            e.lines().next().unwrap_or_default().to_string(),
            error(),
        )],
        Ok(FileDiff::Binary { bytes }) => vec![Line::styled(
            match bytes {
                Some(bytes) => format!("binary · {}", size(*bytes)),
                None => "binary".to_string(),
            },
            dim(),
        )],
        Ok(FileDiff::Text { lines, more }) => {
            let mut rows: Vec<Line<'static>> = Vec::new();
            for line in lines {
                let style = match line.chars().next() {
                    Some('+') => success(),
                    Some('-') => error(),
                    Some('@') => dim(),
                    _ => Style::new(),
                };
                rows.extend(
                    hard_wrap(&line.replace('\t', "    "), width)
                        .into_iter()
                        .map(|row| Line::styled(row, style)),
                );
            }
            if *more > 0 {
                rows.push(Line::styled(
                    format!("{} more lines · e open", thousands(*more)),
                    dim(),
                ));
            } else if rows.is_empty() {
                rows.push(Line::styled("no changes to show", dim()));
            }
            rows
        }
    }
}

/// `text` cut into rows of at most `width` columns, exactly where the
/// columns run out, so a diff's spacing survives. Control characters go.
fn hard_wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut used = 0;
    for c in text.chars().filter(|c| !c.is_control()) {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            rows.push(String::new());
            used = 0;
        }
        if let Some(row) = rows.last_mut() {
            row.push(c);
        }
        used += w;
    }
    rows
}

fn size(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{} KB", bytes / 1024),
        _ => format!("{} MB", bytes / 1_048_576),
    }
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_wrap_where_the_columns_run_out() {
        assert_eq!(hard_wrap("+abcdef", 4), ["+abc", "def"]);
        assert_eq!(hard_wrap("", 4), [""]);
        assert_eq!(
            hard_wrap("a漢b", 2),
            ["a", "漢", "b"],
            "wide characters stay whole"
        );
    }

    #[test]
    fn counts_read_with_separators() {
        assert_eq!(thousands(12_340), "12,340");
        assert_eq!(thousands(7), "7");
        assert_eq!(size(48 * 1024), "48 KB");
    }
}
