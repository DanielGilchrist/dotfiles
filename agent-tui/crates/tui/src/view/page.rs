//! The main view [0] when it reads a document instead of showing the
//! terminal.

use agent_core::activity::AgentId;
use agent_core::model::{Head, RepoKey};
use agent_core::names::{CarriageReturn, SessionName, clean};
use agent_core::ports::{
    BranchLog, Change, ChangeKind, Compared, Edit, FileDiff, FileState, Lines,
};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::fmt::Write;
use std::path::Path;
use unicode_width::UnicodeWidthChar;

use super::log;
use super::sidebar::{
    activity_message, edit_letter, fits_beside, others_on, second_tab, status_line, subagent_rows,
};
use super::{
    accent, bold, dim, error, fit, justify, line_width, spinner, success, truncate_end,
    truncate_start, warning, wrap,
};
use crate::document::{
    BranchKey, CommitKey, DiffKey, Doc, FolderDocs, FolderKey, Hover, Key, Laid, Placed, Resolved,
};
use crate::model::{ActivityFiles, AgentStatus, Asking, LogKey, MainLayout, Model};
use crate::review::{self, DiffView, LineInfo};

/// [0]'s title, what its top border says on the right, its bottom-right
/// corner and its rows.
pub(super) struct Page {
    pub title: Vec<Span<'static>>,
    pub right: Vec<Span<'static>>,
    pub corner: Vec<Span<'static>>,
    pub lines: Vec<Line<'static>>,
    /// How the document sat in its room, for the keys that move over it.
    pub laid: MainLayout,
    /// The document's rows, if this frame built them.
    pub built: Option<Laid>,
}

/// A document's rows: a few that are not from a document, or a document's
/// rows, built for this frame or `None` for the ones the document cache
/// kept from an earlier frame.
enum Rows {
    Built(Vec<Line<'static>>),
    Doc(Option<Laid>),
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
    /// A log's entries and their rows.
    entries: Vec<crate::log::EntryRows>,
    /// The log's entry the cursor is on, and which log it belongs to.
    nav: Option<LogKey>,
    /// A diff, whose lines the cursor moves over.
    diff: bool,
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
            diff: false,
        }
    }
}

/// Which of a document's rows the page shows: `height` of its `total`
/// from row `top`, below `from` rows that stay on top.
#[derive(Clone, Copy)]
struct Window {
    from: usize,
    top: usize,
    height: usize,
    total: usize,
}

/// What [0] reads instead of the terminal, in `area`, scrolled as the model
/// says, or `None` while the cursor is on nothing it reads. `border` is the
/// corner's style and `width` the whole panel's. Nothing is kept: what the
/// model should remember comes back in the [`Page`].
pub(super) fn page(m: &Model, area: Rect, width: usize, border: Style) -> Option<Page> {
    let hover = m.hover()?;
    let columns = usize::from(area.width);
    // An agent that asks takes the top row, since it needs an answer and
    // the document doesn't. It never switches [0] to the terminal itself.
    // The activity pins its own, which says more.
    let pins_own = matches!(&hover, Hover::Activity(name) if m.activity_detail(name).is_some());
    let question = m
        .selected_entry()
        .and_then(|e| match m.status(e) {
            AgentStatus::Asking { question, .. } => Some(question),
            _ => None,
        })
        .filter(|_| !pins_own);
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
        Hover::Commit(key) => commit_body(m, key, columns),
        Hover::Activity(name) => activity_body(m, name, columns, height)?,
        Hover::Subagent(name, id) => log_body(m, name, id, columns, budget),
        Hover::Repo(key) => repo_body(m, key, columns)?,
    };
    let pinned = body.pinned.len().min(height);
    let height = height - pinned;
    let total = match &body.rows {
        Rows::Built(rows) => rows.len(),
        Rows::Doc(Some(laid)) => laid.rows.len(),
        Rows::Doc(None) => m.docs.cached().len(),
    };
    let doc_lines: &[LineInfo] = match &body.rows {
        Rows::Built(_) => &[],
        Rows::Doc(Some(laid)) => &laid.lines,
        Rows::Doc(None) => m.docs.lines(),
    };
    let diff = (body.diff && matches!(body.rows, Rows::Doc(_)))
        .then(|| hover.key().map(|k| k.slot()))
        .flatten();
    let cursor = diff
        .as_ref()
        .map(|slot| m.diff_view.settled(slot, doc_lines));
    let placed = m.docs.placed(total, height, body.tails);
    let top = placed.top;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(question) = question {
        lines.extend(question_lines(&question, columns));
    }
    lines.extend(body.pinned.iter().take(pinned).cloned());
    let from = lines.len();
    match &body.rows {
        Rows::Built(rows) => lines.extend(rows.iter().skip(top).take(height).cloned()),
        Rows::Doc(Some(laid)) => lines.extend(laid.rows.iter().skip(top).take(height).cloned()),
        Rows::Doc(None) => lines.extend(m.docs.cached().iter().skip(top).take(height).cloned()),
    }
    let window = Window {
        from,
        top,
        height,
        total,
    };
    if let Some(cursor) = &cursor {
        mark_lines(m, &mut lines, doc_lines, cursor, window);
    }
    mark_entry(m, &body, &mut lines, window);

    let (title, right) = title_and_status(m, body.title, width, status_least);

    let corner = corner(m, body.loading, placed, window, border);
    Some(Page {
        title,
        right,
        corner,
        lines,
        laid: MainLayout {
            total,
            height,
            tails: body.tails,
            entries: body.entries,
            nav: body.nav,
            diff,
        },
        built: match body.rows {
            Rows::Doc(laid) => laid,
            Rows::Built(_) => None,
        },
    })
}

/// The question an agent asks, which takes the top row since it needs an
/// answer and the document doesn't.
fn question_lines(question: &str, columns: usize) -> Vec<Line<'static>> {
    let ask = "i answer";
    let room = columns.saturating_sub(ask.len() + 3);
    let text = question.lines().next().unwrap_or_default().trim();
    vec![
        justify(
            vec![
                Span::styled("◆ ", warning().add_modifier(Modifier::BOLD)),
                Span::styled(truncate_end(text, room), warning()),
            ],
            vec![Span::raw(ask)],
            columns,
        ),
        Line::raw(""),
    ]
}

/// `▌` in the gutter of the log entry the cursor is on while [0] has the
/// keys.
fn mark_entry(m: &Model, body: &Body, lines: &mut [Line<'static>], window: Window) {
    let Window {
        from, top, height, ..
    } = window;
    if let Some(key) = &body.nav
        && m.main_focused()
        && let Some(rows) = m
            .nav(key)
            .and_then(|nav| nav.cursor.as_ref())
            .and_then(|at| body.entries.iter().find(|e| &e.key == at))
    {
        for row in rows.start.max(top)..(rows.start + rows.len).min(top + height) {
            if let Some(line) = lines.get_mut(from + row - top)
                && let Some(first) = line.spans.first_mut()
            {
                *first = Span::styled("▌", accent());
            }
        }
    }
}

/// The title cut to leave room for the agent's state, and that state if it
/// fits beside the title.
fn title_and_status(
    m: &Model,
    mut title: Vec<Span<'static>>,
    width: usize,
    status_least: usize,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
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
    (title, right)
}

/// [0]'s bottom-right corner: what it is waiting for, or where the rows
/// shown sit among the document's.
fn corner(
    m: &Model,
    loading: bool,
    placed: Placed,
    window: Window,
    border: Style,
) -> Vec<Span<'static>> {
    let Window {
        top, height, total, ..
    } = window;
    let shown = format!("{}-{} of {total}", top + 1, (top + height).min(total));
    if loading || m.docs.loading(m.clock.mono) {
        vec![Span::styled("loading", dim())]
    } else if placed.following() {
        vec![Span::styled("following", border)]
    } else if placed.paused() {
        vec![Span::styled(format!("paused {shown}"), border)]
    } else if total > height {
        vec![Span::styled(shown, border)]
    } else {
        Vec::new()
    }
}

/// `▌` in the gutter of the cursor's lines while [0] has the keys, and `✎`
/// on lines the agent's draft comments on.
fn mark_lines(
    m: &Model,
    out: &mut [Line<'static>],
    infos: &[LineInfo],
    cursor: &DiffView,
    window: Window,
) {
    let Window {
        from, top, height, ..
    } = window;
    let drafts = m.selected_entry().map_or(&[][..], |e| m.draft(e.name()));
    let rev = crate::update::commit_rev(m);
    let (lo, hi) = cursor.span();
    for (at, line) in infos.iter().enumerate() {
        let selected = m.main_focused() && (lo..=hi).contains(&at);
        let commented = drafts.iter().any(|c| c.covers(line, rev.as_deref()));
        if !selected && !commented {
            continue;
        }
        let mark = if selected {
            Span::styled("▌", accent())
        } else {
            Span::styled("✎", warning())
        };
        for row in line.row.max(top)..(line.row + line.height).min(top + height) {
            if let Some(first) = out
                .get_mut(from + row - top)
                .and_then(|l| l.spans.first_mut())
            {
                *first = mark.clone();
            }
        }
    }
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
fn diff_body(m: &Model, hover: &DiffKey, columns: usize, budget: usize) -> Body {
    let rows = match m.docs.resolve() {
        Resolved::Current(key, loaded) | Resolved::Stale(key, loaded) => {
            let (key, loaded) = (key.clone(), loaded.clone());
            Rows::Doc(m.docs.laid(&key, columns, || match loaded.as_ref() {
                Ok(Doc::Diff(diff)) => diff_rows_info(&hover.change.path, Ok(diff), columns, 0),
                Ok(Doc::Branch(_) | Doc::Folder(_)) => (Vec::new(), Vec::new()),
                Err(e) => diff_rows_info(&hover.change.path, Err(e), columns, 0),
            }))
        }
        Resolved::Missing => Rows::Built(Vec::new()),
    };
    let mut counts = Vec::new();
    match hover.change.kind {
        ChangeKind::Tracked {
            lines: Lines::Counted { added, removed },
            ..
        } => {
            for (count, text, style) in [
                (added, format!(" +{added}"), success()),
                (removed, format!(" −{removed}"), error()),
            ] {
                if count > 0 {
                    counts.push(Span::styled(text, style));
                }
            }
        }
        ChangeKind::Tracked {
            lines: Lines::Binary,
            ..
        } => counts.push(Span::styled(" bin", dim())),
        ChangeKind::Tracked {
            lines: Lines::Unknown,
            ..
        } => {}
        ChangeKind::Untracked => counts.push(Span::styled(" new", dim())),
    }
    let others = others_on(m, &hover.name, &hover.change.path);
    let fixed = 4 + line_width(&counts);
    let path_room = budget.saturating_sub(fixed).max(12);
    let path = truncate_start(&hover.change.path.to_string_lossy(), path_room);
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
        diff: true,
    }
}

/// A folder: a compact list of the files changed under it with their
/// markers and counts, then each file's diff under its own header, in the
/// tree's order. The list comes from the git facts already read, so it shows
/// at once. The diffs follow when they have been read.
fn folder_body(m: &Model, hover: &FolderKey, columns: usize, budget: usize) -> Body {
    let rows = match m.docs.resolve() {
        Resolved::Current(key, loaded) | Resolved::Stale(key, loaded) => {
            let (key, loaded) = (key.clone(), loaded.clone());
            Rows::Doc(
                m.docs
                    .laid(&key, columns, || match (&key, loaded.as_ref()) {
                        (Key::Folder(folder), Ok(Doc::Folder(docs))) => {
                            folder_rows(folder, Some(Ok(docs)), columns)
                        }
                        (Key::Folder(folder), Err(e)) => folder_rows(folder, Some(Err(e)), columns),
                        _ => (Vec::new(), Vec::new()),
                    }),
            )
        }
        Resolved::Missing => Rows::Built(folder_rows(hover, None, columns).0),
    };
    let (mut added, mut removed) = (0, 0);
    for change in &hover.files {
        if let ChangeKind::Tracked {
            lines:
                Lines::Counted {
                    added: a,
                    removed: r,
                },
            ..
        } = change.kind
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
        diff: true,
        ..Body::built(title, Vec::new())
    }
}

pub(super) fn count_spans(kind: ChangeKind) -> Vec<Span<'static>> {
    match kind {
        ChangeKind::Tracked {
            lines: Lines::Counted { added, removed },
            ..
        } => {
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
        ChangeKind::Tracked {
            lines: Lines::Binary,
            ..
        } => vec![Span::styled("bin", dim())],
        ChangeKind::Tracked {
            lines: Lines::Unknown,
            ..
        } => Vec::new(),
        ChangeKind::Untracked => vec![Span::styled("new", dim())],
    }
}

/// The folder's file list, then a section per file read so far.
fn folder_rows(
    folder: &FolderKey,
    docs: Option<Result<&FolderDocs, &String>>,
    columns: usize,
) -> (Vec<Line<'static>>, Vec<LineInfo>) {
    section_rows(&folder.files, &folder.path, docs, columns)
}

/// `files` listed, then a section per file of `docs`. Paths in the list are
/// shown relative to `base`.
fn section_rows(
    files: &[Change],
    base: &Path,
    docs: Option<Result<&FolderDocs, &String>>,
    columns: usize,
) -> (Vec<Line<'static>>, Vec<LineInfo>) {
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut infos: Vec<LineInfo> = Vec::new();
    for change in files {
        let rel = change
            .path
            .strip_prefix(base)
            .unwrap_or(&change.path)
            .to_string_lossy()
            .into_owned();
        let mut left = super::sidebar::file_marker(change.state());
        let right = count_spans(change.kind);
        let room = columns.saturating_sub(line_width(&left) + line_width(&right) + 1);
        left.push(Span::raw(truncate_start(&rel, room)));
        rows.push(justify(left, right, columns));
    }
    let docs = match docs {
        Some(Ok(docs)) => docs,
        Some(Err(e)) => {
            rows.push(Line::raw(""));
            rows.extend(diff_rows(Err(e), columns));
            return (rows, infos);
        }
        None => return (rows, infos),
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
        let counts = count_spans(file.change.kind);
        if !counts.is_empty() {
            head.push(Span::raw("  "));
            head.extend(counts);
        }
        rows.push(Line::from(head));
        let (diff, lines) =
            diff_rows_info(&file.change.path, file.diff.as_ref(), columns, rows.len());
        rows.extend(diff);
        infos.extend(lines);
    }
    if docs.more_files > 0 {
        rows.push(Line::raw(""));
        rows.push(Line::styled(
            format!("+{} more files, open one to see its diff", docs.more_files),
            dim(),
        ));
    }
    (rows, infos)
}

/// A commit opened from the branch view: the files it changed, then each
/// file's diff against its parent.
fn commit_body(m: &Model, hover: &CommitKey, columns: usize) -> Body {
    let rows = match m.docs.resolve() {
        Resolved::Current(key, loaded) | Resolved::Stale(key, loaded) => {
            let (key, loaded) = (key.clone(), loaded.clone());
            Rows::Doc(m.docs.laid(&key, columns, || match loaded.as_ref() {
                Ok(Doc::Folder(docs)) => {
                    let files: Vec<Change> = docs.files.iter().map(|f| f.change.clone()).collect();
                    section_rows(&files, Path::new(""), Some(Ok(docs)), columns)
                }
                Err(e) => section_rows(&[], Path::new(""), Some(Err(e)), columns),
                Ok(_) => (Vec::new(), Vec::new()),
            }))
        }
        Resolved::Missing => Rows::Built(Vec::new()),
    };
    let short: String = hover.sha.chars().take(8).collect();
    let title = vec![
        Span::raw("[0] "),
        Span::styled(format!("commit {short}"), bold()),
        Span::styled(format!(" {}", hover.subject), dim()),
    ];
    Body {
        rows,
        diff: true,
        ..Body::built(title, Vec::new())
    }
}

/// The branch: how far it is from shipping, its commits and the files not
/// yet committed.
fn branch_body(m: &Model, key: &BranchKey, columns: usize) -> Body {
    let title = vec![
        Span::raw("[0] "),
        Span::styled(second_tab(&key.compared), bold()),
        Span::styled(format!(" · {}", compared_title(&key.compared)), dim()),
    ];
    let compared = key.compared.clone();
    let behind = key.behind;
    let rows = match m.docs.resolve() {
        Resolved::Current(key, loaded) | Resolved::Stale(key, loaded) => {
            let (key, loaded) = (key.clone(), loaded.clone());
            Rows::Doc(m.docs.laid(&key, columns, || match loaded.as_ref() {
                Ok(Doc::Branch(log)) => branch_rows(log, &compared, behind, columns),
                Ok(Doc::Diff(_) | Doc::Folder(_)) => (Vec::new(), Vec::new()),
                Err(e) => (
                    vec![Line::styled(
                        e.lines().next().unwrap_or_default().to_string(),
                        error(),
                    )],
                    Vec::new(),
                ),
            }))
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
        diff: true,
    }
}

/// A branch log as rows: its commits, then the files not committed, each
/// after a gutter for the cursor, and a line for each commit and file.
fn branch_rows(
    log: &BranchLog,
    compared: &Compared,
    behind: usize,
    columns: usize,
) -> (Vec<Line<'static>>, Vec<LineInfo>) {
    let gutter = || Span::raw(" ");
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut items: Vec<LineInfo> = Vec::new();
    let item =
        |row: usize, height: usize, path: &Path, kind: review::LineKind, text: &str| LineInfo {
            row,
            height,
            path: path.to_path_buf(),
            kind,
            old: None,
            new: None,
            text: text.to_string(),
            edit: std::num::NonZeroUsize::MIN,
        };
    if log.commits.is_empty() && log.uncommitted.is_empty() {
        rows.push(Line::from(vec![
            gutter(),
            Span::styled("nothing ahead of the default branch", dim()),
        ]));
    }
    if !log.commits.is_empty() {
        rows.push(Line::from(vec![
            gutter(),
            Span::styled(format!("Commits ({})", log.commits.len()), dim()),
        ]));
        for commit in &log.commits {
            let (hash, subject) = commit.split_once(' ').unwrap_or((commit, ""));
            let room = columns.saturating_sub(hash.len() + 2);
            let cut = hard_wrap(subject, room);
            items.push(item(
                rows.len(),
                cut.len(),
                Path::new(""),
                review::LineKind::Commit,
                commit,
            ));
            for (i, row) in cut.into_iter().enumerate() {
                let lead = if i == 0 {
                    hash.to_string()
                } else {
                    " ".repeat(hash.len())
                };
                rows.push(Line::from(vec![
                    gutter(),
                    Span::styled(lead, dim()),
                    Span::raw(format!(" {row}")),
                ]));
            }
        }
        rows.push(Line::raw(""));
    }
    if !log.uncommitted.is_empty() {
        rows.push(Line::from(vec![
            gutter(),
            Span::styled(format!("Uncommitted ({})", log.uncommitted.len()), dim()),
        ]));
        for file in &log.uncommitted {
            let (code, path) = (status_code(file.state), &file.path);
            items.push(item(
                rows.len(),
                1,
                path,
                review::LineKind::Entry(file.state),
                &code,
            ));
            rows.push(Line::from(vec![
                gutter(),
                Span::styled(format!("{code} "), dim()),
                Span::raw(truncate_start(
                    &path.to_string_lossy(),
                    columns.saturating_sub(code.len() + 2),
                )),
            ]));
        }
    }
    if let (true, Some(base)) = (behind > 0, compared.base()) {
        rows.push(Line::raw(""));
        rows.push(Line::from(vec![
            gutter(),
            Span::styled(format!("behind {base} by {behind}"), dim()),
        ]));
    }
    (rows, items)
}

fn status_code(state: FileState) -> String {
    let letter = |edit: Option<Edit>| edit.map_or(" ", edit_letter);
    match state {
        FileState::Untracked => "??".to_string(),
        FileState::Conflicted => "UU".to_string(),
        state => format!("{}{}", letter(state.staged()), letter(state.unstaged())),
    }
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
        let work = m.facts(entry.name());
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

/// The activity: its log when the harness has one, and otherwise the whole
/// prompt, the whole message or question, and the subagents.
fn activity_body(m: &Model, name: &SessionName, columns: usize, height: usize) -> Option<Body> {
    let (_, entry) = m.snapshot()?.entry(name)?;
    let activity = m.activity(name)?;
    let title = vec![Span::raw("[0] "), Span::styled("Activity", bold())];
    let watch = m.tracked(name).map(|t| &t.activity_view);
    let Some(detail) = m.activity_detail(name) else {
        let failed = match watch.and_then(|w| w.watch.log.as_ref()) {
            Some(Err(e)) => e.lines().next().map(str::to_string),
            _ => None,
        };
        return Some(plain_activity_body(
            m, entry, title, failed, columns, height,
        ));
    };
    let closing = m.closing(name);
    let asking = m.asking(name);
    let files = m.activity_files(name);
    let view = log::ActivityView {
        state: activity_state(m, activity, asking.as_ref()),
        closing: closing.as_deref(),
        looked: watch.and_then(|w| w.looked.seen),
        height,
        touched: files.map_or(&[], |f| &f.touched),
        listed: files.map_or(&[], ActivityFiles::listed),
    };
    let roots = m.roots(name);
    let home = std::env::var("HOME").ok();
    let nav = watch.map(|w| w.watch.nav.clone()).unwrap_or_default();
    let rendered = log::render_activity(
        detail,
        &view,
        &nav,
        &log::Context {
            columns,
            now: m.clock.wall,
            spinner: spinner(m.clock.wall),
            roots: roots.as_ref(),
            home: home.as_deref(),
        },
    );
    let mut rows = rendered.rows;
    let queue = queue_rows(m, name, columns);
    if !queue.is_empty() {
        rows.push(Line::raw(""));
        rows.extend(queue);
    }
    Some(Body {
        tails: !rendered.finished,
        pinned: rendered.pinned,
        entries: rendered.layout,
        nav: Some(LogKey::Activity(name.clone())),
        ..Body::built(title, rows)
    })
}

/// What the status row says of the agent's state.
fn activity_state<'a>(
    m: &Model,
    activity: &'a agent_core::activity::Activity,
    asking: Option<&'a Asking>,
) -> log::ActivityStage<'a> {
    use agent_core::activity::State;
    let stale = activity.stale(m.clock.wall);
    if let Some(ask) = activity.waiting() {
        let by = match asking {
            Some(Asking::Subagent(id)) => activity.subagents.get(id).map(|s| {
                s.description
                    .clone()
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| s.kind.clone())
            }),
            _ => None,
        };
        return log::ActivityStage::Asking {
            since: ask.since,
            question: &ask.question,
            asking,
            by,
        };
    }
    match &activity.state {
        State::Working { .. } => log::ActivityStage::Working {
            since: activity.since,
            stale,
            running: activity.running(),
        },
        State::Delegating => log::ActivityStage::Delegating {
            running: activity.running(),
            quiet: m
                .clock
                .wall
                .duration_since(activity.heard)
                .unwrap_or_default(),
            stale,
        },
        State::Failed { reason } => log::ActivityStage::Failed {
            reason,
            since: activity.since,
        },
        State::Done { .. } => log::ActivityStage::Done { seen: false },
        State::Idle | State::Exited => log::ActivityStage::Done { seen: true },
    }
}

/// `queued: …` rows for messages waiting to be typed into the agent.
fn queue_rows(m: &Model, name: &SessionName, columns: usize) -> Vec<Line<'static>> {
    let Some(queue) = m.tracked(name).and_then(|t| t.outbox.as_ref()) else {
        return Vec::new();
    };
    let mut rows: Vec<Line<'static>> = queue
        .iter()
        .map(|q| {
            Line::styled(
                truncate_end(
                    &format!("queued: {}", crate::update::first_words(q.as_ref())),
                    columns,
                ),
                dim(),
            )
        })
        .collect();
    if queue.stalled(m.activity(name).map(|a| a.events), m.clock.mono) {
        rows.push(Line::styled(
            truncate_end(&format!("queued · waiting for {name} to report"), columns),
            dim(),
        ));
    }
    rows
}

/// The activity as the hooks tell it, for a harness with no transcript to read:
/// the whole prompt, the whole message or question, and the subagents.
fn plain_activity_body(
    m: &Model,
    entry: &agent_core::model::Entry,
    title: Vec<Span<'static>>,
    failed: Option<String>,
    columns: usize,
    height: usize,
) -> Body {
    let name = entry.name();
    let mut rows: Vec<Line<'static>> = Vec::new();
    if let Some(failed) = failed {
        rows.push(Line::styled(truncate_end(&failed, columns), error()));
        rows.push(Line::raw(""));
    }
    if let Some(prompt) = m.activity(name).and_then(|a| a.prompt.as_ref()) {
        for (i, row) in wrap(prompt, columns.saturating_sub(2))
            .into_iter()
            .enumerate()
        {
            let lead = if i == 0 { "› " } else { "  " };
            rows.push(Line::styled(format!("{lead}{row}"), dim()));
        }
        rows.push(Line::raw(""));
    }
    if let Some(message) = activity_message(m, entry) {
        rows.extend(wrap(&message, columns).into_iter().map(Line::raw));
        rows.push(Line::raw(""));
    }
    let queue = queue_rows(m, name, columns);
    if !queue.is_empty() {
        rows.extend(queue);
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
    if rows.is_empty() {
        rows = empty_activity(columns, height);
    }
    Body::built(title, rows)
}

/// The one DIM line an activity that hasn't started shows, in the middle.
fn empty_activity(columns: usize, height: usize) -> Vec<Line<'static>> {
    let text = "no activity yet · s send";
    let pad = columns.saturating_sub(text.chars().count()) / 2;
    let mut rows = vec![Line::raw(""); height.saturating_sub(1) / 2];
    rows.push(Line::styled(format!("{}{text}", " ".repeat(pad)), dim()));
    rows
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
    let detail = match m.watched(&key).and_then(|w| w.watch.log.as_ref()) {
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
    let roots = m.roots(name);
    let home = std::env::var("HOME").ok();
    let nav = m
        .watched(&key)
        .map(|w| w.watch.nav.clone())
        .unwrap_or_default();
    let log_key = LogKey::Subagent(key);
    let rendered = log::render(
        detail,
        subagent,
        &nav,
        &log::Context {
            columns,
            now: m.clock.wall,
            spinner: spinner(m.clock.wall),
            roots: roots.as_ref(),
            home: home.as_deref(),
        },
    );
    Body {
        tails: !rendered.finished,
        pinned: rendered.pinned,
        entries: rendered.layout,
        nav: Some(log_key),
        ..Body::built(title, rendered.rows)
    }
}

fn diff_rows(loaded: Result<&FileDiff, &String>, width: usize) -> Vec<Line<'static>> {
    diff_rows_info(Path::new(""), loaded, width, 0).0
}

/// A diff's rows, each after a one column gutter for the cursor, and what
/// each of its lines is. `offset` is the document row the first one lands on.
fn diff_rows_info(
    path: &Path,
    loaded: Result<&FileDiff, &String>,
    width: usize,
    offset: usize,
) -> (Vec<Line<'static>>, Vec<LineInfo>) {
    let gutter = || Span::raw(" ");
    let note = |text: String, style: Style| {
        (
            vec![Line::from(vec![gutter(), Span::styled(text, style)])],
            Vec::new(),
        )
    };
    let width = width.saturating_sub(1).max(1);
    match loaded {
        Err(e) => note(e.lines().next().unwrap_or_default().to_string(), error()),
        Ok(FileDiff::Binary { bytes }) => note(
            match bytes {
                Some(bytes) => format!("binary · {}", size(*bytes)),
                None => "binary".to_string(),
            },
            dim(),
        ),
        Ok(FileDiff::Text { lines, more }) => {
            let mut rows: Vec<Line<'static>> = Vec::new();
            let mut infos = Vec::new();
            for (line, (kind, old, new, edit)) in lines.iter().zip(review::classify(lines)) {
                let style = match line.chars().next() {
                    Some('+') => success(),
                    Some('-') => error(),
                    Some('@') => dim(),
                    _ => Style::new(),
                };
                let cut = if line.starts_with("@@") {
                    vec![truncate_end(line, width)]
                } else {
                    hard_wrap(&line.replace('\t', "    "), width)
                };
                infos.push(LineInfo {
                    row: offset + rows.len(),
                    height: cut.len(),
                    path: path.to_path_buf(),
                    kind,
                    old,
                    new,
                    text: line.clone(),
                    edit,
                });
                rows.extend(
                    cut.into_iter()
                        .map(|row| Line::from(vec![gutter(), Span::styled(row, style)])),
                );
            }
            if *more > 0 {
                rows.push(Line::from(vec![
                    gutter(),
                    Span::styled(format!("{} more lines · e open", thousands(*more)), dim()),
                ]));
            } else if rows.is_empty() {
                rows.push(Line::from(vec![
                    gutter(),
                    Span::styled("no changes to show", dim()),
                ]));
            }
            (rows, infos)
        }
    }
}

/// `text` cut into rows of at most `width` columns, exactly where the
/// columns run out, so a diff's spacing survives. Control characters go.
fn hard_wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut used = 0;
    let text = clean(text, CarriageReturn::Drop, None);
    for c in text.chars().filter(|&c| c != '\n' && c != '\t') {
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
