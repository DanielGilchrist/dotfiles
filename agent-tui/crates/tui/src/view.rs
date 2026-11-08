use std::cell::Cell;
use std::path::Path;
use std::time::SystemTime;

use agent_core::actions::Kind;
use agent_core::model::{Entry, EntryKind, Head};
use agent_core::names::{Checkout, SessionName};
use agent_core::ops::Deletes;
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::keymap::{self, Situation};
use crate::model::{
    AgentStatus, Blocked, Confirm, Derived, Enter, Field, Focus, FormKind, HelpState, Launchable,
    Load, Model, NameCheck, NameSource, NamingState, NewAgent, OfferEffect, Overlay, Pane, Phase,
    Proposal, Pulse, Query, Reach, RepoInput, Row, Screen, StatusKind, Submission, View, Waiting,
    WorkRoom, fuzzy_match, invalid_chars, repo_matches,
};
use crate::palette::Palette;
use crate::update::{Pick, Picks, Unusable, picker_matches, switcher_matches};

mod page;
mod sidebar;

const SIDEBAR_MAX: u16 = 48;
const SIDEBAR_MIN: u16 = 28;
/// The sidebar's share of the width, in percent, between the bounds above.
const SIDEBAR_SHARE: u32 = 24;

const TASK_ROWS: usize = 6;
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
/// Below this width the agent needs every column; Space still switches agents.
const NARROW: u16 = 60;
/// A name keeps at least this many columns before row details give way.
const NAME_MIN: usize = 8;
thread_local! {
    /// Set from the model at the start of each draw. Styles are chosen deep
    /// in helpers that only see what they draw, and tests draw on their own
    /// threads.
    static PALETTE: Cell<Palette> = const { Cell::new(Palette::DEFAULT) };
}

fn palette() -> Palette {
    PALETTE.get()
}

fn dim() -> Style {
    palette().dim.style()
}

fn accent() -> Style {
    Style::new().fg(palette().accent)
}

fn warning() -> Style {
    Style::new().fg(palette().warning)
}

fn error() -> Style {
    Style::new().fg(palette().error)
}

fn success() -> Style {
    Style::new().fg(palette().success)
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
        AgentStatus::Asking { .. } => Span::styled("◆", warning().add_modifier(Modifier::BOLD)),
        AgentStatus::Failed { .. } => Span::styled("!", error().add_modifier(Modifier::BOLD)),
        AgentStatus::Done { .. } => Span::styled("✓", success().add_modifier(Modifier::BOLD)),
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
            .fg(palette().accent)
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
    PALETTE.set(m.palette);
    m.settle_main();
    let [body, footer] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let borders = 2;
    let sidebar_width = if body.width < NARROW || m.screen == Screen::Full {
        0
    } else {
        let share = u32::from(body.width) * SIDEBAR_SHARE / 100;
        u16::try_from(share)
            .unwrap_or(u16::MAX)
            .clamp(SIDEBAR_MIN, SIDEBAR_MAX)
            + borders
    };
    let [left, right] =
        Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(0)]).areas(body);
    let (sidebar_style, pane_style) = focus_styles(m);

    if sidebar_width > 0 {
        sidebar::draw(frame, m, left, sidebar_style);
    } else {
        // The panels keep the keys while the sidebar is out of sight, and
        // the list is laid out as if it were there, so moving still works.
        m.areas.sidebar = Rect::new(0, 0, SIDEBAR_MIN, sidebar::list_room(body.height));
        m.areas.rows.clear();
        m.areas.agent_rows.clear();
        m.areas.change_rows.clear();
        m.areas.agent_panel = Rect::default();
        m.areas.changes_panel = Rect::default();
        m.work_room = WorkRoom::Fits;
    }
    m.sidebar_shown = sidebar_width > 0;

    let pane_focused = matches!(m.focus(), Focus::Agent(_)) || m.main_focused();
    // Padding keeps the agent's own full-width rules off our border. The
    // terminal's rect never depends on what [0] shows, so switching between
    // it and a document never resizes the agent's pty.
    let padding = Padding::horizontal(1);
    let terminal = Block::bordered().padding(padding).inner(right);
    m.areas.terminal = terminal;
    if let Some(page) = page::page(m, terminal, usize::from(right.width), pane_style) {
        let block = panel(page.title, page.corner, pane_style, pane_focused)
            .padding(padding)
            .title_top(Line::from(sidebar::pad_title(page.right)).right_aligned());
        frame.render_widget(Paragraph::new(page.lines).block(block), right);
    } else {
        let (title, path) = pane_title(m, usize::from(right.width));
        let block = panel(title, path, pane_style, pane_focused).padding(padding);
        frame.render_widget(block, right);
        draw_terminal(frame, m, terminal);
    }

    draw_status_bar(frame, m, footer);

    draw_peek(frame, m);

    match &m.overlay {
        Overlay::None => {}
        Overlay::New(form) => draw_new(frame, m, form),
        Overlay::Switcher(query) => draw_switcher(frame, m, query),
        Overlay::Picker(query) => draw_picker(frame, m, query),
        Overlay::Confirm(confirm) => draw_confirm(frame, confirm),
        Overlay::Help(help) => draw_help(frame, m, help),
        Overlay::Actions(target) => draw_actions(frame, m, target),
        Overlay::Talk { to, text } => draw_talk(frame, m, to, text),
        Overlay::Planning { name, .. } => draw_planning(frame, name),
    }
}

fn sidebar_focused(m: &Model) -> bool {
    matches!(m.focus(), Focus::Nav(_)) && !m.main_focused()
}

/// Border styles for the sidebar and pane: Cyan on the panel keys go to,
/// DIM elsewhere and while an overlay has the keys.
fn focus_styles(m: &Model) -> (Style, Style) {
    let accent = accent();
    match m.focus() {
        Focus::Agent(_) => (dim(), accent),
        Focus::Nav(_) if m.main_focused() => (dim(), accent),
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
            span.style = span.style.fg(palette().accent);
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

/// The tab strip needs the repo name at its shortest, the two fixed tabs
/// and the separators.
const TABS_MIN: usize = 4 + NAME_MIN + 18;

/// `[1]` then the tabs, lazygit-style: the active one BOLD, the others
/// plain with a DIM `·` between. The form follows the width alone, so
/// moving the cursor never flips it: below `TABS_MIN` only the active tab
/// shows, and above it the repo name yields to fit.
fn sidebar_title(m: &Model, width: usize) -> Vec<Span<'static>> {
    let Some(_) = m.snapshot() else {
        return vec![Span::raw("[1] "), Span::styled("Loading…", dim())];
    };
    let repo = m.selected().map(|(repo, _)| repo.name().into_owned());
    let name = |view: View, room: usize| match view {
        View::Focus => repo.as_deref().map(|r| truncate_end(r, room)),
        View::Overview => Some("Agents".into()),
        View::Pinned => Some("Pinned".into()),
    };
    if width < TABS_MIN || repo.is_none() {
        let mut title = vec![Span::raw("[1]")];
        if let Some(active) = name(m.view, width.saturating_sub(4)) {
            title.push(Span::raw(" "));
            title.push(Span::styled(active, bold()));
        }
        return title;
    }
    let mut tabs = vec![Span::raw("[1] ")];
    for (i, view) in View::ALL.into_iter().enumerate() {
        if i > 0 {
            tabs.push(Span::styled(" · ", dim()));
        }
        // An explicit default colour keeps the inactive tabs out of the
        // focused panel's accent, which would read them as one title.
        let style = if view == m.view {
            bold()
        } else {
            Style::new().fg(Color::Reset)
        };
        tabs.extend(name(view, width - (TABS_MIN - NAME_MIN)).map(|n| Span::styled(n, style)));
    }
    tabs
}

/// `3 of 7`: where the selection is in the list, in the border's colour.
/// Agents listed from other repos count, though they can't be selected.
/// While the filter hides entries, a DIM `+7 hidden` leads it when `width`
/// has room.
fn sidebar_count(m: &Model, border: Style, width: usize) -> Vec<Span<'static>> {
    let items = m.items();
    let elsewhere = m
        .rows()
        .iter()
        .filter(|r| matches!(r, Row::Elsewhere(_)))
        .count();
    let mut corner = Vec::new();
    let hidden = m.hidden();
    if hidden > 0 && width >= HIDDEN_MIN {
        corner.push(Span::styled(format!("+{hidden} hidden"), dim()));
    }
    if let Some(i) = m.cursor().and_then(|c| items.iter().position(|i| i == c)) {
        if !corner.is_empty() {
            corner.push(Span::styled(" · ", dim()));
        }
        corner.push(Span::styled(
            format!("{} of {}", i + 1, items.len() + elsewhere),
            border,
        ));
    }
    corner
}

/// The narrowest list that shows `+n hidden` beside `i of n`.
const HIDDEN_MIN: usize = 26;

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
    let mut title = vec![Span::raw("[0] ")];
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
            warning(),
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
    // An overlay clears only its own area, so a centred line would show
    // through beside it.
    if !matches!(m.overlay, Overlay::None) {
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
        (_, _, None) if m.view == View::Pinned => Line::styled("Nothing pinned", dim()),
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
    let table = if matches!(focus, Focus::Agent(_) | Focus::Overlay(Overlay::New(_))) {
        Vec::new()
    } else {
        keymap::footer(&Situation::of(m), &nav)
    };
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
    } else if let Focus::Overlay(Overlay::Switcher(_) | Overlay::Picker(_) | Overlay::Talk { .. }) =
        focus
    {
        // The overlay's border carries its keys, so the footer doesn't
        // compete with it.
        hints.push(ranked("?", "keys", 9));
    } else {
        hints.extend(table.iter().map(|h| ranked(&h.keys, h.label, h.rank)));
    }

    // What needs you, led by the key that goes there. It shows and hides
    // as a block at the right edge, so the hints never shift.
    let mut counts: Vec<Span<'static>> = Vec::new();
    for (kind, n) in m.attention().parts() {
        let colour = match kind {
            Waiting::Asking => palette().warning,
            Waiting::Failed => palette().error,
            Waiting::Done => palette().success,
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
    let queued: usize = m.outboxes.values().map(|o| o.queue.len()).sum();
    if queued > 0 {
        let lead = [
            Span::styled(format!("{queued} queued"), dim()),
            Span::raw("  "),
        ];
        counts.splice(0..0, lead);
        if counts.len() == 2 {
            counts.pop();
        }
    }
    // The counts sit between a space, the right edge's space and the
    // column `justify` keeps between the sides.
    let badge_width = 9 + match line_width(&counts) {
        0 => 0,
        counts => counts + 3,
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
                error()
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
        _ if m.peeking() => " PEEK  ",
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
                base.add_modifier(Modifier::BOLD).fg(palette().accent)
            } else {
                base
            };
            Span::styled(c.to_string(), style)
        })
        .collect()
}

type Item<'a> = (Vec<Span<'a>>, Vec<Span<'a>>);

/// Columns of a query overlay's width that aren't a row's: the border,
/// the list margin, the gutter and its pad.
const QUERY_ROW_INSET: usize = 6;

/// A query overlay: the input, then `items` with the one under the cursor
/// marked. `verb` is what `⏎` does with it, left out when nothing is under
/// it. A long query keeps its end in view.
fn draw_query_list(
    frame: &mut Frame<'_>,
    title: &str,
    verb: Option<&str>,
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
    let keys: Vec<(&str, &str)> = verb
        .map(|verb| ("⏎", verb))
        .into_iter()
        .chain([("Esc", "close")])
        .collect();
    let block = overlay_block(title, hints(&keys), dim());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [input, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    let list = list.inner(ratatui::layout::Margin::new(1, 0));
    let room = usize::from(input.width).saturating_sub(" › ".width() + 1);
    let prompt = Line::from(vec![
        Span::styled(" › ", accent()),
        Span::raw(truncate_start(&query.text, room)),
        Span::styled("█", dim()),
    ]);
    frame.render_widget(Paragraph::new(prompt), input);

    let index = query.index(len);
    let marked = verb.map(|_| index);
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
            let gutter = if marked == Some(i) {
                Span::styled("▌", accent())
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
    let items: Vec<Item<'_>> = switcher_matches(m, &query.text)
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
            (left, right)
        })
        .collect();
    let verb = (!items.is_empty()).then_some("switch");
    draw_query_list(frame, "Switch", verb, query, items);
}

/// Every row says where its repo is (DIM), since a name alone can't tell
/// two clones apart, and a listed row its live count before that. A typed
/// path keeps its end. With no rows, the one row says why.
fn draw_picker(frame: &mut Frame<'_>, m: &Model, query: &Query) {
    let matches = picker_matches(m, &query.text);
    let rows = matches.rows();
    let verb = query.pick(rows).map(Pick::verb);
    let width = usize::from(centred(frame, 60, 5).width).saturating_sub(QUERY_ROW_INSET);
    // A place never takes more than half the row, so the name stays legible.
    let place = |path: &Path| {
        truncate_start(
            &path.parent().map_or_else(String::new, display_path),
            width / 2,
        )
    };
    let mut items: Vec<Item<'_>> = rows
        .iter()
        .filter_map(|pick| {
            Some(match pick {
                Pick::Listed(key) => {
                    let repo = m.snapshot()?.repo(key)?;
                    let live = repo.counts().live;
                    let mut right = Vec::new();
                    if live > 0 {
                        right.push(Span::raw(format!("● {live}  ")));
                    }
                    right.push(Span::styled(
                        repo.key()
                            .root()
                            .map(|r| place(r.path()))
                            .unwrap_or_default(),
                        dim(),
                    ));
                    (highlighted_owned(&repo.name(), &query.text), right)
                }
                Pick::Found(root) => (
                    highlighted_owned(&root.name(), &query.text),
                    vec![Span::styled(place(root.path()), dim())],
                ),
                Pick::Path(path) => {
                    let right = Span::styled("add its repo", dim());
                    let room = width.saturating_sub(right.width() + 1);
                    (
                        vec![Span::raw(truncate_start(&display_path(path), room))],
                        vec![right],
                    )
                }
            })
        })
        .collect();
    match matches {
        Picks::Rows(_) => {}
        Picks::NoMatch => items.push((vec![Span::styled("no match", dim())], Vec::new())),
        Picks::Unusable(why) => {
            let text = match why {
                Unusable::UserHome => "~user paths aren't supported",
                Unusable::NoHome => "~ needs HOME set",
            };
            items.push((vec![Span::styled(text, error())], Vec::new()));
        }
    }
    draw_query_list(frame, "Repos", verb, query, items);
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
                    if c == ' ' {
                        continue;
                    }
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
    let red = error();
    let yellow = warning();
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
    // What's wrong with the branch sits under it, live for a pasted link
    // and after ⏎ for anything else, since a branch is invalid mid-word.
    let target_problem = match (form.kind, &derived.submission) {
        (FormKind::Checkout, Submission::Blocked(Blocked::Target(why)))
            if form.task.contains("github.com") || form.error.is_some() =>
        {
            Some(why.clone())
        }
        _ => None,
    };
    match &target_problem {
        Some(why) => lines.extend(
            wrap(why, inner.saturating_sub(8))
                .into_iter()
                .map(|row| Line::from(vec![indent(), Span::styled(row, red)])),
        ),
        None => lines.push(Line::raw("")),
    }
    let error = form
        .error
        .as_ref()
        .filter(|e| target_problem.as_ref() != Some(e));

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
                    NameSource::Link
                        if form.kind == FormKind::New
                            && matches!(form.task.trim().parse(), Ok(Checkout::PullRequest(_))) =>
                    {
                        "from the link · Esc then c checks it out".to_string()
                    }
                    NameSource::Link => "from the link".to_string(),
                    NameSource::Words => "from the prompt".to_string(),
                    NameSource::Branch => "from the branch".to_string(),
                    NameSource::Model => format!("named by {}", m.harness_label),
                };
                source.push(Span::styled(from, dim()));
            }
            None => name.push(Span::styled(
                match form.kind {
                    FormKind::New => "from the prompt, or type one",
                    FormKind::Checkout => "from the branch, or type one",
                },
                dim(),
            )),
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

    let message = match (&form.phase, error, check) {
        (Phase::Submitting { .. }, ..) | (_, None, NameCheck::Empty | NameCheck::Free(_)) => None,
        (_, Some(error), _) => Some((error.clone(), red)),
        (_, None, NameCheck::TooLong { bytes }) => {
            Some((format!("{} over zellij's limit", bytes - max), red))
        }
        (_, None, NameCheck::Invalid(why)) => Some((why.clone(), red)),
        (_, None, NameCheck::Own) => Some(("this TUI runs in that session".into(), yellow)),
        (_, None, NameCheck::MainCheckout { repo }) => Some((
            match form.kind {
                FormKind::New => {
                    format!("{repo}'s main checkout: select it in the list and press ⏎")
                }
                FormKind::Checkout => format!("{repo}'s main checkout: pick another name"),
            },
            yellow,
        )),
        (_, None, NameCheck::Live { .. } | NameCheck::Exited { .. } | NameCheck::Idle { .. }) => {
            let (text, style) = match &derived.submission {
                Submission::Ready(Launchable::Checkout {
                    target: Checkout::Branch(_),
                    ..
                }) => ("already on that branch · ⏎ attaches", yellow),
                Submission::Ready(Launchable::Checkout {
                    target: Checkout::PullRequest(_),
                    ..
                }) => (
                    "already an agent · ⏎ attaches if its branch is the PR's",
                    yellow,
                ),
                Submission::Blocked(
                    refused @ (Blocked::OnAnotherBranch(_) | Blocked::InAnotherRepo(_)),
                ) => {
                    let text = match refused {
                        Blocked::InAnotherRepo(repo) => format!("already an agent in {repo}"),
                        Blocked::OnAnotherBranch(Some(on)) => format!("already an agent on {on}"),
                        _ => "already an agent on a detached HEAD".to_string(),
                    };
                    lines.push(justify(
                        vec![indent(), Span::styled(text, red)],
                        vec![Span::styled(format!("{}/{max}", derived.name.len()), dim())],
                        inner,
                    ));
                    lines.push(Line::raw(""));
                    lines.push(repo_line(m, form, &derived, &fields, inner));
                    return finish_form(frame, form, &derived, lines, width);
                }
                Submission::Ready(Launchable::Attach { task: Some(_), .. }) => {
                    ("already running: the prompt will be typed into it", yellow)
                }
                Submission::Ready(Launchable::Attach { task: None, .. }) => {
                    ("already running · ⏎ attaches", yellow)
                }
                Submission::Ready(Launchable::Resurrect(_)) => ("exited · ⏎ resurrects it", yellow),
                Submission::Blocked(Blocked::ExitedWithPrompt) => {
                    ("exited, so clear the prompt to resurrect it", yellow)
                }
                Submission::Ready(Launchable::Start { .. }) => {
                    ("an idle worktree · ⏎ starts it", yellow)
                }
                Submission::Ready(Launchable::Create { .. }) | Submission::Blocked(_) => {
                    ("", yellow)
                }
            };
            Some((text.to_string(), style))
        }
    };
    let length = derived.name.len();
    let counter_style = if length > max { red } else { dim() };
    let counter = vec![Span::styled(format!("{length}/{max}"), counter_style)];
    // Worker errors can be long, so they wrap under the name rather than
    // being cut, with the counter beside the first row.
    let rows: Vec<String> = match &message {
        Some((text, _)) => wrap(text, inner.saturating_sub(8 + counter[0].width() + 1)),
        None => Vec::new(),
    };
    if !rows.is_empty() || length > 0 {
        let style = message.as_ref().map_or(dim(), |(_, style)| *style);
        let mut rows = rows.into_iter();
        let mut first = vec![indent()];
        first.extend(rows.next().map(|row| Span::styled(row, style)));
        lines.push(justify(first, counter, inner));
        lines.extend(rows.map(|row| Line::from(vec![indent(), Span::styled(row, style)])));
    }
    lines.push(Line::raw(""));

    lines.push(repo_line(m, form, &derived, &fields, inner));
    finish_form(frame, form, &derived, lines, width);
}

/// Draws the assembled form, sized to its lines.
fn finish_form(
    frame: &mut Frame<'_>,
    form: &NewAgent,
    derived: &Derived,
    lines: Vec<Line<'static>>,
    width: u16,
) {
    let keys = form_keys(form, derived, usize::from(width).saturating_sub(4));
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let area = overlay_area(frame, width, height);
    clear_around(frame, area);
    let title = match form.kind {
        FormKind::New => "New agent",
        FormKind::Checkout => "Check out for review",
    };
    let block = overlay_block(title, keys, dim()).padding(Padding::horizontal(1));
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
            accent().add_modifier(Modifier::BOLD)
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
    let (label, placeholder) = match form.kind {
        FormKind::New => (
            "prompt",
            format!("first message for {harness_label} (optional)"),
        ),
        FormKind::Checkout => ("branch", "a branch, or a GitHub PR link".to_string()),
    };
    if rows.is_empty() {
        lines.push(Line::from(vec![
            fields.label(Field::Task, label),
            fields.cursor(Field::Task),
            Span::styled(placeholder, dim()),
        ]));
    } else {
        let last = rows.len() - 1;
        for (i, row) in rows.into_iter().enumerate() {
            let lead = match (i, hidden) {
                (0, 0) => fields.label(Field::Task, label),
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
    let red = error();
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
            let linked = match form.task.trim().parse() {
                Ok(Checkout::PullRequest(pr)) if form.kind == FormKind::Checkout => Some(pr),
                _ => None,
            };
            match linked {
                Some(pr) if root.as_ref().is_none_or(|root| root.name() != pr.repo) => {
                    right.push(Span::styled(
                        format!("the link is {}", pr.slug()),
                        Style::new().fg(Color::Yellow),
                    ));
                }
                _ if fields.focus == Some(Field::Repo) => {
                    right.push(Span::styled("type to search · ~/path", dim()));
                }
                _ => {}
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
            let count = repo_matches(m.snapshot(), &m.catalogue, &form.repo.text).len();
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
    if form.kind == FormKind::New {
        if form.focus == Field::Task {
            hints.push(ranked("⌃J", "newline", 5));
        }
        hints.push(ranked("⌃E", "open in $EDITOR", 3));
    }
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
    let warn = warning();
    let danger = error();
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
        Confirm::Quit(queued) => (
            "Quit with messages queued".to_string(),
            vec![
                Line::raw(format!(
                    "{queued} queued message{} would be lost. Agents keep running.",
                    if *queued == 1 { "" } else { "s" }
                )),
                Line::styled("Queues live in this TUI, not on disk.", dim()),
            ],
            [destructive("y/⏎", "quit"), hints(&[("n/Esc", "stay")])].concat(),
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
        warning(),
    );
    frame.render_widget(Paragraph::new(line).block(block), area);
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

fn draw_talk(frame: &mut Frame<'_>, m: &Model, to: &SessionName, text: &str) {
    let queued: Vec<&str> = m
        .outboxes
        .get(to)
        .map(|o| o.queue.iter().map(AsRef::as_ref).collect())
        .unwrap_or_default();
    let shown = queued.len().min(3);
    let height = u16::try_from(3 + shown + usize::from(queued.len() > shown)).unwrap_or(3);
    let area = centred(frame, 72, height);
    clear_around(frame, area);
    let ready = m.ready_for_text(to);
    let hint_keys: &[(&str, &str)] = match (ready, queued.is_empty()) {
        (true, true) => &[("⏎", "send"), ("Esc", "close")],
        (_, true) => &[("⏎", "queue"), ("Esc", "close")],
        (_, false) => &[("⏎", "queue"), ("⌃X", "clear queue"), ("Esc", "close")],
    };
    let title = if ready {
        format!("Send to {to}")
    } else {
        format!("Send to {to} · queues until it's at rest")
    };
    let block = overlay_block(&title, hints(hint_keys), dim());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let room = usize::from(inner.width).saturating_sub(4);
    let mut lines: Vec<Line<'static>> = queued
        .iter()
        .take(shown)
        .map(|q| {
            Line::styled(
                format!(
                    " queued: {}",
                    truncate_end(&crate::update::first_words(q), room.saturating_sub(8))
                ),
                dim(),
            )
        })
        .collect();
    if queued.len() > shown {
        lines.push(Line::styled(
            format!(" +{} more queued", queued.len() - shown),
            dim(),
        ));
    }
    lines.push(Line::from(vec![
        Span::styled(" › ", accent()),
        Span::raw(truncate_start(text, room)),
        Span::styled("█", dim()),
    ]));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The peek float: the agent's terminal at the pty's size, over everything
/// else, which is dimmed. A pty bigger than the screen is cropped.
fn draw_peek(frame: &mut Frame<'_>, m: &Model) {
    let Some(name) = m.peek.as_ref().filter(|_| m.peeking()) else {
        return;
    };
    let screen = frame.area();
    for cell in &mut frame.buffer_mut().content {
        cell.modifier.insert(Modifier::DIM);
    }
    let size = match &m.pane {
        Pane::Connected(c) if &c.name == name => Some(c.term.size()),
        _ => None,
    };
    let (cols, rows) = size.map_or((m.areas.terminal.width, m.areas.terminal.height), |s| {
        (s.cols.get(), s.rows.get())
    });
    let width = cols.saturating_add(2).min(screen.width);
    let height = rows.saturating_add(2).min(screen.height);
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let title = m.snapshot().and_then(|s| s.entry(name)).map_or_else(
        || vec![Span::raw(name.to_string())],
        |(_, entry)| sidebar::status_line(m, entry, 0),
    );
    let mut keys = Vec::new();
    let nav = m.keys.nav().to_string();
    for hint in keymap::footer(&Situation::of(m), &nav) {
        if !keys.is_empty() {
            keys.push(Span::styled(" · ", dim()));
        }
        keys.push(Span::raw(hint.keys));
        keys.push(Span::styled(format!(" {}", hint.label), dim()));
    }
    let mut title_spans = vec![Span::raw(" ")];
    title_spans.extend(title);
    title_spans.push(Span::raw(" "));
    let fits = line_width(&title_spans) + line_width(&keys) + 4 <= usize::from(area.width);
    let cropped = cols > area.width.saturating_sub(2) || rows > area.height.saturating_sub(2);
    let mut block = Block::bordered()
        .border_type(BorderType::Double)
        .border_style(accent())
        .title_top(Line::from(title_spans))
        .title_top(Line::from(if fits { keys } else { Vec::new() }).right_aligned());
    if cropped {
        block = block.title_bottom(Line::styled(" cropped, showing the bottom ", dim()));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    match &m.pane {
        Pane::Connected(c) if &c.name == name => {
            if cropped {
                render_bottom(&c.term, (cols, rows), inner, frame.buffer_mut());
            } else {
                c.term.render(inner, frame.buffer_mut(), false);
            }
        }
        _ => centered(
            frame,
            inner,
            vec![Line::styled(format!("attaching to {name}…"), dim())],
        ),
    }
}

/// Draws a terminal of `size` into `area`, dropping rows from the top and
/// columns from the right so the bottom, where the prompt is, stays visible.
fn render_bottom(
    term: &agent_term::Terminal<Box<dyn agent_term::Emulator>>,
    (cols, rows): (u16, u16),
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
) {
    let full = Rect::new(0, 0, cols, rows);
    let mut scratch = ratatui::buffer::Buffer::empty(full);
    term.render(full, &mut scratch, false);
    let skip = rows.saturating_sub(area.height);
    for y in 0..area.height.min(rows) {
        for x in 0..area.width.min(cols) {
            if let (Some(from), Some(to)) = (
                scratch.cell((x, y + skip)),
                buf.cell_mut((area.x + x, area.y + y)),
            ) {
                *to = from.clone();
            }
        }
    }
}

fn pad(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

/// The keybindings list: the focused context's keys, then the global ones,
/// one row each with the one under the cursor highlighted, and below it
/// a longer description of that row. Keys that do nothing here aren't
/// listed.
fn draw_help(frame: &mut Frame<'_>, m: &Model, state: &HelpState) {
    let nav = m.keys.nav().to_string();
    let situation = Situation::of(m);
    let help = keymap::help(&situation, &nav, &state.filter);
    let area = centred(frame, 78, 26);
    clear_around(frame, area);
    let describe = if area.height >= 12 { 5 } else { 0 };
    let [list_area, describe_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(describe)]).areas(area);

    let selected = state.selected.min(help.len().saturating_sub(1));
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(dim())
        .title_top(Line::styled(" Keybindings ", bold()).left_aligned())
        .title_top(Line::styled(" (type to filter) ", dim()).right_aligned())
        .title_bottom(
            Line::styled(
                format!(
                    " {} of {} ",
                    usize::from(help.len() > 0) + selected,
                    help.len()
                ),
                dim(),
            )
            .right_aligned(),
        );
    if state.typing || !state.filter.is_empty() {
        block = block.title_bottom(
            Line::from(vec![
                Span::raw(format!(" filter: {}", state.filter)),
                Span::styled("█ ", dim()),
            ])
            .left_aligned(),
        );
    }
    let inner = block.inner(list_area);
    frame.render_widget(block, list_area);

    let width = usize::from(inner.width);
    let key_width = help
        .local
        .iter()
        .chain(&help.global)
        .map(|r| r.keys.width())
        .max()
        .unwrap_or(0)
        .min(20);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected_line = 0;
    let mut index = 0;
    for (title, rows) in [("Local", &help.local), ("Global", &help.global)] {
        if rows.is_empty() {
            continue;
        }
        let mut header = vec![Span::styled(format!(" {title}"), bold())];
        if title == "Local" {
            header.push(Span::styled(format!("  {}", situation.ctx.name()), dim()));
        }
        lines.push(Line::from(header));
        for row in rows {
            let text = pad(
                &format!("  {}  {}", pad(&row.keys, key_width), row.label),
                width,
            );
            if index == selected {
                selected_line = lines.len();
                lines.push(Line::styled(
                    text,
                    Style::new().add_modifier(Modifier::REVERSED),
                ));
            } else {
                lines.push(Line::from(vec![
                    Span::raw(format!("  {}", pad(&row.keys, key_width))),
                    Span::raw(format!("  {}", row.label)),
                ]));
            }
            index += 1;
        }
    }
    if lines.is_empty() {
        lines.push(Line::styled("  no key matches", dim()));
    }
    let visible = usize::from(inner.height);
    let offset = scroll(state.offset.get(), selected_line, visible, lines.len());
    state.offset.set(offset);
    frame.render_widget(
        Paragraph::new(
            lines
                .into_iter()
                .skip(offset)
                .take(visible)
                .collect::<Vec<_>>(),
        ),
        inner,
    );

    if describe > 0 {
        let escape = if state.typing || !state.filter.is_empty() {
            "clear filter"
        } else {
            "close"
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(dim())
            .title_top(Line::styled(" Description ", bold()).left_aligned())
            .title_bottom(Line::from(hints(&[("Esc", escape)])).right_aligned());
        let inner = block.inner(describe_area);
        frame.render_widget(block, describe_area);
        let text = help.get(selected).map_or("", |row| row.long);
        frame.render_widget(
            Paragraph::new(format!(" {text}")).wrap(Wrap { trim: true }),
            inner,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
