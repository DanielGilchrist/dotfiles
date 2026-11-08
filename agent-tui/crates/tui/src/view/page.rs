//! The main view [0] when it reads a document instead of showing the
//! terminal.

use std::fmt::Write;
use std::time::Instant;

use agent_core::activity::AgentId;
use agent_core::model::{Head, RepoKey};
use agent_core::names::SessionName;
use agent_core::ports::{BranchLog, Change, Compared, FileDiff, Lines, LogEntry};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::sidebar::{fits_beside, others_on, status_line, subagent_rows, turn_message};
use super::{
    ago, bold, dim, error, fit, justify, line_width, spinner, success, truncate_end,
    truncate_start, warning, wrap,
};
use crate::document::{BranchKey, DiffKey, Doc, FolderKey, Hover, Resolved};
use crate::model::{AgentStatus, Model};

/// [0]'s title, what its top border says on the right, its bottom-right
/// corner and its rows.
pub(super) struct Page {
    pub title: Vec<Span<'static>>,
    pub right: Vec<Span<'static>>,
    pub corner: Vec<Span<'static>>,
    pub lines: Vec<Line<'static>>,
}

/// A document's rows: built for this frame, or the ones the document cache
/// kept from an earlier frame.
enum Rows {
    Built(Vec<Line<'static>>),
    Cached,
}

/// What a document says before it's scrolled and framed.
struct Body {
    title: Vec<Span<'static>>,
    rows: Rows,
    /// A log, which follows its end.
    tails: bool,
    /// Still being read.
    loading: bool,
}

impl Body {
    fn built(title: Vec<Span<'static>>, rows: Vec<Line<'static>>) -> Self {
        Self {
            title,
            rows: Rows::Built(rows),
            tails: false,
            loading: false,
        }
    }
}

/// A result sits within this many columns of its call, so it stays near it
/// on a wide screen.
const RESULT_COLUMNS: usize = 100;

/// What [0] reads instead of the terminal, in `area`, scrolled as the model
/// says, or `None` while the cursor is on nothing it reads. `border` is the
/// corner's style and `width` the whole panel's.
pub(super) fn page(m: &mut Model, area: Rect, width: usize, border: Style) -> Option<Page> {
    let hover = m.hover()?;
    let columns = usize::from(area.width);
    // An agent that asks takes the top row, since it needs an answer and
    // the document doesn't. It never switches [0] to the terminal itself.
    let question = m.selected_entry().and_then(|e| match m.status(e) {
        AgentStatus::Asking { question, .. } => Some(question),
        _ => None,
    });
    let reserved = if question.is_some() { 2 } else { 0 };
    let height = usize::from(area.height).saturating_sub(reserved);
    // The agent's state keeps its glyph, name and time, so the title yields
    // to them first.
    let status_least = m
        .selected_entry()
        .map_or(0, |entry| line_width(&status_line(m, entry, 0)));
    let budget = width.saturating_sub(7 + status_least);
    let body = match &hover {
        Hover::Diff(key) => diff_body(m, key, columns, budget),
        Hover::Branch(key) => branch_body(m, key, columns),
        Hover::Folder(key) => folder_body(m, key, columns, budget),
        Hover::Turn(name) => turn_body(m, name, columns)?,
        Hover::Subagent(name, id) => log_body(m, name, id, columns),
        Hover::Repo(key) => repo_body(m, key, columns)?,
    };
    let total = match &body.rows {
        Rows::Built(rows) => rows.len(),
        Rows::Cached => m.docs.cached().len(),
    };
    let top = m.docs.place(total, height, body.tails);
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
    match &body.rows {
        Rows::Built(rows) => lines.extend(rows.iter().skip(top).take(height).cloned()),
        Rows::Cached => lines.extend(m.docs.cached().iter().skip(top).take(height).cloned()),
    }

    let mut title = body.title;
    if !m.sidebar_shown
        && let Some((at, of)) = m.position()
    {
        title.insert(1, Span::styled(format!("{at} of {of} · "), dim()));
    }
    let title = fit(title, width.saturating_sub(6 + status_least).max(8));
    let status = m
        .selected_entry()
        .map(|entry| status_line(m, entry, width.saturating_sub(line_width(&title) + 7)))
        .unwrap_or_default();
    let right = if fits_beside(&title, line_width(&status), width) {
        status
    } else {
        Vec::new()
    };

    let shown = format!("{}-{} of {total}", top + 1, (top + height).min(total));
    let corner = if body.loading || m.docs.loading(Instant::now()) {
        vec![Span::styled("loading", dim())]
    } else if m.docs.following() {
        vec![Span::styled("following", border)]
    } else if m.docs.paused() {
        vec![Span::styled(format!("paused {shown}"), border)]
    } else if total > height {
        vec![Span::styled(shown, border)]
    } else {
        Vec::new()
    };
    Some(Page {
        title,
        right,
        corner,
        lines,
    })
}

/// `names` as far as `room` columns allow, the rest counted: `a, b +2`.
fn names_in(names: &[String], room: usize) -> String {
    let mut shown = String::new();
    for (at, name) in names.iter().enumerate() {
        let left = names.len() - at - 1;
        let more = if left > 0 {
            format!(" +{left}").len()
        } else {
            0
        };
        let sep = if shown.is_empty() { 0 } else { 2 };
        if shown.len() + sep + name.len() + more > room {
            break;
        }
        if sep > 0 {
            shown.push_str(", ");
        }
        shown.push_str(name);
    }
    if shown.is_empty() {
        return names.len().to_string();
    }
    let left = names.len() - shown.split(", ").count();
    if left > 0 {
        let _ = write!(shown, " +{left}");
    }
    shown
}

/// The hovered file's diff, or the last read of the same file while the new
/// one loads, under the file's name, counts and who else changed it. Another
/// file's diff is never shown under this one's title.
fn diff_body(m: &mut Model, hover: &DiffKey, columns: usize, budget: usize) -> Body {
    let rows = match m.docs.resolve() {
        Resolved::Current(key, loaded) | Resolved::Stale(key, loaded) => {
            let (key, loaded) = (key.clone(), loaded.clone());
            m.docs.rows(&key, columns, || match loaded.as_ref() {
                Ok(Doc::Diff(diff)) => diff_rows(Ok(diff), columns),
                Ok(Doc::Branch(_)) => Vec::new(),
                Err(e) => diff_rows(Err(e), columns),
            });
            Rows::Cached
        }
        Resolved::Missing => Rows::Built(Vec::new()),
    };
    let mut counts = Vec::new();
    match hover.lines {
        Lines::Counted { added, removed } => {
            for (count, text, style) in [
                (added, format!(" +{added}"), success()),
                (removed, format!(" −{removed}"), error()),
            ] {
                if count > 0 {
                    counts.push(Span::styled(text, style));
                }
            }
        }
        Lines::Binary => counts.push(Span::styled(" bin", dim())),
        Lines::Untracked => counts.push(Span::styled(" new", dim())),
    }
    let others = others_on(m, &hover.name, &hover.path);
    let fixed = 4 + line_width(&counts);
    let path_room = budget.saturating_sub(fixed).max(12);
    let path = truncate_start(&hover.path.to_string_lossy(), path_room);
    let mut title = vec![Span::raw("[0] "), Span::styled(path, bold())];
    title.extend(counts);
    let left = budget.saturating_sub(line_width(&title) + 4);
    if !others.is_empty() && left >= 8 {
        title.push(Span::raw("  "));
        title.push(Span::styled("∩ ", warning()));
        title.push(Span::styled(names_in(&others, left), dim()));
    }
    Body {
        title,
        rows,
        tails: false,
        loading: false,
    }
}

/// A folder: the files changed under it with their counts. The list comes
/// from the git facts already read, so nothing waits on git (a combined
/// diff would be one more read per folder).
fn folder_body(m: &Model, key: &FolderKey, columns: usize, budget: usize) -> Body {
    let files: Vec<&Change> = m
        .changes()
        .iter()
        .filter(|c| c.path.starts_with(&key.path))
        .collect();
    let (mut added, mut removed) = (0, 0);
    let cells: Vec<(String, Span<'static>, String)> = files
        .iter()
        .map(|c| {
            let rel = c
                .path
                .strip_prefix(&key.path)
                .unwrap_or(&c.path)
                .to_string_lossy()
                .into_owned();
            match c.lines {
                Lines::Counted {
                    added: a,
                    removed: r,
                } => {
                    added += a;
                    removed += r;
                    (
                        rel,
                        Span::styled(
                            if a > 0 {
                                format!("+{a}")
                            } else {
                                String::new()
                            },
                            success(),
                        ),
                        if r > 0 {
                            format!("−{r}")
                        } else {
                            String::new()
                        },
                    )
                }
                Lines::Binary => (rel, Span::styled("bin", dim()), String::new()),
                Lines::Untracked => (rel, Span::styled("new", dim()), String::new()),
            }
        })
        .collect();
    let plus = cells.iter().map(|(_, a, _)| a.width()).max().unwrap_or(0);
    let minus = cells.iter().map(|(_, _, r)| r.width()).max().unwrap_or(0);
    let rows: Vec<Line<'static>> = cells
        .into_iter()
        .map(|(rel, a, r)| {
            let mut right = vec![Span::styled(format!("{:>plus$}", a.content), a.style)];
            if minus > 0 {
                right.push(Span::styled(format!(" {r:<minus$}"), error()));
            }
            let room = columns.saturating_sub(line_width(&right) + 1);
            justify(vec![Span::raw(truncate_start(&rel, room))], right, columns)
        })
        .collect();
    let mut title = vec![
        Span::raw("[0] "),
        Span::styled(
            truncate_start(
                &format!("{}/", key.path.display()),
                budget.saturating_sub(24).max(12),
            ),
            bold(),
        ),
        Span::styled(format!(" {} files", files.len()), dim()),
    ];
    for (count, text, style) in [
        (added, format!(" +{added}"), success()),
        (removed, format!(" −{removed}"), error()),
    ] {
        if count > 0 {
            title.push(Span::styled(text, style));
        }
    }
    Body::built(title, rows)
}

/// The branch: how far it is from shipping, its commits and the files not
/// yet committed.
fn branch_body(m: &mut Model, key: &BranchKey, columns: usize) -> Body {
    let title = vec![
        Span::raw("[0] "),
        Span::styled(key.compared.tab(), bold()),
        Span::styled(format!(" · {}", compared_title(&key.compared)), dim()),
    ];
    let compared = key.compared.clone();
    let behind = key.behind;
    let rows = match m.docs.resolve() {
        Resolved::Current(key, loaded) | Resolved::Stale(key, loaded) => {
            let (key, loaded) = (key.clone(), loaded.clone());
            m.docs.rows(&key, columns, || match loaded.as_ref() {
                Ok(Doc::Branch(log)) => branch_rows(log, &compared, behind, columns),
                Ok(Doc::Diff(_)) => Vec::new(),
                Err(e) => vec![Line::styled(
                    e.lines().next().unwrap_or_default().to_string(),
                    error(),
                )],
            });
            Rows::Cached
        }
        Resolved::Missing => Rows::Built(Vec::new()),
    };
    Body {
        title,
        rows,
        tails: false,
        loading: false,
    }
}

/// A branch log as rows: its commits, then the files not committed.
fn branch_rows(
    log: &BranchLog,
    compared: &Compared,
    behind: usize,
    columns: usize,
) -> Vec<Line<'static>> {
    let mut rows: Vec<Line<'static>> = wrap(&compared_explanation(compared), columns)
        .into_iter()
        .map(|row| Line::styled(row, dim()))
        .collect();
    rows.push(Line::raw(""));
    if log.commits.is_empty() && log.uncommitted.is_empty() {
        rows.push(Line::styled("nothing ahead of the default branch", dim()));
    }
    if !log.commits.is_empty() {
        rows.push(Line::styled(
            format!("Commits ({})", log.commits.len()),
            dim(),
        ));
        for commit in &log.commits {
            let (hash, subject) = commit.split_once(' ').unwrap_or((commit, ""));
            let room = columns.saturating_sub(hash.len() + 1);
            for (i, row) in hard_wrap(subject, room).into_iter().enumerate() {
                let lead = if i == 0 {
                    hash.to_string()
                } else {
                    " ".repeat(hash.len())
                };
                rows.push(Line::from(vec![
                    Span::styled(lead, dim()),
                    Span::raw(format!(" {row}")),
                ]));
            }
        }
        rows.push(Line::raw(""));
    }
    if !log.uncommitted.is_empty() {
        rows.push(Line::styled(
            format!("Uncommitted ({})", log.uncommitted.len()),
            dim(),
        ));
        for (code, path) in &log.uncommitted {
            rows.push(Line::from(vec![
                Span::styled(format!("{code} "), dim()),
                Span::raw(truncate_start(
                    &path.to_string_lossy(),
                    columns.saturating_sub(code.len() + 1),
                )),
            ]));
        }
    }
    if let (true, Some(base)) = (behind > 0, compared.base()) {
        rows.push(Line::raw(""));
        rows.push(Line::styled(format!("behind {base} by {behind}"), dim()));
    }
    rows
}

fn compared_title(compared: &Compared) -> String {
    match compared.base() {
        Some(base) => format!("{} vs {base}", compared.head()),
        None => format!("{} · no base", compared.head()),
    }
}

fn compared_explanation(compared: &Compared) -> String {
    let head = compared.head();
    match compared {
        Compared::Trunk { base, .. } => format!(
            "Commits on {head} that aren't on {base} yet, then uncommitted work. Pushing empties the commits."
        ),
        Compared::Branch { base, .. } => format!(
            "What {head} changes since it left {base}, like a pull request, then uncommitted work."
        ),
        Compared::Detached { base, .. } => {
            format!("What {head} changes since it left {base}, then uncommitted work.")
        }
        Compared::NoBase { .. } => {
            "There is no default branch to compare with, so only uncommitted work is listed."
                .to_string()
        }
    }
}

/// A repo row's summary: its checkouts as the cards say them.
fn repo_body(m: &Model, key: &RepoKey, columns: usize) -> Option<Body> {
    let repo = m.snapshot()?.repo(key)?;
    let mut rows: Vec<Line<'static>> = Vec::new();
    let branch = repo.entries().iter().find_map(|e| match e.head() {
        Some(Head::Branch(branch)) if e.path().is_some() => Some(branch.to_string()),
        _ => None,
    });
    for entry in repo.entries().iter().filter(|e| m.shown(e)) {
        let mut line = status_line(m, entry, columns.saturating_sub(2));
        let work = m.facts.get(entry.name());
        if let Some(Ok(facts)) = work {
            let d = facts.diff();
            if d.files > 0 {
                line.push(Span::styled(
                    format!("  {} files +{} −{}", d.files, d.added, d.removed),
                    dim(),
                ));
            }
        }
        rows.push(Line::from(line));
    }
    if rows.is_empty() {
        rows.push(Line::styled("nothing running", dim()));
    }
    let mut title = vec![
        Span::raw("[0] "),
        Span::styled(repo.name().into_owned(), bold()),
    ];
    if let Some(branch) = branch {
        title.push(Span::styled(format!(" {branch}"), dim()));
    }
    Some(Body::built(title, rows))
}

/// The turn: the whole prompt, the whole message or question, and the
/// subagents.
fn turn_body(m: &Model, name: &SessionName, columns: usize) -> Option<Body> {
    let (_, entry) = m.snapshot()?.entry(name)?;
    let activity = m.activity(name)?;
    let mut rows: Vec<Line<'static>> = Vec::new();
    if let Some(prompt) = &activity.prompt {
        for (i, row) in wrap(prompt, columns.saturating_sub(2))
            .into_iter()
            .enumerate()
        {
            let lead = if i == 0 { "› " } else { "  " };
            rows.push(Line::styled(format!("{lead}{row}"), dim()));
        }
        rows.push(Line::raw(""));
    }
    if let Some(message) = turn_message(m, entry) {
        rows.extend(wrap(&message, columns).into_iter().map(Line::raw));
        rows.push(Line::raw(""));
    }
    if let Some(queue) = m.outboxes.get(name) {
        rows.extend(queue.queue.iter().map(|q| {
            Line::styled(
                truncate_end(
                    &format!("queued: {}", crate::update::first_words(q.as_ref())),
                    columns,
                ),
                dim(),
            )
        }));
        if queue.stalled(m.activity(name).map(|a| a.events)) {
            rows.push(Line::styled(
                truncate_end(&format!("queued · waiting for {name} to report"), columns),
                dim(),
            ));
        }
        rows.push(Line::raw(""));
    }
    let subagents = subagent_rows(m, entry, columns);
    if !subagents.is_empty() {
        rows.push(Line::styled("Subagents", dim()));
        rows.extend(subagents);
    }
    if rows
        .last()
        .is_some_and(|row| row.spans.is_empty() || line_width(&row.spans) == 0)
    {
        rows.pop();
    }
    Some(Body::built(
        vec![Span::raw("[0] "), Span::styled("Turn", bold())],
        rows,
    ))
}

/// A subagent's transcript: what it was told, then a line per tool call
/// with the start of its result, and what it said.
fn log_body(m: &Model, name: &SessionName, id: &AgentId, columns: usize) -> Body {
    let subagent = m.activity(name).and_then(|a| a.subagents.get(id));
    let mut title = vec![Span::raw("[0] ")];
    match subagent {
        Some(s) => {
            title.push(Span::styled(s.kind.clone(), bold()));
            if let Some(description) = s.description.as_deref().filter(|d| !d.is_empty()) {
                title.push(Span::styled(format!(" · {description}"), dim()));
            }
        }
        None => title.push(Span::styled("subagent", bold())),
    }
    let note = |text: String, style: Style| Body {
        tails: true,
        ..Body::built(title.clone(), vec![Line::styled(text, style)])
    };
    let key = (name.clone(), id.clone());
    let detail = match m.subagent_logs.get(&key) {
        None => {
            return Body {
                loading: true,
                ..note(String::new(), dim())
            };
        }
        Some(Err(e)) => return note(e.lines().next().unwrap_or_default().to_string(), error()),
        Some(Ok(None)) => return note("no transcript for this subagent".into(), dim()),
        Some(Ok(Some(detail))) => detail,
    };
    let running = subagent
        .and_then(|s| s.tools.last())
        .map(|tool| ago(tool.since, m.now));
    let mut rows: Vec<Line<'static>> = Vec::new();
    for (i, row) in wrap(&detail.prompt, columns.saturating_sub(2))
        .into_iter()
        .enumerate()
    {
        rows.push(Line::raw(format!(
            "{}{row}",
            if i == 0 { "› " } else { "  " }
        )));
    }
    rows.push(Line::raw(""));
    let cap = columns.min(RESULT_COLUMNS);
    let last = detail.log.len().saturating_sub(1);
    let ended = detail.finished && matches!(detail.log.last(), Some(LogEntry::Said(_)));
    let body = if ended { last } else { detail.log.len() };
    for (at, entry) in detail.log.iter().take(body).enumerate() {
        match entry {
            LogEntry::Said(text) => {
                rows.extend(wrap(text, columns).into_iter().map(Line::raw));
            }
            LogEntry::Called {
                tool,
                target,
                result,
            } => {
                let live = result.is_none() && at == last && !detail.finished;
                let lead = if live {
                    Span::raw(format!("{} ", spinner()))
                } else {
                    Span::raw("  ")
                };
                let right = match (result, live) {
                    (Some(result), _) => result.clone(),
                    (None, true) => running
                        .as_ref()
                        .map_or_else(|| "running".to_string(), |since| format!("running {since}")),
                    (None, false) => String::new(),
                };
                rows.push(justify(
                    vec![
                        lead,
                        Span::styled(format!("{tool}  "), dim()),
                        Span::raw(target.clone().unwrap_or_default()),
                    ],
                    vec![Span::styled(right, dim())],
                    cap,
                ));
            }
        }
    }
    if let (true, Some(LogEntry::Said(text))) = (ended, detail.log.last()) {
        rows.push(Line::styled("─".repeat(columns.min(RESULT_COLUMNS)), dim()));
        rows.extend(wrap(text, columns).into_iter().map(Line::raw));
    }
    Body {
        tails: true,
        ..Body::built(title, rows)
    }
}

fn diff_rows(loaded: Result<&FileDiff, &String>, width: usize) -> Vec<Line<'static>> {
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
                if line.starts_with("@@") {
                    rows.push(Line::styled(truncate_end(line, width), style));
                    continue;
                }
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
    fn hunk_headers_are_cut_with_an_ellipsis_while_code_wraps() {
        let diff = FileDiff::Text {
            lines: vec![
                "@@ -38,9 +39,15 @@ export function grossPay(shifts: Shift[]): Money {".into(),
                "+const a = something_long_enough_to_wrap;".into(),
            ],
            more: 0,
        };
        let rows = diff_rows(Ok(&diff), 30);
        let text = |row: &Line<'_>| {
            row.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert!(text(&rows[0]).ends_with('…') && text(&rows[0]).chars().count() <= 30);
        assert_eq!(rows.len(), 3, "the header is one row and the code wraps");
    }

    #[test]
    fn names_are_listed_as_far_as_they_fit_and_the_rest_counted() {
        let names: Vec<String> = ["alpha", "beta", "gamma"].map(String::from).to_vec();
        assert_eq!(names_in(&names, 40), "alpha, beta, gamma");
        assert_eq!(names_in(&names, 12), "alpha +2");
        assert_eq!(names_in(&names, 3), "3", "nothing fits, so only the count");
    }

    #[test]
    fn counts_read_with_separators() {
        assert_eq!(thousands(12_340), "12,340");
        assert_eq!(thousands(7), "7");
        assert_eq!(size(48 * 1024), "48 KB");
    }
}
