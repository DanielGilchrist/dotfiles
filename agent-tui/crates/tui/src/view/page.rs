//! The main view [0] when it reads a document instead of showing the
//! terminal.

use std::fmt::Write;
use std::time::Instant;

use agent_core::activity::AgentId;
use agent_core::model::{Head, RepoKey};
use agent_core::names::SessionName;
use agent_core::ports::{BranchLog, Compared, FileDiff, Lines};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use super::log;
use super::sidebar::{fits_beside, others_on, status_line, subagent_rows, turn_message};
use super::{
    accent, bold, dim, error, fit, justify, line_width, spinner, success, truncate_end,
    truncate_start, warning, wrap,
};
use crate::document::{BranchKey, DiffKey, Doc, FolderDocs, FolderKey, Hover, Key, Resolved};
use crate::model::{AgentStatus, Model, SubagentKey};

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
    /// Rows that stay on top while the rest scrolls.
    pinned: Vec<Line<'static>>,
    /// A log's entries: each one's first row and row count.
    entries: Vec<(usize, usize)>,
    /// The log's entry the cursor is on, and which subagent it belongs to.
    nav: Option<SubagentKey>,
}

impl Body {
    fn built(title: Vec<Span<'static>>, rows: Vec<Line<'static>>) -> Self {
        Self {
            title,
            rows: Rows::Built(rows),
            tails: false,
            loading: false,
            pinned: Vec::new(),
            entries: Vec::new(),
            nav: None,
        }
    }
}

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
        Hover::Subagent(name, id) => log_body(m, name, id, columns, budget),
        Hover::Repo(key) => repo_body(m, key, columns)?,
    };
    let pinned = body.pinned.len().min(height);
    let height = height - pinned;
    let total = match &body.rows {
        Rows::Built(rows) => rows.len(),
        Rows::Cached => m.docs.cached().len(),
    };
    if let Some(key) = &body.nav {
        reveal(m, key, &body.entries, height);
    }
    let top = m.docs.place(total, height, body.tails);
    if let Some(key) = &body.nav {
        follow_page(m, key, &body.entries, top);
    }
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
    lines.extend(body.pinned.iter().take(pinned).cloned());
    let from = lines.len();
    match &body.rows {
        Rows::Built(rows) => lines.extend(rows.iter().skip(top).take(height).cloned()),
        Rows::Cached => lines.extend(m.docs.cached().iter().skip(top).take(height).cloned()),
    }
    if let Some(key) = &body.nav
        && m.main_focused()
        && let Some(&(start, len)) = m
            .log_view
            .navs
            .get(key)
            .and_then(|nav| nav.cursor)
            .and_then(|at| body.entries.get(at))
    {
        for row in start.max(top)..(start + len).min(top + height) {
            if let Some(line) = lines.get_mut(from + row - top)
                && let Some(first) = line.spans.first_mut()
            {
                *first = Span::styled("▌", accent());
            }
        }
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

/// Scrolls so the cursor's entry shows, once, after a key moved it.
fn reveal(m: &mut Model, key: &SubagentKey, entries: &[(usize, usize)], height: usize) {
    if !std::mem::take(&mut m.log_view.reveal) {
        return;
    }
    let Some(&(start, len)) = m
        .log_view
        .navs
        .get(key)
        .and_then(|nav| nav.cursor)
        .and_then(|at| entries.get(at))
    else {
        return;
    };
    let top = m.docs.scroll;
    if start < top {
        m.docs.scroll = start;
    } else if start + len > top + height {
        m.docs.scroll = (start + len).saturating_sub(height).min(start);
    }
}

/// Moves the cursor onto the first entry on screen after a page scroll.
fn follow_page(m: &mut Model, key: &SubagentKey, entries: &[(usize, usize)], top: usize) {
    if !std::mem::take(&mut m.log_view.follow_page) {
        return;
    }
    let at = entries
        .iter()
        .position(|&(start, len)| start + len > top)
        .unwrap_or(entries.len().saturating_sub(1));
    m.log_view.navs.entry(key.clone()).or_default().cursor = Some(at);
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
                Ok(Doc::Branch(_) | Doc::Folder(_)) => Vec::new(),
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
        pinned: Vec::new(),
        entries: Vec::new(),
        nav: None,
    }
}

/// A folder: a compact list of the files changed under it with their
/// markers and counts, then each file's diff under its own header, in the
/// tree's order. The list comes from the git facts already read, so it shows
/// at once. The diffs follow when they have been read.
fn folder_body(m: &mut Model, hover: &FolderKey, columns: usize, budget: usize) -> Body {
    let rows = match m.docs.resolve() {
        Resolved::Current(key, loaded) | Resolved::Stale(key, loaded) => {
            let (key, loaded) = (key.clone(), loaded.clone());
            m.docs
                .rows(&key, columns, || match (&key, loaded.as_ref()) {
                    (Key::Folder(folder), Ok(Doc::Folder(docs))) => {
                        folder_rows(folder, Some(Ok(docs)), columns)
                    }
                    (Key::Folder(folder), Err(e)) => folder_rows(folder, Some(Err(e)), columns),
                    _ => Vec::new(),
                });
            Rows::Cached
        }
        Resolved::Missing => Rows::Built(folder_rows(hover, None, columns)),
    };
    let (mut added, mut removed) = (0, 0);
    for change in &hover.files {
        if let Lines::Counted {
            added: a,
            removed: r,
        } = change.lines
        {
            added += a;
            removed += r;
        }
    }
    let mut title = vec![
        Span::raw("[0] "),
        Span::styled(
            truncate_start(
                &format!("{}/", hover.path.display()),
                budget.saturating_sub(24).max(12),
            ),
            bold(),
        ),
        Span::styled(format!(" {} files", hover.files.len()), dim()),
    ];
    for (count, text, style) in [
        (added, format!(" +{added}"), success()),
        (removed, format!(" −{removed}"), error()),
    ] {
        if count > 0 {
            title.push(Span::styled(text, style));
        }
    }
    Body {
        rows,
        ..Body::built(title, Vec::new())
    }
}

fn count_spans(lines: Lines) -> Vec<Span<'static>> {
    match lines {
        Lines::Counted { added, removed } => {
            let mut spans = Vec::new();
            if added > 0 {
                spans.push(Span::styled(format!("+{added}"), success()));
            }
            if removed > 0 {
                if !spans.is_empty() {
                    spans.push(Span::raw(" "));
                }
                spans.push(Span::styled(format!("−{removed}"), error()));
            }
            spans
        }
        Lines::Binary => vec![Span::styled("bin", dim())],
        Lines::Untracked => vec![Span::styled("new", dim())],
    }
}

/// The folder's file list, then a section per file read so far.
fn folder_rows(
    folder: &FolderKey,
    docs: Option<Result<&FolderDocs, &String>>,
    columns: usize,
) -> Vec<Line<'static>> {
    let mut rows: Vec<Line<'static>> = Vec::new();
    for change in &folder.files {
        let rel = change
            .path
            .strip_prefix(&folder.path)
            .unwrap_or(&change.path)
            .to_string_lossy()
            .into_owned();
        let mut left = super::sidebar::file_marker(change.state);
        let right = count_spans(change.lines);
        let room = columns.saturating_sub(line_width(&left) + line_width(&right) + 1);
        left.push(Span::raw(truncate_start(&rel, room)));
        rows.push(justify(left, right, columns));
    }
    let docs = match docs {
        Some(Ok(docs)) => docs,
        Some(Err(e)) => {
            rows.push(Line::raw(""));
            rows.extend(diff_rows(Err(e), columns));
            return rows;
        }
        None => return rows,
    };
    for file in &docs.files {
        rows.push(Line::raw(""));
        rows.push(Line::styled("─".repeat(columns), dim()));
        let mut head = vec![Span::styled(
            truncate_start(
                &file.change.path.to_string_lossy(),
                columns.saturating_sub(12).max(12),
            ),
            bold(),
        )];
        let counts = count_spans(file.change.lines);
        if !counts.is_empty() {
            head.push(Span::raw("  "));
            head.extend(counts);
        }
        rows.push(Line::from(head));
        rows.extend(diff_rows(file.diff.as_ref(), columns));
    }
    if docs.more_files > 0 {
        rows.push(Line::raw(""));
        rows.push(Line::styled(
            format!("+{} more files, open one to see its diff", docs.more_files),
            dim(),
        ));
    }
    rows
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
                Ok(Doc::Diff(_) | Doc::Folder(_)) => Vec::new(),
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
        pinned: Vec::new(),
        entries: Vec::new(),
        nav: None,
    }
}

/// A branch log as rows: its commits, then the files not committed.
fn branch_rows(
    log: &BranchLog,
    compared: &Compared,
    behind: usize,
    columns: usize,
) -> Vec<Line<'static>> {
    let mut rows: Vec<Line<'static>> = Vec::new();
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

/// A subagent's log: its status, what it was told, a row per call and what
/// it handed back.
fn log_body(m: &Model, name: &SessionName, id: &AgentId, columns: usize, budget: usize) -> Body {
    let subagent = m
        .subagent_rows()
        .into_iter()
        .find(|(held, _)| *held == id)
        .map(|(_, s)| s);
    let mut title = vec![Span::raw("[0] ")];
    match subagent {
        Some(s) => match s.description.as_deref().filter(|d| !d.is_empty()) {
            Some(description) => {
                title.push(Span::styled(description.to_string(), bold()));
                let kind = format!(" · {}", s.kind);
                if line_width(&title) + kind.chars().count() <= budget {
                    title.push(Span::styled(kind, dim()));
                }
            }
            None => title.push(Span::styled(s.kind.clone(), bold())),
        },
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
    let checkout = m
        .selected_entry()
        .and_then(|e| e.path())
        .map(|p| p.to_string_lossy().into_owned());
    let home = std::env::var("HOME").ok();
    let nav = m.log_view.navs.get(&key).cloned().unwrap_or_default();
    let rendered = log::render(
        detail,
        subagent,
        &nav,
        &log::Context {
            columns,
            now: m.now,
            spinner: spinner(),
            checkout: checkout.as_deref(),
            home: home.as_deref(),
        },
    );
    Body {
        tails: !rendered.finished,
        pinned: rendered.pinned,
        entries: rendered.layout,
        nav: Some(key),
        ..Body::built(title, rendered.rows)
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
