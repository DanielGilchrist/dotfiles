use std::path::Path;
use std::time::SystemTime;

use agent_core::actions::Kind;
use agent_core::config::Keys;
use agent_core::model::{Entry, EntryKind, Head};
use agent_core::names::SessionName;
use agent_core::ops::Deletes;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::model::{
    AgentStatus, Blocked, Confirm, Derived, Doing, Enter, Field, Focus, Fold, Launchable, Load,
    Model, NameCheck, NameSource, NamingState, NewAgent, OfferEffect, Overlay, Pane, Phase, Place,
    Proposal, Pulse, Query, Reach, RepoInput, Row, StatusKind, Submission, View, Waiting,
    fuzzy_match, invalid_chars, repo_matches,
};
use crate::update::{picker_matches, switcher_matches};

const SIDEBAR_MAX: u16 = 30;
const SIDEBAR_MIN: u16 = 16;
const ACCENT: Color = Color::Cyan;

const TASK_ROWS: usize = 6;
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
/// Below this width the agent needs every column; Space still switches agents.
const NARROW: u16 = 60;
/// Below this many rows the peek gives its space back to the list.
const PEEK_MIN_BODY: u16 = 15;
/// The list keeps at least this many rows when the peek shows.
const LIST_MIN_ROWS: u16 = 5;
/// A name keeps at least this many columns before row details give way.
const NAME_MIN: usize = 8;
/// The NAV and switcher sections of the help; the AGENT section names the
/// configured keys.
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        " NAV ",
        &[
            ("⏎ l", "attach, resurrect or start"),
            ("Esc i", "back to the current agent"),
            ("j k", "move"),
            ("g G", "first / last"),
            ("[ ]", "previous / next repo"),
            ("Space /", "switch agent"),
            ("p", "pick repo"),
            ("Tab", "focus ↔ overview"),
            ("1-9", "pinned agents"),
            ("m", "pin / unpin"),
            ("n", "new agent"),
            ("o", "open in $EDITOR"),
            ("d", "kill session"),
            ("D", "remove with worktree and branch"),
            ("x", "run a configured action"),
            ("a", "next agent that needs you"),
            ("q", "quit"),
        ],
    ),
    (
        " SWITCH ",
        &[("⌃N ⌃P", "move"), ("⏎", "switch"), ("Esc", "close")],
    ),
    (
        " STATES ",
        &[
            ("◆", "asking you"),
            ("!", "failed"),
            ("✓", "done, unseen"),
            ("⠹", "working"),
            ("⠹?", "gone quiet"),
            ("●", "idle"),
            ("○", "exited"),
            ("·", "no session"),
        ],
    ),
];

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// Truncate keeping the start, measured in display width.
fn truncate_end(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

/// Truncate keeping the end, for paths.
fn truncate_start(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut kept = Vec::new();
    let mut used = 1;
    for c in text.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        kept.push(c);
    }
    std::iter::once('…').chain(kept.into_iter().rev()).collect()
}

fn line_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// `spans` cut to `room` columns, truncating the span that crosses the edge.
fn fit(mut spans: Vec<Span<'_>>, room: usize) -> Vec<Span<'_>> {
    let mut used = 0;
    let mut keep = spans.len();
    for (i, span) in spans.iter_mut().enumerate() {
        let w = span.width();
        if used + w > room {
            *span = Span::styled(truncate_end(&span.content, room - used), span.style);
            keep = i + 1;
            break;
        }
        used += w;
    }
    spans.truncate(keep);
    spans
}

/// Left spans, then right spans flush to `width`; the left side yields first.
fn justify<'a>(left: Vec<Span<'a>>, right: Vec<Span<'a>>, width: usize) -> Line<'a> {
    let right_width = line_width(&right);
    let mut left = fit(
        left,
        width.saturating_sub(right_width + usize::from(right_width > 0)),
    );
    let pad = width.saturating_sub(line_width(&left) + right_width);
    left.push(Span::raw(" ".repeat(pad)));
    left.extend(right);
    Line::from(left)
}

/// One glyph per state: a spinner that stops when the agent goes quiet, and
/// attention glyphs that read without colour.
fn glyph(status: &AgentStatus) -> Span<'static> {
    match status {
        AgentStatus::NoSession => Span::styled("·", dim()),
        AgentStatus::Exited => Span::styled("○", dim()),
        AgentStatus::Idle => Span::raw("●"),
        AgentStatus::Working {
            pulse: Pulse::Steady,
            ..
        } => Span::raw(spinner()),
        AgentStatus::Working {
            pulse: Pulse::Stale,
            ..
        } => Span::styled(SPINNER[0], dim()),
        AgentStatus::Asking { .. } => Span::styled(
            "◆",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        AgentStatus::Failed { .. } => Span::styled(
            "!",
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        AgentStatus::Done { .. } => Span::styled(
            "✓",
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
    }
}

/// Agents waiting on the user read BOLD, like unread mail. Dead ones DIM.
fn name_style(status: &AgentStatus, selected: bool) -> Style {
    let base = match status {
        AgentStatus::NoSession | AgentStatus::Exited => dim(),
        AgentStatus::Asking { .. } | AgentStatus::Failed { .. } | AgentStatus::Done { .. } => {
            Style::new().add_modifier(Modifier::BOLD)
        }
        AgentStatus::Idle | AgentStatus::Working { .. } => Style::new(),
    };
    if selected {
        base.add_modifier(Modifier::BOLD)
    } else {
        base
    }
}

/// `38s`, `4m`, `2h`, `3d`.
fn ago(since: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(since).unwrap_or_default().as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// How long it's been in its state, for states where that matters. A
/// working agent that's gone quiet gets a `?`.
fn time_in_state(status: &AgentStatus, now: SystemTime) -> Option<Span<'static>> {
    match status {
        AgentStatus::Working {
            since,
            pulse: Pulse::Stale,
            ..
        } => Some(Span::styled(format!("{}?", ago(*since, now)), dim())),
        AgentStatus::Asking { since, .. } => Some(Span::styled(
            ago(*since, now),
            Style::new().fg(Color::Yellow),
        )),
        AgentStatus::Working {
            since,
            pulse: Pulse::Steady,
            ..
        }
        | AgentStatus::Failed { since, .. }
        | AgentStatus::Done { since, .. } => Some(Span::styled(ago(*since, now), dim())),
        AgentStatus::NoSession | AgentStatus::Exited | AgentStatus::Idle => None,
    }
}

fn hint<'a>(key: &'a str, desc: &'a str) -> [Span<'a>; 3] {
    [
        Span::raw(key),
        Span::styled(format!(" {desc}"), dim()),
        Span::raw("  "),
    ]
}

fn hints<'a>(pairs: &[(&'a str, &'a str)]) -> Vec<Span<'a>> {
    let mut spans: Vec<Span<'a>> = pairs.iter().flat_map(|(k, d)| hint(k, d)).collect();
    spans.pop();
    spans
}

/// Keeps `selected` at least `margin` rows from either edge of a `height`
/// tall viewport, moving `offset` only when it must.
fn scroll(offset: usize, selected: usize, height: usize, len: usize) -> usize {
    let margin = 2.min(height.saturating_sub(1) / 2);
    let offset = if selected < offset + margin {
        selected.saturating_sub(margin)
    } else if selected + margin >= offset + height {
        selected + margin + 1 - height
    } else {
        offset
    };
    offset.min(len.saturating_sub(height))
}

fn badge(text: &'static str) -> Span<'static> {
    Span::styled(
        text,
        Style::new()
            .fg(ACCENT)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD),
    )
}

fn display_path(path: &Path) -> String {
    match agent_core::paths::home()
        .ok()
        .and_then(|h| path.strip_prefix(h).ok().map(Path::to_path_buf))
    {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// lazygit-style: two titled, rounded panels (the agents, and the selected
/// agent's terminal) with the focused one's border in Cyan, over a one-line
/// status bar carrying the mode badge and context hints.
pub fn draw(frame: &mut Frame<'_>, m: &mut Model) {
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let borders = 2;
    let sidebar_width = if body.width < NARROW {
        0
    } else {
        SIDEBAR_MAX.min(body.width / 4).max(SIDEBAR_MIN) + borders
    };
    let [left, right] =
        Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(0)]).areas(body);
    let (sidebar_style, pane_style) = focus_styles(m);

    if sidebar_width > 0 {
        let peek = peek(m, usize::from(left.width).saturating_sub(4));
        let height = peek
            .as_ref()
            .map_or(0, |p| peek_height(body.height, p.lines.len()));
        let [list_area, peek_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(height)]).areas(left);
        let title = fit(sidebar_title(m), usize::from(left.width).saturating_sub(4));
        let sidebar = panel(
            title,
            sidebar_count(m, sidebar_style),
            sidebar_style,
            sidebar_focused(m),
        );
        let list = sidebar.inner(list_area);
        frame.render_widget(sidebar, list_area);
        draw_sidebar(frame, m, list);
        if let Some(peek) = peek.filter(|_| height > 0) {
            draw_peek(frame, peek, peek_area);
        }
    }

    let (title, path) = pane_title(m, usize::from(right.width));
    let pane_focused = matches!(m.focus(), Focus::Agent(_));
    // Padding keeps the agent's own full-width rules off our border.
    let pane = panel(title, path, pane_style, pane_focused).padding(Padding::horizontal(1));
    let terminal = pane.inner(right);
    m.areas.terminal = terminal;
    frame.render_widget(pane, right);
    draw_terminal(frame, m, terminal);

    draw_status_bar(frame, m, footer);

    match &m.overlay {
        Overlay::None => {}
        Overlay::New(form) => draw_new(frame, m, form),
        Overlay::Switcher(query) => draw_switcher(frame, m, query),
        Overlay::Picker(query) => draw_picker(frame, m, query),
        Overlay::Confirm(confirm) => draw_confirm(frame, confirm),
        Overlay::Help => draw_help(frame, m.keys),
        Overlay::Actions(target) => draw_actions(frame, m, target),
        Overlay::Planning { name, .. } => draw_planning(frame, name),
    }
}

fn sidebar_focused(m: &Model) -> bool {
    matches!(m.focus(), Focus::Nav(_))
}

/// Border styles for the sidebar and pane: Cyan on the panel keys go to,
/// DIM elsewhere and while an overlay has the keys.
fn focus_styles(m: &Model) -> (Style, Style) {
    let accent = Style::new().fg(ACCENT);
    match m.focus() {
        Focus::Agent(_) => (dim(), accent),
        Focus::Nav(_) => (accent, dim()),
        Focus::Overlay(_) => (dim(), dim()),
    }
}

/// A rounded panel with `title` in the top border and `corner` (a count or
/// a path) tucked into the bottom-right, lazygit-style (`1 of 6─╯`). The
/// focused panel's title takes the accent, as lazygit's active title does.
fn panel<'a>(
    mut title: Vec<Span<'a>>,
    mut corner: Vec<Span<'a>>,
    border: Style,
    focused: bool,
) -> Block<'a> {
    if focused {
        for span in title
            .iter_mut()
            .filter(|s| !s.style.add_modifier.contains(Modifier::DIM) && s.style.fg.is_none())
        {
            span.style = span.style.fg(ACCENT);
        }
    }
    if !title.is_empty() {
        title.insert(0, Span::raw(" "));
        title.push(Span::raw(" "));
    }
    if !corner.is_empty() {
        corner.insert(0, Span::raw(" "));
        corner.push(Span::styled("─", border));
    }
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Line::from(title))
        .title_bottom(Line::from(corner).right_aligned())
}

fn sidebar_title(m: &Model) -> Vec<Span<'static>> {
    match (m.snapshot(), m.view, m.selected()) {
        (None, _, _) => vec![Span::styled("Loading…", dim())],
        (Some(_), View::Focus, Some((repo, _))) => {
            let elsewhere = m
                .needs_you()
                .iter()
                .filter(|name| {
                    m.snapshot()
                        .and_then(|s| s.entry(name))
                        .is_some_and(|(r, _)| r.key() != repo.key())
                })
                .count();
            let mut title = vec![Span::styled(repo.name().into_owned(), bold())];
            if elsewhere > 0 {
                title.push(Span::styled(
                    format!(" ◆ {elsewhere}"),
                    Style::new().fg(Color::Yellow),
                ));
            }
            title
        }
        (Some(_), View::Focus, None) => vec![Span::styled("Agents", bold())],
        (Some(_), View::Overview, _) => vec![Span::styled("All repos", bold())],
    }
}

/// `3 of 7`: where the selection is in the list, in the border's colour.
fn sidebar_count(m: &Model, border: Style) -> Vec<Span<'static>> {
    let items = m.items();
    match m.cursor().and_then(|c| items.iter().position(|i| i == c)) {
        Some(i) => vec![Span::styled(
            format!("{} of {}", i + 1, items.len()),
            border,
        )],
        None => Vec::new(),
    }
}

/// A collapsed repo's live and exited agents, leaving out those already
/// listed under Needs you.
fn settled_counts(m: &Model, repo: &agent_core::model::Repo) -> (usize, usize) {
    repo.entries()
        .iter()
        .fold((0, 0), |(live, exited), entry| match m.status(entry) {
            AgentStatus::Idle | AgentStatus::Working { .. } => (live + 1, exited),
            AgentStatus::Exited => (live, exited + 1),
            AgentStatus::NoSession
            | AgentStatus::Asking { .. }
            | AgentStatus::Failed { .. }
            | AgentStatus::Done { .. } => (live, exited),
        })
}

/// Glyph and label on the left. On the right: the repo for a group row,
/// the time in state, then shared and pin markers. When the name would get
/// fewer than [`NAME_MIN`] columns, the repo goes first, then a finished
/// agent's time. Markers and the time of an agent waiting on you stay.
/// `repo_label` names a collapsed single-entry repo.
fn entry_row<'a>(
    m: &Model,
    entry: &'a Entry,
    indent: &'static str,
    repo_label: Option<String>,
    group_repo: Option<String>,
    selected: bool,
    width: usize,
) -> (Vec<Span<'a>>, Vec<Span<'a>>) {
    let status = m.status(entry);
    let style = name_style(&status, selected);
    let mut left = vec![Span::raw(indent), glyph(&status), Span::raw(" ")];
    let fixed = line_width(&left);
    match repo_label {
        Some(repo) => {
            left.push(Span::styled(repo, style));
            if let Some(Head::Branch(branch)) = entry.head() {
                left.push(Span::styled(format!(" {branch}"), dim()));
            }
        }
        None => left.push(Span::styled(entry.label(), style)),
    }
    let mut markers: Vec<Span<'a>> = Vec::new();
    if let Some(n) = m.subagents(entry) {
        markers.push(Span::raw(format!("↳{n}")));
    }
    if let Some(others) = m.pane.connection().and_then(|c| c.sharing(entry.name())) {
        markers.push(Span::styled(
            format!("⇄{others}"),
            Style::new().fg(Color::Yellow),
        ));
    }
    if let Some(pin) = entry.pin() {
        markers.push(Span::styled(
            pin.slot()
                .map_or_else(|| "*".to_string(), |s| s.get().to_string()),
            dim(),
        ));
    }
    let mut repo = group_repo.map(|r| Span::styled(r, dim()));
    let mut time = time_in_state(&status, m.now);
    let joined = |repo: &Option<Span<'a>>, time: &Option<Span<'static>>| -> Vec<Span<'a>> {
        let cells: Vec<Span<'a>> = repo
            .iter()
            .cloned()
            .chain(time.iter().cloned())
            .chain(markers.iter().cloned())
            .collect();
        let mut right = Vec::new();
        for cell in cells {
            if !right.is_empty() {
                right.push(Span::raw(" "));
            }
            right.push(cell);
        }
        right
    };
    let squeezed =
        |right: &[Span<'a>]| width.saturating_sub(fixed + line_width(right) + 1) < NAME_MIN;
    if squeezed(&joined(&repo, &time)) {
        repo = None;
    }
    if squeezed(&joined(&repo, &time)) && matches!(status, AgentStatus::Done { .. }) {
        time = None;
    }
    (left, joined(&repo, &time))
}

fn draw_sidebar(frame: &mut Frame<'_>, m: &mut Model, area: Rect) {
    let Some(snapshot) = m.snapshot() else { return };
    let content_width = usize::from(area.width).saturating_sub(2);
    let focused = sidebar_focused(m);
    let rows = m.rows();
    let cursor = m.cursor();
    let selected_index =
        cursor.and_then(|c| rows.iter().position(|r| r.cursor().as_ref() == Some(c)));
    let height = usize::from(area.height);
    let offset = scroll(
        m.sidebar_offset,
        selected_index.unwrap_or(0),
        height,
        rows.len(),
    );

    let mut lines = Vec::new();
    let mut hits = Vec::new();
    for (i, row) in rows.iter().enumerate().skip(offset).take(height) {
        let y = area.y + u16::try_from(i - offset).unwrap_or(0);
        let selected = selected_index == Some(i);
        let gutter = match (selected, focused) {
            (true, true) => Span::styled("▌", Style::new().fg(ACCENT)),
            (true, false) => Span::styled("▌", dim()),
            (false, _) => Span::raw(" "),
        };
        hits.extend(row.cursor().map(|c| (y, c)));
        let (left, right) = match row {
            Row::Header(group) => (
                vec![
                    Span::styled("▾ ", dim()),
                    Span::styled(group.title(), dim()),
                ],
                Vec::new(),
            ),
            Row::Repo { key, fold } => {
                let Some(repo) = snapshot.repo(key) else {
                    continue;
                };
                let (live, exited) = match fold {
                    Fold::Closed => settled_counts(m, repo),
                    Fold::Open => (0, 0),
                };
                let mut right = Vec::new();
                for (n, g, style) in [(live, "●", Style::new()), (exited, "○", dim())] {
                    if n > 0 {
                        if !right.is_empty() {
                            right.push(Span::raw("  "));
                        }
                        right.push(Span::styled(format!("{g} {n}"), style));
                    }
                }
                let name = Span::styled(
                    repo.name().into_owned(),
                    if selected { bold() } else { Style::new() },
                );
                let arrow = match fold {
                    Fold::Open => "▾ ",
                    Fold::Closed => "▸ ",
                };
                (vec![Span::styled(arrow, dim()), name], right)
            }
            Row::Grouped(_, name) => {
                let Some((repo, entry)) = snapshot.entry(name) else {
                    continue;
                };
                entry_row(
                    m,
                    entry,
                    "  ",
                    None,
                    Some(repo.name().into_owned()),
                    selected,
                    content_width,
                )
            }
            Row::Entry { name, place } => {
                let Some((repo, entry)) = snapshot.entry(name) else {
                    continue;
                };
                match place {
                    Place::Solo => entry_row(
                        m,
                        entry,
                        "",
                        Some(repo.name().into_owned()),
                        None,
                        selected,
                        content_width,
                    ),
                    Place::Nested => entry_row(m, entry, "  ", None, None, selected, content_width),
                    Place::Alone => entry_row(m, entry, "", None, None, selected, content_width),
                }
            }
        };
        let mut line = justify(left, right, content_width);
        line.spans.insert(0, gutter);
        lines.push(line);
    }
    frame.render_widget(Paragraph::new(lines), area);
    m.sidebar_offset = offset;
    m.areas.rows = hits;
}

/// The pane's title (the selected agent) and its corner (the path, or a
/// shared-session warning), cut to fit `width`.
/// The title names what the pane shows: the session it's connected or
/// connecting to, else the selection its placeholder describes.
fn pane_title(m: &Model, width: usize) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let shown = match m.pane.session() {
        Some(name) => match m.snapshot().and_then(|s| s.entry(name)) {
            Some((repo, entry)) => Some((repo, Some(entry))),
            None => return (vec![Span::styled(name.to_string(), bold())], Vec::new()),
        },
        None => m.selected(),
    };
    let Some((repo, entry)) = shown else {
        return (Vec::new(), Vec::new());
    };
    let identifier = bold();
    let repo_name = repo.name().into_owned();
    let mut title = Vec::new();
    match entry {
        Some(e) if e.name().as_str() != repo_name => {
            title.push(Span::styled(repo_name, dim()));
            title.push(Span::styled(" / ", dim()));
            title.push(Span::styled(e.name().to_string(), identifier));
        }
        Some(e) => {
            title.push(Span::styled(repo_name, identifier));
            if let Some(Head::Branch(branch)) = e.head() {
                title.push(Span::styled(format!(" {branch}"), dim()));
            }
        }
        None => title.push(Span::styled(repo_name, identifier)),
    }
    let sharing = entry.and_then(|e| m.pane.connection().and_then(|c| c.sharing(e.name())));
    let corner = if let Some(others) = sharing {
        let clients = if others.get() == 1 {
            "client"
        } else {
            "clients"
        };
        vec![Span::styled(
            format!("⇄ {others} other {clients} · ⏎ take over"),
            Style::new().fg(Color::Yellow),
        )]
    } else {
        let path = entry
            .and_then(Entry::path)
            .or_else(|| repo.key().root().map(agent_core::paths::RepoRoot::path));
        path.map(|p| {
            vec![Span::styled(
                truncate_start(&display_path(p), width.saturating_sub(7)),
                dim(),
            )]
        })
        .unwrap_or_default()
    };
    (fit(title, width.saturating_sub(6)), corner)
}

fn centered(frame: &mut Frame<'_>, area: Rect, lines: Vec<Line<'_>>) {
    let height = u16::try_from(lines.len()).unwrap_or(1);
    let [middle] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    frame.render_widget(Paragraph::new(lines).centered(), middle);
}

fn draw_terminal(frame: &mut Frame<'_>, m: &Model, area: Rect) {
    if let Pane::Connected(c) = &m.pane {
        let show_cursor = matches!(m.focus(), Focus::Agent(_)) && c.term.modes().cursor_visible;
        c.term.render(area, frame.buffer_mut(), show_cursor);
        return;
    }
    let action = |key: &'static str, text: String| {
        Line::from(vec![
            Span::styled(key, bold()),
            Span::raw(format!(" {text}")),
        ])
    };
    let line = match (&m.load, &m.pane, m.selected()) {
        (_, Pane::Connecting { name, .. }, _) => {
            Line::styled(format!("attaching to {name}…"), dim())
        }
        (Load::Pending, ..) => Line::styled("Loading…", dim()),
        (_, _, None) => action("n", "new agent".into()),
        (_, _, Some((repo, None))) => action("⏎", format!("focus {}", repo.name())),
        (_, _, Some((_, Some(entry)))) => match m.reach(entry) {
            Reach::Own => Line::styled("this TUI runs here", dim()),
            Reach::Other {
                enter: Enter::Attach,
                ..
            } => Line::styled(format!("attaching to {}…", entry.name()), dim()),
            Reach::Other {
                enter: Enter::Resurrect | Enter::Restart(_),
                ..
            } => action("⏎", format!("resurrect {}", entry.name())),
            Reach::Other {
                enter: Enter::Start(_),
                ..
            } => action("⏎", format!("start {} here", m.harness_label)),
        },
    };
    centered(frame, area, vec![line]);
}

/// A status-bar hint; lower `rank` is dropped first when space runs out.
struct Hint<'a> {
    spans: [Span<'a>; 3],
    rank: u8,
}

fn ranked<'a>(key: &'a str, desc: &'a str, rank: u8) -> Hint<'a> {
    Hint {
        spans: hint(key, desc),
        rank,
    }
}

/// The mode badge, then context hints, then the status message on the
/// right. Hints that don't fit are dropped least important first (`? keys`
/// always stays); the status never pushes hints out, which made the row
/// jitter as messages came and went, it's cut to fit instead.
fn draw_status_bar(frame: &mut Frame<'_>, m: &Model, area: Rect) {
    let focus = m.focus();
    let nav = m.keys.nav().to_string();
    let twice = format!("{nav} {nav}");
    let mut hints: Vec<Hint<'_>> = Vec::new();
    if let Focus::Agent(_) = focus {
        hints.extend([ranked(&nav, "nav", 9), ranked(&twice, "previous", 8)]);
    } else if let Focus::Overlay(Overlay::New(_)) = focus {
        hints.extend([
            ranked("⌃U", "clear field", 4),
            ranked("⌃W", "delete word", 3),
            ranked("⇧Tab", "previous field", 2),
            ranked("⌃R", "use suggestion", 1),
        ]);
    } else {
        let selected = m.selected();
        let entry = selected.and_then(|(_, e)| e);
        match (selected, entry.map(|e| m.reach(e))) {
            (Some((_, None)), _) => hints.push(ranked("⏎", "focus", 7)),
            (
                _,
                Some(Reach::Other {
                    enter,
                    kill,
                    remove,
                }),
            ) => {
                hints.push(ranked("⏎", enter.verb(), 7));
                hints.extend(kill.map(|_| ranked("d", "kill", 2)));
                hints.extend(remove.map(|_| ranked("D", "remove", 1)));
            }
            (_, Some(Reach::Own) | None) => {}
        }
        hints.push(ranked("Space", "switch", 6));
        if let Some(entry) = entry {
            let pin = match entry.pin() {
                Some(_) => "unpin",
                None => "pin",
            };
            hints.push(ranked("m", pin, 3));
        }
        hints.push(ranked("n", "new", 4));
        hints.push(ranked(
            "Tab",
            if m.view == View::Focus {
                "overview"
            } else {
                "focus"
            },
            5,
        ));
        if entry.is_some() && !m.actions.is_empty() {
            hints.push(ranked("x", "actions", 0));
        }
        hints.push(ranked("?", "keys", 9));
    }

    // What needs you, led by the key that goes there. It shows and hides
    // as a block at the right edge, so the hints never shift.
    let mut counts: Vec<Span<'static>> = Vec::new();
    for (kind, n) in m.attention().parts() {
        let colour = match kind {
            Waiting::Asking => Color::Yellow,
            Waiting::Failed => Color::Red,
            Waiting::Done => Color::Green,
        };
        counts.extend([
            Span::styled(
                format!("{} {n}", kind.glyph()),
                Style::new().fg(colour).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
        ]);
    }
    counts.pop();
    if !counts.is_empty() {
        let key = match focus {
            Focus::Agent(_) => format!("{nav} a"),
            Focus::Nav(_) | Focus::Overlay(_) => "a".to_string(),
        };
        counts.splice(0..0, [Span::raw(key), Span::raw(" ")]);
    }
    // The counts sit between a space and the right edge's space.
    let badge_width = 9 + match line_width(&counts) {
        0 => 0,
        counts => counts + 2,
    };
    let width = usize::from(area.width);
    let used = |hints: &[Hint<'_>]| {
        hints
            .iter()
            .map(|h| line_width(&h.spans))
            .sum::<usize>()
            .saturating_sub(2)
    };
    while hints.len() > 1 && used(&hints) > width.saturating_sub(badge_width) {
        let lowest = hints
            .iter()
            .enumerate()
            .min_by_key(|(_, h)| h.rank)
            .map_or(0, |(i, _)| i);
        hints.remove(lowest);
    }
    let room_for_status = width.saturating_sub(badge_width + used(&hints) + 3);
    let mut status = match &m.status {
        Some(status) if room_for_status > 0 => {
            let style = if status.kind == StatusKind::Error {
                Style::new().fg(Color::Red)
            } else {
                Style::new()
            };
            let mut spans = fit(
                vec![Span::styled(status.text.clone(), style)],
                room_for_status,
            );
            spans.push(Span::raw(" "));
            spans
        }
        _ => Vec::new(),
    };
    if !counts.is_empty() {
        status.push(Span::raw(" "));
        status.extend(counts);
        status.push(Span::raw(" "));
    }
    let label = match focus {
        Focus::Agent(_) => " AGENT ",
        Focus::Nav(_) | Focus::Overlay(_) => "  NAV  ",
    };
    let mut left = vec![badge(label), Span::raw("  ")];
    left.extend(hints.into_iter().flat_map(|h| h.spans));
    left.pop();
    frame.render_widget(Paragraph::new(justify(left, status, width)), area);
}

/// A centred rect of at most `width` x `height` (at least 5 tall), leaving
/// a margin around it.
fn overlay_area(frame: &Frame<'_>, width: u16, height: u16) -> Rect {
    centred(frame, width, height.max(5))
}

/// A centred rect of at most `width` x `height`, leaving a margin around it.
fn centred(frame: &Frame<'_>, width: u16, height: u16) -> Rect {
    let area = frame.area();
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    area
}

/// Clears one extra column each side so underlying text doesn't touch the border.
fn clear_around(frame: &mut Frame<'_>, area: Rect) {
    let bounds = frame.area();
    let x = area.x.saturating_sub(1);
    let right = (area.x + area.width + 1).min(bounds.x + bounds.width);
    frame.render_widget(Clear, Rect::new(x, area.y, right - x, area.height));
}

fn overlay_block<'a>(title: &str, mut hints: Vec<Span<'a>>, border: Style) -> Block<'a> {
    while hints.last().is_some_and(|s| s.content.trim().is_empty()) {
        hints.pop();
    }
    hints.insert(0, Span::raw(" "));
    hints.push(Span::raw(" "));
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(Span::styled(format!(" {title} "), bold()))
        .title_bottom(Line::from(hints).right_aligned())
}

fn highlighted<'a>(text: &'a str, query: &str, base: Style) -> Vec<Span<'a>> {
    let matched = fuzzy_match(query, text).unwrap_or_default();
    text.char_indices()
        .map(|(i, c)| {
            let style = if matched.contains(&i) {
                base.add_modifier(Modifier::BOLD).fg(ACCENT)
            } else {
                base
            };
            Span::styled(c.to_string(), style)
        })
        .collect()
}

type Item<'a> = (Vec<Span<'a>>, Vec<Span<'a>>);

fn draw_query_list(
    frame: &mut Frame<'_>,
    title: &str,
    verb: &str,
    query: &Query,
    items: Vec<Item<'_>>,
) {
    let len = items.len();
    let area = overlay_area(
        frame,
        60,
        u16::try_from(len).unwrap_or(u16::MAX).saturating_add(3),
    );
    clear_around(frame, area);
    let block = overlay_block(title, hints(&[("⏎", verb), ("Esc", "close")]), dim());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [input, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    let list = list.inner(ratatui::layout::Margin::new(1, 0));
    let prompt = Line::from(vec![
        Span::styled(" › ", Style::new().fg(ACCENT)),
        Span::raw(query.text.clone()),
        Span::styled("█", dim()),
    ]);
    frame.render_widget(Paragraph::new(prompt), input);

    let index = query.index(len);
    let height = usize::from(list.height);
    let width = usize::from(list.width).saturating_sub(2);
    let offset = scroll(query.offset.get(), index, height, len);
    query.offset.set(offset);
    let lines: Vec<Line> = items
        .into_iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, (left, right))| {
            let gutter = if i == index {
                Span::styled("▌", Style::new().fg(ACCENT))
            } else {
                Span::raw(" ")
            };
            let mut line = justify(left, right, width);
            line.spans.insert(0, gutter);
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
}

fn draw_switcher(frame: &mut Frame<'_>, m: &Model, query: &Query) {
    let Some(snapshot) = m.snapshot() else { return };
    let items = switcher_matches(m, &query.text)
        .into_iter()
        .filter_map(|name| snapshot.entry(&name))
        .map(|(repo, entry)| {
            let status = m.status(entry);
            let mut left = vec![glyph(&status), Span::raw(" ")];
            left.extend(highlighted(
                entry.name().as_str(),
                &query.text,
                name_style(&status, false),
            ));
            let mut right = Vec::new();
            if let EntryKind::Main { head, .. } = entry.kind() {
                if let Head::Branch(branch) = head {
                    left.push(Span::styled(format!(" {branch}"), dim()));
                }
            } else if repo.name() != entry.name().as_str() {
                right.push(Span::styled(repo.name().into_owned(), dim()));
            }
            if let Some(slot) = entry.pin().and_then(agent_core::model::Pin::slot) {
                if !right.is_empty() {
                    right.push(Span::raw(" "));
                }
                right.push(Span::styled(slot.get().to_string(), dim()));
            }
            (left, right)
        })
        .collect();
    draw_query_list(frame, "Switch", "switch", query, items);
}

fn draw_picker(frame: &mut Frame<'_>, m: &Model, query: &Query) {
    let Some(snapshot) = m.snapshot() else { return };
    let items = picker_matches(m, &query.text)
        .into_iter()
        .filter_map(|key| snapshot.repo(&key))
        .map(|repo| {
            let live = repo.counts().live;
            let right = if live > 0 {
                vec![Span::styled(
                    format!("● {live}"),
                    Style::new().fg(Color::Green),
                )]
            } else {
                Vec::new()
            };
            (highlighted_owned(&repo.name(), &query.text), right)
        })
        .collect();
    draw_query_list(frame, "Repos", "focus", query, items);
}

fn highlighted_owned(text: &str, query: &str) -> Vec<Span<'static>> {
    highlighted(text, query, Style::new())
        .into_iter()
        .map(|s| Span::styled(s.content.into_owned(), s.style))
        .collect()
}

/// `text` split into rows of at most `width` columns, breaking at spaces
/// where it can and keeping its newlines.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.split('\n') {
        let mut row = String::new();
        for word in line.split_inclusive(' ') {
            if row.width() + word.trim_end().width() > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row).trim_end().to_string());
            }
            for c in word.chars() {
                if row.width() + c.width().unwrap_or(0) > width {
                    rows.push(std::mem::take(&mut row));
                }
                row.push(c);
            }
        }
        rows.push(row);
    }
    rows
}

fn spinner() -> &'static str {
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() / 100);
    SPINNER[usize::try_from(tick).unwrap_or(0) % SPINNER.len()]
}

fn draw_new(frame: &mut Frame<'_>, m: &Model, form: &NewAgent) {
    let red = Style::new().fg(Color::Red);
    let yellow = Style::new().fg(Color::Yellow);
    let max = agent_core::names::MAX_SESSION_LEN;
    let width = 64.min(frame.area().width.saturating_sub(4));
    let inner = usize::from(width).saturating_sub(4);
    let field_width = inner.saturating_sub(9);
    let fields = Fields::of(form);
    let label = |field: Field, text: &'static str| fields.label(field, text);
    let cursor = |field: Field| fields.cursor(field);
    let indent = || Span::raw(" ".repeat(8));
    let derived = m.derive(form);
    let check = &derived.check;
    let mut lines = Vec::new();

    let task_rows = TASK_ROWS.min(usize::from(frame.area().height.saturating_sub(14)).max(1));
    lines.extend(task_lines(
        form,
        &fields,
        &m.harness_label,
        field_width,
        task_rows,
    ));
    lines.push(Line::raw(""));

    // Name: the suggestion DIM with where it came from until one is typed;
    // typed characters that don't fit or aren't allowed are Red.
    let mut name = vec![label(Field::Name, "name")];
    let mut source = Vec::new();
    if form.name.is_empty() {
        name.push(cursor(Field::Name));
        match &derived.suggestion {
            Some(suggestion) => {
                name.push(Span::styled(suggestion.name.clone(), dim()));
                let from = match suggestion.source {
                    NameSource::Link => "from the link".to_string(),
                    NameSource::Words => "from the prompt".to_string(),
                    NameSource::Model => format!("named by {}", m.harness_label),
                };
                source.push(Span::styled(from, dim()));
            }
            None => name.push(Span::styled("from the prompt, or type one", dim())),
        }
        if let NamingState::Asking = form.naming.state {
            source = vec![Span::styled(
                format!("{} asking {}", spinner(), m.harness_label),
                dim(),
            )];
        }
    } else {
        let bad: Vec<usize> = invalid_chars(&form.name)
            .into_iter()
            .map(|(i, _)| i)
            .collect();
        name.extend(form.name.char_indices().map(|(i, c)| {
            let wrong = i + c.len_utf8() > max || bad.contains(&i);
            Span::styled(c.to_string(), if wrong { red } else { Style::new() })
        }));
        name.push(cursor(Field::Name));
    }
    lines.push(justify(name, source, inner));

    let message = match (&form.phase, &form.error, check) {
        (Phase::Submitting { .. }, ..) | (_, None, NameCheck::Empty | NameCheck::Free(_)) => None,
        (_, Some(error), _) => Some(Span::styled(error.clone(), red)),
        (_, None, NameCheck::TooLong { bytes }) => Some(Span::styled(
            format!("{} over zellij's limit", bytes - max),
            red,
        )),
        (_, None, NameCheck::Invalid(why)) => Some(Span::styled(why.clone(), red)),
        (_, None, NameCheck::Own) => Some(Span::styled("this TUI runs in that session", yellow)),
        (_, None, NameCheck::MainCheckout { repo }) => Some(Span::styled(
            format!("{repo}'s main checkout: select it in the list and press ⏎"),
            yellow,
        )),
        (_, None, NameCheck::Live { .. } | NameCheck::Exited { .. } | NameCheck::Idle { .. }) => {
            let text = match &derived.submission {
                Submission::Ready(Launchable::Attach { task: Some(_), .. }) => {
                    "already running: the prompt will be typed into it"
                }
                Submission::Ready(Launchable::Attach { task: None, .. }) => {
                    "already running · ⏎ attaches"
                }
                Submission::Ready(Launchable::Resurrect(_)) => "exited · ⏎ resurrects it",
                Submission::Blocked(Blocked::ExitedWithPrompt) => {
                    "exited, so clear the prompt to resurrect it"
                }
                Submission::Ready(Launchable::Start { .. }) => "an idle worktree · ⏎ starts it",
                Submission::Ready(Launchable::Create { .. }) | Submission::Blocked(_) => "",
            };
            Some(Span::styled(text, yellow))
        }
    };
    let length = derived.name.len();
    let counter_style = if length > max { red } else { dim() };
    let counter = vec![Span::styled(format!("{length}/{max}"), counter_style)];
    if message.is_some() || length > 0 {
        let mut row = vec![indent()];
        row.extend(message);
        lines.push(justify(row, counter, inner));
    }
    lines.push(Line::raw(""));

    lines.push(repo_line(m, form, &derived, &fields, inner));

    let keys = form_keys(form, &derived, usize::from(width).saturating_sub(4));

    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = overlay_area(frame, width, height);
    clear_around(frame, area);
    let block = overlay_block("New agent", keys, dim()).padding(Padding::horizontal(1));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The form's field labels and cursor, which show focus only while it
/// takes input.
struct Fields {
    focus: Option<Field>,
}

impl Fields {
    fn of(form: &NewAgent) -> Self {
        let focus = match form.phase {
            Phase::Editing => Some(form.focus),
            Phase::Submitting { .. } => None,
        };
        Self { focus }
    }

    fn label(&self, field: Field, text: &'static str) -> Span<'static> {
        let style = if self.focus == Some(field) {
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            dim()
        };
        Span::styled(format!("{text:<8}"), style)
    }

    fn cursor(&self, field: Field) -> Span<'static> {
        if self.focus == Some(field) {
            Span::styled("█", dim())
        } else {
            Span::raw("")
        }
    }
}

/// The prompt, wrapped on words, its last `task_rows` rows shown with a
/// marker for the rest.
fn task_lines(
    form: &NewAgent,
    fields: &Fields,
    harness_label: &str,
    field_width: usize,
    task_rows: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut rows = if form.task.is_empty() {
        Vec::new()
    } else {
        wrap(&form.task, field_width)
    };
    let hidden = rows.len().saturating_sub(task_rows);
    rows.drain(..hidden);
    if rows.is_empty() {
        let placeholder = format!("first message for {harness_label} (optional)");
        lines.push(Line::from(vec![
            fields.label(Field::Task, "prompt"),
            fields.cursor(Field::Task),
            Span::styled(placeholder, dim()),
        ]));
    } else {
        let last = rows.len() - 1;
        for (i, row) in rows.into_iter().enumerate() {
            let lead = match (i, hidden) {
                (0, 0) => fields.label(Field::Task, "prompt"),
                (0, n) => Span::styled(format!("{:<8}", format!("↑{n}")), dim()),
                _ => Span::raw(" ".repeat(8)),
            };
            let mut spans = vec![lead, Span::raw(row)];
            if i == last {
                spans.push(fields.cursor(Field::Task));
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

/// The repo row: the default DIM, a search completed inline, a path checked.
fn repo_line(
    m: &Model,
    form: &NewAgent,
    derived: &Derived,
    fields: &Fields,
    inner: usize,
) -> Line<'static> {
    let red = Style::new().fg(Color::Red);
    let attaching = matches!(
        &derived.submission,
        Submission::Ready(
            Launchable::Attach { .. } | Launchable::Resurrect(_) | Launchable::Start { .. }
        ) | Submission::Blocked(Blocked::ExitedWithPrompt)
    );
    let mut repo = vec![fields.label(Field::Repo, "repo")];
    let mut right = Vec::new();
    match &derived.repo_input {
        _ if attaching => {
            repo.push(Span::styled(form.repo.text.clone(), dim()));
            repo.push(Span::styled("unused: it already exists", dim()));
        }
        RepoInput::Default(root) => {
            repo.push(fields.cursor(Field::Repo));
            match root {
                Some(root) => repo.push(Span::styled(root.name().into_owned(), dim())),
                None => repo.push(Span::styled("none selected", dim())),
            }
            if fields.focus == Some(Field::Repo) {
                right.push(Span::styled("type to search · ~/path", dim()));
            }
        }
        RepoInput::Search(found) => {
            repo.push(Span::raw(form.repo.text.clone()));
            match (derived.completion.clone(), found) {
                (Some(rest), _) => {
                    repo.push(fields.cursor(Field::Repo));
                    repo.push(Span::styled(rest, dim()));
                }
                (None, Some(root)) => {
                    repo.push(fields.cursor(Field::Repo));
                    repo.push(Span::styled(format!("  → {}", root.name()), dim()));
                }
                (None, None) => {
                    repo.push(fields.cursor(Field::Repo));
                    repo.push(Span::styled("  no repo matches", red));
                }
            }
            let count = repo_matches(m.snapshot(), &form.repo.text).len();
            if form.focus == Field::Repo && count > 1 {
                right.push(Span::styled(
                    format!("⌃N ⌃P {} of {count}", form.repo.index(count) + 1),
                    dim(),
                ));
            }
        }
        RepoInput::Path(_) => {
            repo.push(Span::raw(form.repo.text.clone()));
            repo.push(fields.cursor(Field::Repo));
        }
        RepoInput::Missing(_) => {
            repo.push(Span::raw(form.repo.text.clone()));
            repo.push(fields.cursor(Field::Repo));
            repo.push(Span::styled("  no such directory", red));
        }
        RepoInput::Unsupported => {
            repo.push(Span::raw(form.repo.text.clone()));
            repo.push(fields.cursor(Field::Repo));
            repo.push(Span::styled("  ~user paths aren't supported", red));
        }
    }
    justify(repo, right, inner)
}

/// The form's bottom-border hints, dropped least important first to fit
/// `room` columns.
fn form_keys<'a>(form: &'a NewAgent, derived: &Derived, room: usize) -> Vec<Span<'a>> {
    if let Phase::Submitting { launch, .. } = &form.phase {
        return vec![
            Span::styled(
                format!("{} {} {}", spinner(), launch.progress(), launch.session()),
                dim(),
            ),
            Span::raw("  "),
            Span::raw("Esc"),
            Span::styled(" background", dim()),
        ];
    }
    let enter = match &derived.submission {
        Submission::Ready(launch) => Some(launch.verb()),
        Submission::Blocked(_) => None,
    };
    let mut hints: Vec<Hint<'_>> = Vec::new();
    hints.extend(enter.map(|enter| ranked("⏎", enter, 9)));
    hints.push(
        match derived
            .completion
            .as_ref()
            .filter(|_| form.focus == Field::Repo)
        {
            Some(_) => ranked("Tab", "complete", 8),
            None => ranked("Tab", "next", 4),
        },
    );
    if form.focus == Field::Task {
        hints.push(ranked("⌃J", "newline", 5));
    }
    hints.push(ranked("⌃E", "open in $EDITOR", 3));
    hints.push(ranked("Esc", "cancel", 9));
    let used = |hints: &[Hint<'_>]| hints.iter().map(|h| line_width(&h.spans)).sum::<usize>();
    while hints.len() > 2 && used(&hints) > room {
        let lowest = hints
            .iter()
            .enumerate()
            .min_by_key(|(_, h)| h.rank)
            .map_or(0, |(i, _)| i);
        hints.remove(lowest);
    }
    hints.into_iter().flat_map(|h| h.spans).collect()
}

fn draw_confirm(frame: &mut Frame<'_>, confirm: &Confirm) {
    let warn = Style::new().fg(Color::Yellow);
    let danger = Style::new().fg(Color::Red);
    let destructive = |key: &'static str, desc: &'static str| {
        vec![
            Span::styled(key, danger.add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {desc}"), dim()),
            Span::raw("  "),
        ]
    };
    let (title, body, keys, border) = match confirm {
        Confirm::Kill(running) => (
            format!("Kill {}", running.name()),
            vec![
                Line::raw("Stops the session. The checkout and branch stay."),
                Line::styled("Resurrect it from the sidebar.", dim()),
            ],
            [destructive("y/⏎", "kill"), hints(&[("n/Esc", "cancel")])].concat(),
            warn,
        ),
        Confirm::Remove(proposal) => {
            let plan = proposal.plan();
            let mut body = vec![match plan.deletes() {
                Deletes::Agent {
                    branch: Some(branch),
                    ..
                } => Line::raw(format!(
                    "Deletes the session, the worktree and the {branch} branch."
                )),
                Deletes::Agent { branch: None, .. } => {
                    Line::raw("Deletes the session and the worktree. It's on no branch.")
                }
                Deletes::Session => {
                    Line::raw("Deletes the session. No files or branches are touched.")
                }
            }];
            body.extend(
                plan.hazards()
                    .iter()
                    .map(|h| Line::styled(h.to_string(), danger.add_modifier(Modifier::BOLD))),
            );
            let mut keys: Vec<Span<'_>> = proposal
                .offers()
                .into_iter()
                .flat_map(|offer| {
                    let key = match offer.effect {
                        OfferEffect::Destroys => {
                            Span::styled(offer.key.to_string(), danger.add_modifier(Modifier::BOLD))
                        }
                        OfferEffect::Keeps => Span::raw(offer.key.to_string()),
                    };
                    [
                        key,
                        Span::styled(format!(" {}", offer.label), dim()),
                        Span::raw("  "),
                    ]
                })
                .collect();
            keys.extend(hints(&[("n/Esc", "cancel")]));
            let border = match proposal {
                Proposal::Clean(_) => warn,
                Proposal::Hazardous(_) => danger,
            };
            (format!("Remove {}", plan.name()), body, keys, border)
        }
    };
    let content = body
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .max(line_width(&keys) + 4);
    let area = overlay_area(
        frame,
        u16::try_from(content + 6).unwrap_or(u16::MAX),
        u16::MAX,
    );
    let inner_width = usize::from(area.width).saturating_sub(6).max(1);
    let wrapped: usize = body
        .iter()
        .map(|l| l.width().div_ceil(inner_width).max(1))
        .sum();
    let height = u16::try_from(wrapped + 4).unwrap_or(u16::MAX);
    let area = overlay_area(frame, area.width, height);
    clear_around(frame, area);
    let mut lines = vec![Line::raw("")];
    lines.extend(body.into_iter().map(|mut l| {
        l.spans.insert(0, Span::raw("  "));
        l
    }));
    let paragraph = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
    frame.render_widget(paragraph.block(overlay_block(&title, keys, border)), area);
}

fn draw_planning(frame: &mut Frame<'_>, name: &SessionName) {
    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{} checking what would be lost…", spinner()), dim()),
    ]);
    let width = u16::try_from(line.width() + 6).unwrap_or(u16::MAX);
    let area = centred(frame, width.max(30), 3);
    clear_around(frame, area);
    let block = overlay_block(
        &format!("Remove {name}"),
        hints(&[("Esc", "cancel")]),
        Style::new().fg(Color::Yellow),
    );
    frame.render_widget(Paragraph::new(line).block(block), area);
}

/// The bottom of the sidebar: what the pane can't show about an agent.
struct Peek {
    title: Vec<Span<'static>>,
    /// In display order, each with its priority: lower survives longer.
    lines: Vec<(u8, Line<'static>)>,
}

/// Rows for the peek, or none when it doesn't fit: at most a third of the
/// sidebar and 8 lines, never leaving the list fewer than 5 rows.
fn peek_height(body: u16, lines: usize) -> u16 {
    let lines = u16::try_from(lines).unwrap_or(u16::MAX);
    if body < PEEK_MIN_BODY || lines == 0 {
        return 0;
    }
    let inner = (body / 3).saturating_sub(2).clamp(3, 8).min(lines);
    let room = body.saturating_sub(2 + LIST_MIN_ROWS + 2);
    match inner.min(room) {
        0 => 0,
        inner => inner + 2,
    }
}

/// What a state line says, and the detail under it.
fn describe_status(status: &AgentStatus) -> (String, Option<String>) {
    match status {
        AgentStatus::NoSession => ("no session".to_string(), None),
        AgentStatus::Exited => ("exited".to_string(), None),
        AgentStatus::Idle => ("idle".to_string(), None),
        AgentStatus::Working {
            doing: Doing::Tool(tool),
            ..
        } => (tool.name.clone(), tool.target.clone()),
        AgentStatus::Working {
            doing: Doing::Thinking,
            ..
        } => ("thinking".to_string(), None),
        AgentStatus::Working {
            doing: Doing::HandingBack,
            ..
        } => ("handing back".to_string(), None),
        AgentStatus::Working {
            doing: Doing::Delegating(n),
            ..
        } => (
            format!(
                "waiting on {n} subagent{}",
                if n.get() == 1 { "" } else { "s" }
            ),
            None,
        ),
        AgentStatus::Asking { question, .. } => ("asking".to_string(), Some(question.clone())),
        AgentStatus::Failed { reason, .. } => (reason.clone(), None),
        AgentStatus::Done { summary, .. } => ("done".to_string(), summary.clone()),
    }
}

/// How long the current tool has run, else how long it's been in its state.
fn state_time(status: &AgentStatus, now: SystemTime) -> Option<Span<'static>> {
    match status {
        AgentStatus::Working {
            doing: Doing::Tool(tool),
            pulse: Pulse::Steady,
            ..
        } => Some(Span::styled(ago(tool.since, now), dim())),
        _ => time_in_state(status, now),
    }
}

fn peek(m: &Model, width: usize) -> Option<Peek> {
    let now = m.now;
    let entry = m.peek_target()?;
    let status = m.status(entry);
    let mut lines: Vec<(u8, Line<'static>)> = Vec::new();
    let (verb, detail) = describe_status(&status);
    // When the pane shows the agent, its question or tool is already on
    // screen: the title carries the state and the body what the pane can't.
    let in_pane = m.pane.session() == Some(entry.name());
    let title = if in_pane {
        let mut title = vec![glyph(&status), Span::styled(format!(" {verb}"), bold())];
        title.extend(
            state_time(&status, now).map(|t| Span::styled(format!(" {}", t.content), t.style)),
        );
        title
    } else {
        vec![Span::styled(entry.label().to_string(), bold())]
    };
    if !in_pane {
        let mut state = vec![glyph(&status), Span::raw(format!(" {verb}"))];
        let time: Vec<Span<'static>> = state_time(&status, now).into_iter().collect();
        state = justify(state, time, width).spans;
        lines.push((1, Line::from(state)));
        if let Some(detail) = detail {
            let mut wrapped = wrap(&detail, width).into_iter();
            if let Some(first) = wrapped.next() {
                lines.push((2, Line::raw(first)));
            }
            if let Some(second) = wrapped.next() {
                lines.push((6, Line::raw(second)));
            }
        }
    }
    if let Some(prompt) = m.activity(entry.name()).and_then(|a| a.prompt.clone()) {
        lines.push((
            3,
            Line::from(fit(vec![Span::styled(format!("› {prompt}"), dim())], width)),
        ));
    }
    if let Some((_, facts)) = m.facts.as_ref().filter(|(name, _)| name == entry.name()) {
        let diff = facts.progress.diff;
        let changes = match diff.files {
            0 => vec![Span::styled("no changes", dim())],
            files => vec![
                Span::styled(format!("+{}", diff.added), Style::new().fg(Color::Green)),
                Span::raw(" "),
                Span::styled(format!("−{}", diff.removed), Style::new().fg(Color::Red)),
                Span::styled(
                    format!(" · {files} {}", if files == 1 { "file" } else { "files" }),
                    dim(),
                ),
            ],
        };
        lines.push((4, Line::from(changes)));
        let mut git = Vec::new();
        if facts.progress.ahead > 0 {
            git.push(format!("↑{}", facts.progress.ahead));
        }
        if facts.dirty > 0 {
            git.push(format!("{} uncommitted", facts.dirty));
        }
        if !git.is_empty() {
            lines.push((5, Line::from(Span::styled(git.join(" · "), dim()))));
        }
    }
    Some(Peek { title, lines })
}

fn draw_peek(frame: &mut Frame<'_>, peek: Peek, area: Rect) {
    let rows = usize::from(area.height.saturating_sub(2));
    let mut kept: Vec<(usize, u8)> = peek
        .lines
        .iter()
        .enumerate()
        .map(|(i, (priority, _))| (i, *priority))
        .collect();
    kept.sort_by_key(|(i, priority)| (*priority, *i));
    kept.truncate(rows);
    kept.sort_unstable();
    let mut all: Vec<Option<Line<'static>>> =
        peek.lines.into_iter().map(|(_, line)| Some(line)).collect();
    let lines: Vec<Line<'static>> = kept
        .into_iter()
        .filter_map(|(i, _)| all.get_mut(i).and_then(Option::take))
        .collect();
    let block = panel(peek.title, Vec::new(), dim(), false).padding(Padding::horizontal(1));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_actions(frame: &mut Frame<'_>, m: &Model, target: &SessionName) {
    let actions = m.actions.all();
    let label_width = actions
        .iter()
        .map(|a| a.label.as_str().width())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line<'_>> = actions
        .iter()
        .map(|action| {
            let detail = match &action.kind {
                Kind::Run(command) | Kind::Interactive(command) => command.program().to_string(),
                Kind::TerminalEvent { name, .. } => name.as_str().to_string(),
            };
            Line::from(vec![
                Span::styled(format!("   {}", action.key), bold()),
                Span::raw(format!("   {}", pad(action.label.as_str(), label_width))),
                Span::styled(format!("   {detail}"), dim()),
            ])
        })
        .collect();
    let width = lines.iter().map(Line::width).max().unwrap_or(0) + 3;
    let title = format!("Run on {target}");
    let width = width.max(title.width() + 6);
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = centred(frame, u16::try_from(width).unwrap_or(u16::MAX), height);
    clear_around(frame, area);
    let block = overlay_block(&title, hints(&[("Esc", "close")]), dim());
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn pad(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

fn help_section<K: AsRef<str>>(
    section: &'static str,
    keys: &[(K, &'static str)],
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![Span::raw(" "), badge(section)])];
    lines.extend(keys.iter().map(|(key, desc)| {
        Line::from(vec![
            Span::raw(format!("   {:<10}", key.as_ref())),
            Span::raw(*desc),
        ])
    }));
    lines
}

/// Sections stacked in one column, or side by side when the terminal is
/// too short for that.
fn draw_help(frame: &mut Frame<'_>, keys: Keys) {
    let nav = keys.nav();
    let agent = [
        (nav.to_string(), "back to NAV"),
        (format!("{nav} {nav}"), "previous agent"),
    ];
    let sections: Vec<Vec<Line<'static>>> = std::iter::once(help_section(" AGENT ", &agent))
        .chain(
            HELP.iter()
                .map(|(section, keys)| help_section(section, keys)),
        )
        .collect();
    let available = usize::from(frame.area().height.saturating_sub(4));
    let stacked = |sections: &[Vec<Line<'static>>], gap: bool| {
        let mut lines = Vec::new();
        for (i, section) in sections.iter().enumerate() {
            if i > 0 && gap {
                lines.push(Line::raw(""));
            }
            lines.extend(section.iter().cloned());
        }
        lines
    };
    let rows: usize = sections.iter().map(Vec::len).sum();
    let columns = if rows + sections.len() - 1 <= available {
        vec![stacked(&sections, true)]
    } else if rows <= available {
        vec![stacked(&sections, false)]
    } else {
        let (longest, _) = sections
            .iter()
            .enumerate()
            .max_by_key(|(_, s)| s.len())
            .unwrap_or((0, &Vec::new()));
        let others: Vec<_> = sections
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != longest)
            .map(|(_, s)| s.clone())
            .collect();
        vec![sections[longest].clone(), stacked(&others, true)]
    };
    let widths: Vec<usize> = columns
        .iter()
        .map(|c| c.iter().map(Line::width).max().unwrap_or(0) + 2)
        .collect();
    let height = columns.iter().map(Vec::len).max().unwrap_or(0);
    let area = centred(
        frame,
        u16::try_from(widths.iter().sum::<usize>() + 2).unwrap_or(u16::MAX),
        u16::try_from(height + 2).unwrap_or(u16::MAX),
    );
    clear_around(frame, area);
    let block = overlay_block("Keys", hints(&[("any key", "close")]), dim());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let constraints: Vec<Constraint> = widths
        .iter()
        .map(|w| Constraint::Length(u16::try_from(*w).unwrap_or(u16::MAX)))
        .collect();
    for (column, rect) in columns
        .into_iter()
        .zip(Layout::horizontal(constraints).split(inner).iter())
    {
        frame.render_widget(Paragraph::new(column), *rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_peek_takes_a_third_up_to_eight_lines_and_leaves_the_list_room() {
        assert_eq!(
            peek_height(23, 7),
            7,
            "an 80x24 terminal: 5 lines and borders"
        );
        assert_eq!(peek_height(44, 7), 9, "never more lines than it has");
        assert_eq!(peek_height(44, 20), 10, "at most 8 lines");
        assert_eq!(peek_height(29, 2), 4);
        assert_eq!(
            peek_height(14, 7),
            0,
            "short terminals keep every row for the list"
        );
        assert_eq!(peek_height(23, 0), 0);
    }

    #[test]
    fn times_read_in_the_largest_whole_unit() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        let ago_secs = |secs| ago(now - std::time::Duration::from_secs(secs), now);
        assert_eq!(
            [
                ago_secs(38),
                ago_secs(240),
                ago_secs(7300),
                ago_secs(300_000)
            ],
            ["38s", "4m", "2h", "3d"]
        );
    }
}
