//! The sidebar, three panels stacked lazygit-style: [1] the fleet as
//! cards, [2] the selected agent's turn, [3] its work in git.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agent_core::activity::Subagent;
use agent_core::model::{Entry, Head, Repo, RepoKey, Snapshot};
use agent_core::names::SessionName;
use agent_core::ports::{DiffStat, Edit, FileState, WorktreeFacts};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::{
    NAME_MIN, accent, ago, bold, dim, error, fit, glyph, justify, line_width, name_style, palette,
    panel, scroll, sidebar_count, sidebar_focused, sidebar_title, spinner, success, truncate_end,
    truncate_start, warning,
};
use crate::model::{
    AgentStatus, Cursor, Doing, Extent, Failure, Fold, ListScroll, Model, Panel as Focused, Place,
    Pulse, Row, Target, WorkRoom,
};
use crate::tree::{ChangesScope, Counts, Node, TreeRow};

/// Rows [2] keeps inside its border however long the list is.
const DETAIL_MIN: usize = 4;
/// File rows [3] keeps inside its border however long the list is. It's
/// kept even for an agent with no changes, so moving the selection never
/// changes how much room the list has.
const CHANGES_MIN: usize = 3;
/// The readiness row above the files, reserved whether or not it shows,
/// for the same reason.
const READINESS_ROW: usize = 1;
/// Inner widths below this use the short forms.
const SHORT_BELOW: usize = 36;
/// Trailing path components a changed file keeps when they fit.
const PATH_TAIL: usize = 3;

/// Whether there's room for the long forms of card and block lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Long,
    Short,
}

impl Form {
    fn of(inner: usize) -> Self {
        if inner < SHORT_BELOW {
            Self::Short
        } else {
            Self::Long
        }
    }
}

/// Which cards get a second line. Chosen from the list's [`Extent`], so
/// agents changing state never change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    /// Every card is one line.
    One,
    /// The first `spare` agents `a` would visit get a second line.
    Waiting { spare: usize },
    /// Every agent that isn't settled gets a second line.
    Two,
}

/// The most second lines that fit the list in `rows`, counting every live
/// card as if it had one.
fn tier(extent: Extent, rows: usize) -> Tier {
    if extent.rows + extent.live <= rows {
        Tier::Two
    } else if extent.rows < rows {
        Tier::Waiting {
            spare: rows - extent.rows,
        }
    } else {
        Tier::One
    }
}

/// How much of an agent's gist a line showed, which decides what [2]
/// repeats of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Whole,
    Cut,
    /// The card has no second line.
    Hidden,
}

fn failure_text(failure: &Failure) -> &str {
    match failure {
        Failure::RateLimit => "rate limited",
        Failure::Overloaded => "overloaded",
        Failure::Billing => "billing limit",
        Failure::Other(reason) => reason,
    }
}

fn target_text(target: &Target) -> &str {
    match target {
        Target::Path(text) | Target::Text(text) => text,
    }
}

/// What a card's second line says about an agent that isn't settled.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Gist<'a> {
    Asking(&'a str),
    Failed(&'a Failure),
    Tool {
        name: &'a str,
        target: Option<&'a Target>,
    },
    Thinking,
    Delegating {
        subagents: NonZeroUsize,
        quiet: String,
    },
    HandingBack,
    Done(Option<&'a str>),
}

impl<'a> Gist<'a> {
    /// Settled agents (idle, exited, no session) have none. `heard` is when
    /// the agent or any of its subagents last reported.
    fn of(status: &'a AgentStatus, heard: SystemTime, now: SystemTime) -> Option<Self> {
        match status {
            AgentStatus::NoSession | AgentStatus::Exited | AgentStatus::Idle => None,
            AgentStatus::Asking { question, .. } => Some(Self::Asking(question)),
            AgentStatus::Failed { failure, .. } => Some(Self::Failed(failure)),
            AgentStatus::Done { summary, .. } => Some(Self::Done(summary.as_deref())),
            AgentStatus::Working { doing, .. } => Some(match doing {
                Doing::Thinking => Self::Thinking,
                Doing::Tool { name, target } => Self::Tool {
                    name,
                    target: target.as_ref(),
                },
                Doing::Delegating(subagents) => Self::Delegating {
                    subagents: *subagents,
                    quiet: ago(heard, now),
                },
                Doing::HandingBack => Self::HandingBack,
            }),
        }
    }

    /// A question is read, everything else glanced at.
    fn style(&self) -> Style {
        match self {
            Self::Asking(_) => Style::new(),
            _ => dim(),
        }
    }

    /// The second line in `width` columns, and whether that's all of it.
    fn line(&self, form: Form, width: usize) -> (String, Shown) {
        let (text, shown) = match (self, form) {
            (Self::Asking(question), _) => first_line(question),
            (Self::Failed(failure), _) => first_line(failure_text(failure)),
            (Self::Tool { name, target }, form) => tool_line(name, *target, form, width),
            (Self::Thinking, _) => ("thinking".to_string(), Shown::Whole),
            (Self::Delegating { subagents, quiet }, Form::Long) => {
                let noun = if subagents.get() == 1 {
                    "subagent"
                } else {
                    "subagents"
                };
                (format!("↳{subagents} {noun} · quiet {quiet}"), Shown::Whole)
            }
            (Self::Delegating { subagents, quiet }, Form::Short) => {
                (format!("↳{subagents} · quiet {quiet}"), Shown::Whole)
            }
            (Self::HandingBack, _) => ("handing back".to_string(), Shown::Whole),
            (Self::Done(Some(summary)), _) => first_line(&plain(summary)),
            (Self::Done(None), _) => ("done".to_string(), Shown::Whole),
        };
        let fitted = truncate_end(&text, width);
        let shown = if fitted == text { shown } else { Shown::Cut };
        (fitted, shown)
    }

    /// All of what the agent says about this, for [0]'s turn document.
    /// Delegating has none, since the subagents say more.
    fn message(&self) -> Option<String> {
        Some(match self {
            Self::Asking(question) => question.trim().to_string(),
            Self::Failed(failure) => failure_text(failure).to_string(),
            Self::Tool {
                name,
                target: Some(target),
            } => format!("{name}  {}", target_text(target)),
            Self::Tool { name, target: None } => (*name).to_string(),
            Self::Thinking => "thinking".to_string(),
            Self::HandingBack => "handing back".to_string(),
            Self::Done(Some(summary)) => plain(summary),
            Self::Done(None) => "done".to_string(),
            Self::Delegating { .. } => return None,
        })
    }
}

/// `{Tool}  {target}`: a path keeps its end, and the short form keeps only
/// its basename.
fn tool_line(name: &str, target: Option<&Target>, form: Form, width: usize) -> (String, Shown) {
    let gap = match form {
        Form::Long => "  ",
        Form::Short => " ",
    };
    let (target, shown) = match (target, form) {
        (None, _) => return (name.to_string(), Shown::Whole),
        (Some(Target::Path(path)), Form::Long) => {
            let cut = truncate_start(path, width.saturating_sub(name.width() + gap.width()));
            let shown = if cut == *path {
                Shown::Whole
            } else {
                Shown::Cut
            };
            (cut, shown)
        }
        (Some(Target::Path(path)), Form::Short) => {
            let base = basename(path);
            let shown = if base == path {
                Shown::Whole
            } else {
                Shown::Cut
            };
            (base.to_string(), shown)
        }
        (Some(Target::Text(text)), _) => (text.clone(), Shown::Whole),
    };
    (format!("{name}{gap}{target}"), shown)
}

/// The first line with text, and whether any other line has text.
fn first_line(text: &str) -> (String, Shown) {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or_default().to_string();
    match lines.next() {
        Some(_) => (first, Shown::Cut),
        None => (first, Shown::Whole),
    }
}

/// `text` without the markdown marks that read as noise outside a
/// renderer.
fn plain(text: &str) -> String {
    text.lines()
        .map(|line| line.trim_start_matches(['#', '>']).trim_start())
        .collect::<Vec<_>>()
        .join("\n")
        .replace("**", "")
        .replace('`', "")
}

fn basename(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.rsplit('/').next().unwrap_or(path)
}

/// How a card names its agent, and whether it sits under a header.
enum Title {
    /// By its label, as in Focus.
    Label,
    /// By its label, indented under its open repo.
    Child,
    /// By its label, with its repo on the right, under a Needs you header.
    Grouped(String),
    /// By its label, with its repo on the right and no header above it,
    /// as in the Pinned tab.
    Pinned(String),
    /// Like [`Title::Grouped`], for an agent from another repo listed in
    /// Focus. It never takes a second line, so it can't change the tier.
    Elsewhere(String),
    /// By its repo, standing in for a repo with no other entry.
    Repo(String),
}

impl Title {
    fn indent(&self) -> &'static str {
        match self {
            Self::Label | Self::Repo(_) | Self::Pinned(_) => "",
            Self::Child | Self::Grouped(_) | Self::Elsewhere(_) => "  ",
        }
    }
}

/// What the list's cards and the selection's panels share.
struct Deck<'m> {
    m: &'m Model,
    tier: Tier,
    form: Form,
    /// Columns between the gutter and the right pad.
    content: usize,
    needs_you: Vec<SessionName>,
    /// Who changed each file, by repo, for overlaps.
    changed_by: HashMap<(&'m RepoKey, &'m Path), Vec<&'m Entry>>,
}

impl<'m> Deck<'m> {
    /// `rows` is the most the list can take.
    fn new(m: &'m Model, snapshot: &'m Snapshot, inner: usize, rows: usize) -> Self {
        let mut changed_by: HashMap<_, Vec<_>> = HashMap::new();
        for (repo, entry) in snapshot.entries() {
            if let Some(Ok(facts)) = m.facts.get(entry.name()) {
                for change in &facts.changed {
                    changed_by
                        .entry((repo.key(), change.path.as_path()))
                        .or_default()
                        .push(entry);
                }
            }
        }
        Self {
            m,
            tier: tier(m.extent(), rows),
            form: Form::of(inner),
            content: inner.saturating_sub(2),
            needs_you: m.needs_you(),
            changed_by,
        }
    }

    /// Whether the tier gives this agent's card a second line, if its
    /// state has anything to say there.
    fn has_line2(&self, name: &SessionName) -> bool {
        match self.tier {
            Tier::One => false,
            Tier::Waiting { spare } => self
                .needs_you
                .iter()
                .position(|n| n == name)
                .is_some_and(|rank| rank < spare),
            Tier::Two => true,
        }
    }

    /// An agent's lines in the list (line 1 always, line 2 when the tier
    /// gives it one), and how much of its gist line 2 shows.
    fn card(&self, entry: &Entry, title: Title, selected: bool) -> (Vec<Line<'static>>, Shown) {
        let m = self.m;
        let status = m.status(entry);
        let heard = m.activity(entry.name()).map_or(m.now, |a| a.heard);
        let gist = Gist::of(&status, heard, m.now)
            .filter(|_| !matches!(title, Title::Elsewhere(_)) && self.has_line2(entry.name()));
        let subagents = match gist {
            Some(Gist::Delegating { .. }) => None,
            _ => m.subagents(entry),
        };
        let indent = title.indent();
        let elsewhere = matches!(title, Title::Elsewhere(_));
        let line1 = self.line1(entry, &status, title, subagents, selected);
        let blocked = matches!(gist, Some(Gist::Asking(_) | Gist::Failed(_)));
        if !blocked
            && !elsewhere
            && self.has_line2(entry.name())
            && let Some(text) = queued_line(
                m,
                entry.name(),
                self.content.saturating_sub(indent.width() + 2),
            )
        {
            let line2 = Line::from(vec![
                Span::raw(format!("{indent}  ")),
                Span::styled(text, dim()),
            ]);
            return (vec![line1, line2], Shown::Whole);
        }
        match gist {
            Some(gist) => {
                let room = self.content.saturating_sub(indent.width() + 2);
                let (text, shown) = gist.line(self.form, room);
                let line2 = Line::from(vec![
                    Span::raw(format!("{indent}  ")),
                    Span::styled(text, gist.style()),
                ]);
                (vec![line1, line2], shown)
            }
            None => (vec![line1], Shown::Hidden),
        }
    }

    /// Glyph and name, then `↳n` while subagents run and line 2 doesn't say
    /// so. On the right: the repo in a group, the time in state, the pin
    /// and a shared marker. When the name would get fewer than
    /// [`NAME_MIN`] columns, the repo goes first, then a finished agent's
    /// time.
    fn line1(
        &self,
        entry: &Entry,
        status: &AgentStatus,
        title: Title,
        subagents: Option<NonZeroUsize>,
        selected: bool,
    ) -> Line<'static> {
        let m = self.m;
        let style = name_style(status, selected);
        let mut left = vec![Span::raw(title.indent()), glyph(status), Span::raw(" ")];
        let fixed = line_width(&left);
        let mut repo = None;
        match title {
            Title::Repo(name) => {
                left.push(Span::styled(name, style));
                if let Some(Head::Branch(branch)) = entry.head() {
                    left.push(Span::styled(format!(" {branch}"), dim()));
                }
            }
            Title::Grouped(name) | Title::Elsewhere(name) | Title::Pinned(name) => {
                left.push(Span::styled(entry.label().to_string(), style));
                repo = match self.form {
                    Form::Long => Some(name),
                    Form::Short => None,
                };
            }
            Title::Label | Title::Child => {
                left.push(Span::styled(entry.label().to_string(), style));
            }
        }
        left.extend(subagents.map(|n| Span::styled(format!(" ↳{n}"), dim())));
        let mut time = time_in_state(status, m.now);
        let mut markers = Vec::new();
        if m.pane
            .connection()
            .and_then(|c| c.sharing(entry.name()))
            .is_some()
        {
            markers.push(Span::styled("⇄", warning()));
        }
        let right = |repo: &Option<String>, time: &[Span<'static>]| {
            let mut right: Vec<Span<'static>> = repo
                .iter()
                .map(|r| Span::styled(r.clone(), dim()))
                .collect();
            if !right.is_empty() && !time.is_empty() {
                right.push(Span::raw("  "));
            }
            right.extend(time.iter().cloned());
            for marker in &markers {
                if !right.is_empty() {
                    right.push(Span::raw(" "));
                }
                right.push(marker.clone());
            }
            right
        };
        let squeezed = |right: &[Span<'static>]| {
            self.content.saturating_sub(fixed + line_width(right) + 1) < NAME_MIN
        };
        if squeezed(&right(&repo, &time)) {
            repo = None;
        }
        if squeezed(&right(&repo, &time)) && matches!(status, AgentStatus::Done { .. }) {
            time.clear();
        }
        justify(left, right(&repo, &time), self.content)
    }

    fn repo_line(&self, repo: &Repo, fold: Fold, selected: bool) -> Line<'static> {
        let (live, exited) = match fold {
            Fold::Closed => settled_counts(self.m, repo),
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
        justify(vec![Span::styled(arrow, dim()), name], right, self.content)
    }

    /// Files this agent changed that others in its repo changed too, with
    /// the others that changed each.
    fn overlaps(&self, repo: &Repo, entry: &Entry) -> HashMap<PathBuf, Vec<&'m Entry>> {
        let Some(Ok(mine)) = self.m.facts.get(entry.name()) else {
            return HashMap::new();
        };
        mine.changed
            .iter()
            .filter_map(|change| {
                let others: Vec<&Entry> = self
                    .changed_by
                    .get(&(repo.key(), change.path.as_path()))?
                    .iter()
                    .copied()
                    .filter(|e| e.name() != entry.name())
                    .collect();
                (!others.is_empty()).then(|| (change.path.clone(), others))
            })
            .collect()
    }

    /// [2]'s rows: the turn, then one per subagent, and the row the cursor
    /// is on while [2] has the keys. What they say is [0]'s to show.
    fn turn_rows(&self, entry: &Entry) -> (Vec<Line<'static>>, Option<usize>) {
        let m = self.m;
        let Some(activity) = m.activity(entry.name()) else {
            return (Vec::new(), None);
        };
        let cursor = matches!(m.panel.listed(), Focused::Agent { .. }).then(|| m.row_at());
        let mut rows = vec![Line::raw(turn_row(
            activity.prompt.as_deref(),
            self.content,
            cursor == Some(0),
        ))];
        rows.extend(tree(
            activity.subagents.values().collect(),
            self.form,
            self.content,
            m.now,
            cursor.and_then(|row| row.checked_sub(1)),
        ));
        (rows, cursor)
    }
}

/// The turn row: `› ` and the prompt's first line, cut to `width`. The mark
/// for the cursor takes the place of the panel's padding.
fn turn_row(prompt: Option<&str>, width: usize, selected: bool) -> String {
    let first = prompt
        .and_then(|p| p.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("no prompt yet");
    let row = truncate_end(&format!("› {first}"), width);
    if selected {
        format!("{GUTTER}{row}")
    } else {
        row
    }
}

/// How long it's been in its state, for states where that matters. A
/// working agent that's gone quiet gets a Yellow `?`.
fn time_in_state(status: &AgentStatus, now: SystemTime) -> Vec<Span<'static>> {
    match status {
        AgentStatus::Working {
            since,
            pulse: Pulse::Stale,
            ..
        } => vec![
            Span::styled(ago(*since, now), dim()),
            Span::styled("?", warning()),
        ],
        AgentStatus::Asking { since, .. } => vec![Span::styled(ago(*since, now), warning())],
        AgentStatus::Working {
            since,
            pulse: Pulse::Steady,
            ..
        }
        | AgentStatus::Failed { since, .. }
        | AgentStatus::Done { since, .. } => vec![Span::styled(ago(*since, now), dim())],
        AgentStatus::NoSession | AgentStatus::Exited | AgentStatus::Idle => Vec::new(),
    }
}

/// A collapsed repo's live and exited agents, leaving out those already
/// listed under Needs you.
fn settled_counts(m: &Model, repo: &Repo) -> (usize, usize) {
    repo.entries()
        .iter()
        .filter(|entry| m.active(entry))
        .fold((0, 0), |(live, exited), entry| match m.status(entry) {
            AgentStatus::Idle | AgentStatus::Working { .. } => (live + 1, exited),
            AgentStatus::Exited => (live, exited + 1),
            AgentStatus::NoSession
            | AgentStatus::Asking { .. }
            | AgentStatus::Failed { .. }
            | AgentStatus::Done { .. } => (live, exited),
        })
}

/// The mark in a row's first column for what the cursor is on. It takes the
/// place of the panel's padding, so rows keep their alignment.
const GUTTER: &str = "▌";

/// `├ ⠹ description  kind  4m`, oldest first, the last child on `└`.
fn tree(
    mut subagents: Vec<&Subagent>,
    form: Form,
    width: usize,
    now: SystemTime,
    cursor: Option<usize>,
) -> Vec<Line<'static>> {
    let cursor = cursor.map(|at| at.min(subagents.len().saturating_sub(1)));
    subagents.sort_by_key(|s| s.since);
    subagents
        .iter()
        .enumerate()
        .map(|(i, subagent)| {
            let branch = if i + 1 == subagents.len() {
                "└ "
            } else {
                "├ "
            };
            let (glyph, since) = match subagent.finished {
                Some(at) => (
                    Span::styled("✓", success().add_modifier(Modifier::BOLD)),
                    ago(subagent.since, at),
                ),
                None => (Span::raw(spinner()), ago(subagent.since, now)),
            };
            let described = subagent.description.as_deref().filter(|d| !d.is_empty());
            let (label, kind_only, right) = match (described, form) {
                (Some(description), Form::Long) => {
                    (description, false, format!(" {}  {since}", subagent.kind))
                }
                (Some(description), Form::Short) => (description, false, format!(" {since}")),
                (None, _) => (subagent.kind.as_str(), true, format!(" {since}")),
            };
            let selected = cursor == Some(i);
            let mut line = justify(
                vec![
                    Span::styled(branch, dim()),
                    glyph,
                    Span::raw(" "),
                    Span::styled(
                        label.to_string(),
                        match (kind_only, selected) {
                            (true, _) => dim(),
                            (false, true) => bold(),
                            (false, false) => Style::new(),
                        },
                    ),
                ],
                vec![Span::styled(right, dim())],
                width,
            );
            if selected {
                line.spans.insert(0, Span::styled(GUTTER, accent()));
            }
            line
        })
        .collect()
}

/// The branch row: `↑24 unpushed · 4 uncommitted`, each part left out when
/// it's zero, then `behind n` when the default branch moved on.
fn branch_summary(facts: &WorktreeFacts) -> Line<'static> {
    let mut parts: Vec<Vec<Span<'static>>> = Vec::new();
    if facts.ahead > 0 {
        parts.push(vec![
            Span::styled("↑", dim()),
            Span::raw(format!("{} unpushed", facts.ahead)),
        ]);
    }
    if facts.dirty > 0 {
        parts.push(vec![Span::raw(format!("{} uncommitted", facts.dirty))]);
    }
    if facts.behind > 0 {
        parts.push(vec![Span::raw(format!("behind {}", facts.behind))]);
    }
    if parts.is_empty() {
        return Line::raw("nothing to commit or push");
    }
    let mut spans = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.extend(part);
    }
    Line::from(spans)
}

/// `+a −r`, leaving out a side that's zero.
fn counts_text(added: usize, removed: usize) -> (String, String) {
    let added = if added > 0 {
        format!("+{added}")
    } else {
        String::new()
    };
    let removed = if removed > 0 {
        format!("−{removed}")
    } else {
        String::new()
    };
    (added, removed)
}

/// The last [`PATH_TAIL`] components of `path`, fewer while they don't fit
/// `width`, and the end of the file name when even that doesn't. The
/// components it drops are the ones every file in a repo shares.
fn tail(path: &str, width: usize) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    (1..=PATH_TAIL.min(parts.len()))
        .rev()
        .map(|n| parts[parts.len() - n..].join("/"))
        .find(|tail| tail.width() <= width)
        .unwrap_or_else(|| truncate_start(basename(path), width))
}

/// The counts at the right of a row: `+a` and `−r`, or `new` or `bin` when
/// no line counts exist. A folder's are DIM.
fn count_cells(counts: Counts, folder: bool) -> (Span<'static>, String) {
    let (added, removed) = counts_text(counts.added, counts.removed);
    if added.is_empty() && removed.is_empty() {
        let word = if counts.new > 0 {
            "new"
        } else if counts.binary > 0 {
            "bin"
        } else {
            ""
        };
        return (Span::styled(word, dim()), String::new());
    }
    let style = if folder { dim() } else { success() };
    (Span::styled(added, style), removed)
}

/// Columns the marker and its space take before a row's name.
const MARKER_WIDTH: usize = 3;

fn edit_letter(edit: Edit) -> &'static str {
    match edit {
        Edit::Modified => "M",
        Edit::Added => "A",
        Edit::Deleted => "D",
    }
}

/// Lazygit's `XY` code before a file's name: the staged letter in the
/// success colour, the unstaged one in the error colour, `??` for a file
/// git isn't tracking, and nothing for one already committed. A folder
/// shows a `●` in each column that has something pending under it.
fn marker(row: &TreeRow) -> Vec<Span<'static>> {
    let cell = |text: Option<&'static str>, colour| match text {
        Some(text) => Span::styled(text, Style::new().fg(colour)),
        None => Span::raw(" "),
    };
    let (palette_ok, palette_err) = (palette().success, palette().error);
    let mut spans = match row.node {
        Node::Folder { .. } => vec![
            cell((row.counts.staged > 0).then_some("●"), palette_ok),
            cell((row.counts.unstaged > 0).then_some("●"), palette_err),
        ],
        Node::File(_) => match row.state {
            FileState::Committed => vec![Span::raw("  ")],
            FileState::Untracked => vec![Span::styled("??", Style::new().fg(palette_err))],
            state => vec![
                cell(state.staged().map(edit_letter), palette_ok),
                cell(state.unstaged().map(edit_letter), palette_err),
            ],
        },
    };
    spans.push(Span::raw(" "));
    spans
}

/// [3]'s rows: `head` (the readiness row) when there is one, then `list`,
/// the changed files flat in `diff --numstat` order or as a tree of
/// folders, with counts in columns, each after a gutter. A file in `shared`
/// (one another agent changed too) carries a Yellow `∩` before its counts.
/// With a cursor (a row index) the rows scroll to keep it in view and its
/// gutter is marked. Without one the last row says how many more when they
/// don't all fit in `rows`.
fn changes(
    list: &[TreeRow],
    shared: &HashMap<PathBuf, Vec<&Entry>>,
    head: Option<Line<'static>>,
    width: usize,
    rows: usize,
    cursor: Option<usize>,
) -> Vec<Line<'static>> {
    let gutter = |row: usize| match cursor {
        Some(at) if at == row => Span::styled("▌", accent()),
        _ => Span::raw(" "),
    };
    let red = error();
    let columns: Vec<(Span<'static>, String)> = list
        .iter()
        .map(|row| count_cells(row.counts, matches!(row.node, Node::Folder { .. })))
        .collect();
    let added_width = columns.iter().map(|(a, _)| a.width()).max().unwrap_or(0);
    let removed_width = columns.iter().map(|(_, r)| r.width()).max().unwrap_or(0);
    let is_shared =
        |row: &TreeRow| matches!(row.node, Node::File(_)) && shared.contains_key(&row.path);
    let any_shared = list.iter().any(is_shared);
    let tree = list.iter().any(|r| matches!(r.node, Node::Folder { .. }));
    let skip = usize::from(head.is_some());
    let mut all: Vec<Line<'static>> = Vec::new();
    if let Some(head) = head {
        let mut line = worktree_row(head, width);
        line.spans[0] = gutter(0);
        all.push(line);
    }
    for (i, (row, (added, removed))) in list.iter().zip(columns).enumerate() {
        let folder = matches!(row.node, Node::Folder { .. });
        let mut right = Vec::new();
        if is_shared(row) {
            right.push(Span::styled("∩ ", warning()));
        } else if any_shared {
            right.push(Span::raw("  "));
        }
        right.push(Span::styled(
            format!("{:>added_width$}", added.content),
            added.style,
        ));
        if removed_width > 0 {
            let style = if folder { dim() } else { red };
            right.push(Span::styled(format!(" {removed:<removed_width$}"), style));
        }
        let room = width.saturating_sub(line_width(&right) + 1 + MARKER_WIDTH);
        let left = match row.node {
            Node::Folder { open } => {
                let arrow = if open { "▼ " } else { "▶ " };
                let indent = "  ".repeat(row.depth);
                let label = truncate_start(
                    &row.label,
                    room.saturating_sub(indent.width() + arrow.width()),
                );
                vec![
                    Span::raw(indent),
                    Span::styled(arrow, dim()),
                    Span::raw(label),
                ]
            }
            Node::File(_) if tree => {
                let indent = "  ".repeat(row.depth);
                let label = truncate_start(&row.label, room.saturating_sub(indent.width() + 2));
                vec![Span::raw(format!("{indent}  {label}"))]
            }
            Node::File(_) => vec![Span::raw(tail(&row.label, room))],
        };
        let left: Vec<Span<'static>> = marker(row).into_iter().chain(left).collect();
        let mut line = justify(left, right, width);
        line.spans.insert(0, gutter(skip + i));
        all.push(line);
    }
    let total = all.len();
    let (first, listed) = window(total, rows, cursor);
    match cursor {
        Some(_) => all.into_iter().skip(first).take(rows).collect(),
        None if total <= rows => all,
        None => {
            let mut shown: Vec<Line<'static>> = all.into_iter().take(listed).collect();
            if let Some(hidden) = NonZeroUsize::new(total - shown.len()) {
                let mut more = more_row(hidden, width);
                more.spans.insert(0, Span::raw(" "));
                shown.push(more);
            }
            shown
        }
    }
}

/// Which of `total` rows [3] shows in `rows` rows: the first, and how many
/// of the shown rows are rows rather than a closing `+k more`. With a cursor
/// the rows scroll to keep it in view.
fn window(total: usize, rows: usize, cursor: Option<usize>) -> (usize, usize) {
    match cursor {
        Some(at) => {
            let first = at
                .min(total.saturating_sub(1))
                .saturating_sub(rows.saturating_sub(1));
            (first, rows.min(total - first))
        }
        None if total <= rows => (0, total),
        None => (0, rows.saturating_sub(1)),
    }
}

/// What [3]'s title says: the totals, and how many files are shared.
struct Changes {
    diff: DiffStat,
    shared: usize,
    scope: ChangesScope,
    /// What the second tab is called.
    since: String,
}

/// The list's lines with the row each belongs to, the selected card's
/// lines, and how much of its agent's gist that card shows.
struct List {
    lines: Vec<(Line<'static>, Option<Cursor>)>,
    selected: Option<Range<usize>>,
    card: Shown,
}

fn list(deck: &Deck<'_>, snapshot: &Snapshot, rows: &[Row], cursor: Option<&Cursor>) -> List {
    let focused = sidebar_focused(deck.m) && deck.m.panel == Focused::Agents;
    let mut list = List {
        lines: Vec::new(),
        selected: None,
        card: Shown::Hidden,
    };
    for row in rows {
        let row_cursor = row.cursor();
        let selected = row_cursor.is_some() && row_cursor.as_ref() == cursor;
        let lines = match row {
            Row::Header(group) => vec![justify(
                vec![
                    Span::styled("▾ ", dim()),
                    Span::styled(group.title(), dim()),
                ],
                Vec::new(),
                deck.content,
            )],
            Row::Repo { key, fold } => match snapshot.repo(key) {
                Some(repo) => vec![deck.repo_line(repo, *fold, selected)],
                None => continue,
            },
            Row::Grouped(_, name) | Row::Entry { name, .. } | Row::Elsewhere(name) => {
                let Some((repo, entry)) = snapshot.entry(name) else {
                    continue;
                };
                let repo_name = repo.name().into_owned();
                let title = match row {
                    Row::Entry {
                        place: Place::Solo, ..
                    } => Title::Repo(repo_name),
                    Row::Elsewhere(_) => Title::Elsewhere(repo_name),
                    Row::Entry {
                        place: Place::Pinned,
                        ..
                    } if repo_name != entry.label() => Title::Pinned(repo_name),
                    Row::Grouped(..) if repo_name != entry.label() => Title::Grouped(repo_name),
                    Row::Grouped(..)
                    | Row::Entry {
                        place: Place::Nested,
                        ..
                    } => Title::Child,
                    _ => Title::Label,
                };
                let (lines, shown) = deck.card(entry, title, selected);
                if selected {
                    list.card = shown;
                }
                lines
            }
        };
        let start = list.lines.len();
        let gutter = match (selected, focused) {
            (true, true) => Span::styled("▌", accent()),
            (true, false) => Span::styled("▌", dim()),
            (false, _) => Span::raw(" "),
        };
        for mut line in lines {
            line.spans.insert(0, gutter.clone());
            list.lines.push((line, row_cursor.clone()));
        }
        if selected {
            list.selected = Some(start..list.lines.len());
        }
    }
    list
}

/// The rows inside [1]'s border at most, leaving [2] and [3] their
/// minimums, in a column `height` tall.
pub(super) fn list_room(height: u16) -> u16 {
    let kept = u16::try_from(DETAIL_MIN + CHANGES_MIN + READINESS_ROW + 6).unwrap_or(u16::MAX);
    height
        .saturating_sub(kept)
        .max(height.saturating_sub(2).min(LIST_MIN))
}

/// Rows [1] keeps when the column is too short for [2] and [3] their
/// minimums.
const LIST_MIN: u16 = 3;

/// A panel's place and its lines, each padded off the border.
struct Panel {
    area: Rect,
    lines: Vec<Line<'static>>,
}

/// The sidebar as laid out for one frame: the panels, which row each list
/// line selects, and the scroll to remember.
struct Laid {
    list: Panel,
    hits: Vec<(u16, Cursor)>,
    scroll: ListScroll,
    /// [2], while an agent with a turn is selected.
    agent: Option<Panel>,
    agent_rows: Vec<(u16, usize)>,
    change_rows: Vec<(u16, usize)>,
    /// The focused panel, its position clamped to what it shows.
    focus: Focused,
    /// Whether the column had room for [3].
    room: WorkRoom,
    /// [3], while the selected agent has changes and there's room.
    changes: Option<(Panel, Changes)>,
}

/// Rows [2] shows at most, whatever it has.
const AGENT_ROWS: usize = 6;

/// Stacks [1] sized to its rows (scrolled to keep the selection in view,
/// on the same screen row while it moves between groups when scrolling
/// allows), [2] with its rows up to a cap, and [3] in the rest. Without
/// [3], [2] takes the rest, and without an agent selected, [1] does. A
/// list with the keys scrolls to keep its cursor in view.
fn lay_out(m: &Model, area: Rect) -> Option<Laid> {
    let snapshot = m.snapshot()?;
    let height = usize::from(area.height);
    let room = usize::from(m.areas.sidebar.height);
    let deck = Deck::new(m, snapshot, usize::from(area.width).saturating_sub(2), room);
    let cursor = m.cursor();
    let list = list(&deck, snapshot, &m.rows(), cursor);
    let len = list.lines.len();
    let selected_line = list.selected.as_ref().map_or(0, |r| r.start);
    let preferred = match (&m.scroll.anchor, cursor) {
        (Some((was, at)), Some(now)) if was.follows(now) => selected_line.saturating_sub(*at),
        _ => m.scroll.offset,
    };
    let offset = scroll(preferred, selected_line, room, len);
    let visible = len.saturating_sub(offset).min(room).max(1);

    let selection = m
        .selected()
        .and_then(|(repo, entry)| entry.map(|entry| (repo, entry)));
    let rest = height.saturating_sub(visible + 2);
    let below = Rect::new(
        area.x,
        area.y + u16::try_from(visible + 2).unwrap_or(u16::MAX),
        area.width,
        u16::try_from(rest).unwrap_or(u16::MAX),
    );
    let lower = selection.map(|(repo, entry)| lower_panels(&deck, repo, entry, below));

    let mut hits = Vec::new();
    let mut lines = Vec::new();
    for (i, (line, row)) in list
        .lines
        .into_iter()
        .enumerate()
        .skip(offset)
        .take(visible)
    {
        let y = area.y + 1 + u16::try_from(i - offset).unwrap_or(0);
        hits.extend(row.map(|c| (y, c)));
        lines.push(line);
    }
    let anchor = cursor
        .cloned()
        .zip(list.selected.map(|r| r.start.saturating_sub(offset)));
    let list_height = if lower.is_some() { visible + 2 } else { height };
    let (agent, agent_rows, changes, change_rows, focus, room) = match lower {
        Some(lower) => (
            lower.agent,
            lower.agent_rows,
            lower.changes,
            lower.change_rows,
            lower.focus,
            lower.room,
        ),
        None => (
            None,
            Vec::new(),
            None,
            Vec::new(),
            Focused::Agents,
            WorkRoom::TooShort,
        ),
    };
    Some(Laid {
        list: Panel {
            area: Rect::new(
                area.x,
                area.y,
                area.width,
                u16::try_from(list_height).unwrap_or(u16::MAX),
            ),
            lines,
        },
        hits,
        scroll: ListScroll { offset, anchor },
        agent,
        agent_rows,
        focus,
        room,
        changes,
        change_rows,
    })
}

/// A row about the worktree rather than a file: DIM, behind the gutter
/// column the file rows have, cut to `width`.
fn worktree_row(line: Line<'static>, width: usize) -> Line<'static> {
    let mut spans = fit(line.spans, width);
    for span in &mut spans {
        span.style = dim();
    }
    spans.insert(0, Span::raw(" "));
    Line::from(spans)
}

/// `+k more`, DIM, as [3]'s last row when its files don't all fit. The key
/// that shows the rest is the footer's to offer.
fn more_row(hidden: NonZeroUsize, width: usize) -> Line<'static> {
    Line::styled(truncate_end(&format!("+{hidden} more"), width), dim())
}

/// What [3] has to show: the files and readiness, or why git couldn't be
/// read.
enum Work<'a> {
    Facts {
        facts: &'a WorktreeFacts,
        head: Option<Line<'static>>,
        list: std::sync::Arc<Vec<TreeRow>>,
    },
    Unreadable(String),
}

impl Work<'_> {
    /// The rows [3]'s cursor can be on.
    fn rows(&self) -> usize {
        match self {
            Self::Facts { head, list, .. } => usize::from(head.is_some()) + list.len(),
            Self::Unreadable(_) => 1,
        }
    }
}

/// What [3] has to say about the selected agent, if anything.
fn work_of<'m>(m: &'m Model, entry: &Entry) -> Option<Work<'m>> {
    match m.facts.get(entry.name()) {
        Some(Ok(facts)) => {
            let head = m.branch_row().then(|| branch_summary(facts));
            let list = m.change_list();
            (!list.is_empty() || head.is_some()).then_some(Work::Facts { facts, head, list })
        }
        Some(Err(e)) => Some(Work::Unreadable(format!("git: {}", first_line(e).0))),
        None => None,
    }
}

/// [2] and [3] as laid out under the list, with the focus clamped to them.
struct Lower {
    agent: Option<Panel>,
    /// The screen row of each of [2]'s rows shown, with its index.
    agent_rows: Vec<(u16, usize)>,
    changes: Option<(Panel, Changes)>,
    change_rows: Vec<(u16, usize)>,
    focus: Focused,
    room: WorkRoom,
}

/// [2] and [3] in `area`: [2] sized to its rows up to [`AGENT_ROWS`], [3]
/// in what's left when it has the rows to say something and the room, and
/// [2] in all of it when [3] has nothing.
fn lower_panels(deck: &Deck<'_>, repo: &Repo, entry: &Entry, area: Rect) -> Lower {
    let m = deck.m;
    let rest = usize::from(area.height);
    let (turn, turn_cursor) = deck.turn_rows(entry);
    let work = work_of(m, entry);
    let work_rows = work.as_ref().map_or(0, Work::rows);
    let want_turn = if turn.is_empty() {
        0
    } else {
        2 + turn.len().min(AGENT_ROWS)
    };
    // [3] keeps its readiness row and three files, or what it has of them,
    // and [2] a row, so when both can't fit [2] takes everything.
    let turn_floor = if turn.is_empty() { 0 } else { 3 };
    let work_least = if work_rows == 0 {
        0
    } else {
        (2 + work_rows).min(2 + READINESS_ROW + CHANGES_MIN)
    };
    let (turn_height, work_height) = if work_rows > 0 && rest >= turn_floor + work_least {
        let turn_height = if turn_floor == 0 {
            0
        } else {
            want_turn.min(rest - work_least)
        };
        (turn_height, rest - turn_height)
    } else if turn_floor > 0 && rest >= turn_floor {
        (rest, 0)
    } else {
        (0, 0)
    };
    let at = |y: usize, height: usize| {
        Rect::new(
            area.x,
            area.y + u16::try_from(y).unwrap_or(u16::MAX),
            area.width,
            u16::try_from(height).unwrap_or(u16::MAX),
        )
    };

    let mut focus = Focused::Agents;
    let mut agent_rows = Vec::new();
    let mut change_rows = Vec::new();
    let agent = (turn_height > 0).then(|| {
        let inside = turn_height - 2;
        let first = match (m.panel.listed(), turn_cursor) {
            (Focused::Agent { offset }, Some(row)) => {
                let first = scroll(offset, row, inside, turn.len());
                focus = Focused::Agent { offset: first };
                first
            }
            _ => 0,
        };
        let lines: Vec<Line<'static>> = turn.into_iter().skip(first).take(inside).collect();
        let top = at(0, turn_height).y + 1;
        agent_rows = (0..lines.len())
            .map(|i| (top + u16::try_from(i).unwrap_or(0), first + i))
            .collect();
        pad(at(0, turn_height), lines)
    });
    let changes = work.filter(|_| work_height > 0).map(|work| {
        let area = at(turn_height, work_height);
        let inside = work_height - 2;
        let cursor = match m.panel.listed() {
            Focused::Changes { file } => {
                let row = file.min(work_rows - 1);
                focus = Focused::Changes { file: row };
                Some(row)
            }
            _ => None,
        };
        let top = area.y + 1;
        let (first, listed) = window(work_rows, inside, cursor);
        change_rows = (0..listed)
            .map(|i| (top + u16::try_from(i).unwrap_or(0), first + i))
            .collect();
        match work {
            Work::Facts { facts, head, list } => {
                let shared = deck.overlaps(repo, entry);
                let lines = changes(&list, &shared, head, deck.content, inside, cursor);
                (
                    Panel { area, lines },
                    summary(facts, &shared, m.changes_scope),
                )
            }
            Work::Unreadable(text) => (
                Panel {
                    area,
                    lines: vec![worktree_row(Line::raw(text), deck.content)],
                },
                Changes {
                    diff: DiffStat::default(),
                    shared: 0,
                    scope: m.changes_scope,
                    since: since_label(None),
                },
            ),
        }
    });
    Lower {
        agent,
        agent_rows,
        changes,
        change_rows,
        focus,
        room: if work_height > 0 || work_rows == 0 {
            WorkRoom::Fits
        } else {
            WorkRoom::TooShort
        },
    }
}

/// What [3]'s title says about `facts`.
fn summary(
    facts: &WorktreeFacts,
    shared: &HashMap<PathBuf, Vec<&Entry>>,
    scope: ChangesScope,
) -> Changes {
    Changes {
        diff: match scope {
            ChangesScope::Uncommitted => facts.pending_diff(),
            ChangesScope::Since => facts.diff(),
        },
        shared: shared.len(),
        scope,
        since: since_label(facts.base.as_deref()),
    }
}

/// The second tab of [3]: `Since main`.
fn since_label(base: Option<&str>) -> String {
    format!("Since {}", base.unwrap_or("fork"))
}

/// `[3]` then its tabs as [1]'s title has them: the active one BOLD, a DIM
/// `·` between. They all show while they fit `width`, else only the active
/// one, so moving the cursor never changes the form.
fn changes_tabs(scope: ChangesScope, since: &str, width: usize) -> Vec<Span<'static>> {
    let tabs = [
        (ChangesScope::Uncommitted, "Uncommitted".to_string()),
        (ChangesScope::Since, since.to_string()),
    ];
    let all = 4 + tabs.iter().map(|(_, n)| n.width()).sum::<usize>() + 3 * (tabs.len() - 1);
    if width < all {
        let active = tabs
            .into_iter()
            .find(|(s, _)| *s == scope)
            .map_or_else(String::new, |(_, n)| n);
        return vec![Span::raw("[3] "), Span::styled(active, bold())];
    }
    let mut title = vec![Span::raw("[3] ")];
    for (i, (tab, name)) in tabs.into_iter().enumerate() {
        if i > 0 {
            title.push(Span::styled(" · ", dim()));
        }
        let style = if tab == scope {
            bold()
        } else {
            Style::new().fg(Color::Reset)
        };
        title.push(Span::styled(name, style));
    }
    title
}

/// A panel holding `lines`, each a column off the left border.
fn pad(area: Rect, lines: Vec<Line<'static>>) -> Panel {
    let lines = lines
        .into_iter()
        .map(|mut line| {
            if line
                .spans
                .first()
                .is_none_or(|s| !s.content.starts_with(GUTTER))
            {
                line.spans.insert(0, Span::raw(" "));
            }
            line
        })
        .collect();
    Panel { area, lines }
}

/// The other agents in the repo that changed `path` too, by label.
pub(super) fn others_on(m: &Model, name: &SessionName, path: &Path) -> Vec<String> {
    let Some((repo, _)) = m.snapshot().and_then(|s| s.entry(name)) else {
        return Vec::new();
    };
    repo.entries()
        .iter()
        .filter(|e| e.name() != name)
        .filter(|e| match m.facts.get(e.name()) {
            Some(Ok(facts)) => facts.changed.iter().any(|c| c.path == path),
            _ => false,
        })
        .map(|e| e.label().to_string())
        .collect()
}

/// What the selected agent says about what it's doing, in full.
pub(super) fn turn_message(m: &Model, entry: &Entry) -> Option<String> {
    let status = m.status(entry);
    let heard = m.activity(entry.name()).map_or(m.now, |a| a.heard);
    Gist::of(&status, heard, m.now)?.message()
}

/// The agent's subagents as tree rows, for [0]'s turn document.
pub(super) fn subagent_rows(m: &Model, entry: &Entry, width: usize) -> Vec<Line<'static>> {
    m.activity(entry.name()).map_or_else(Vec::new, |a| {
        tree(
            a.subagents.values().collect(),
            Form::Long,
            width,
            m.now,
            None,
        )
    })
}

/// `queued: <first words>` for an agent with messages waiting, cut to
/// `width`.
pub(super) fn queued_line(m: &Model, name: &SessionName, width: usize) -> Option<String> {
    let outbox = m.outboxes.get(name)?;
    if outbox.stalled(m.activity(name).map(|a| a.events)) {
        return Some(truncate_end(
            &format!("queued · waiting for {name} to report"),
            width,
        ));
    }
    let queue = &outbox.queue;
    let first = crate::update::first_words(queue.front()?.as_ref());
    let more = match queue.len() {
        1 => String::new(),
        n => format!(" +{}", n - 1),
    };
    let room = width.saturating_sub(more.width());
    Some(format!(
        "{}{more}",
        truncate_end(&format!("queued: {first}"), room)
    ))
}

/// The selected agent's state as one line: its glyph, name, what it's doing
/// and for how long, cut to `width` by dropping what it's doing first.
pub(super) fn status_line(m: &Model, entry: &Entry, width: usize) -> Vec<Span<'static>> {
    let status = m.status(entry);
    let heard = m.activity(entry.name()).map_or(m.now, |a| a.heard);
    let gist = Gist::of(&status, heard, m.now);
    // A delegating gist already ends in how long it's been quiet.
    let time = match gist {
        Some(Gist::Delegating { .. }) => Vec::new(),
        _ => time_in_state(&status, m.now),
    };
    let mut spans = vec![
        glyph(&status),
        Span::raw(" "),
        Span::styled(entry.label().to_string(), bold()),
    ];
    let room = width.saturating_sub(line_width(&spans) + line_width(&time) + 4);
    if let Some(gist) = gist
        && room >= GIST_MIN
    {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(gist.line(Form::Long, room).0, gist.style()));
    }
    if !time.is_empty() {
        spans.push(Span::raw("  "));
        spans.extend(time);
    }
    spans
}

/// The least room what an agent is doing gets beside its name, else only
/// the name and time show.
const GIST_MIN: usize = 14;

/// The three panels in `area`, the one `j`/`k` act on with `border` (the
/// accent while the sidebar has the keys) and the others DIM.
pub(super) fn draw(frame: &mut Frame<'_>, m: &mut Model, area: Rect, border: Style) {
    let nav = sidebar_focused(m);
    let style_of = |focused: bool| if focused { border } else { dim() };
    let agents = matches!(m.panel, Focused::Agents);
    let agent = matches!(m.panel.listed(), Focused::Agent { .. });
    let files = matches!(m.panel.listed(), Focused::Changes { .. });
    // Overview's folds and the tier depend on the list's room, so it's
    // measured before anything asks for the rows.
    m.areas.sidebar = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        list_room(area.height),
    );
    let title = sidebar_title(m, usize::from(area.width).saturating_sub(4));
    let corner = sidebar_count(
        m,
        style_of(agents),
        usize::from(area.width).saturating_sub(4),
    );
    let Some(laid) = lay_out(m, area) else {
        m.work_room = WorkRoom::TooShort;
        frame.render_widget(panel(title, corner, style_of(agents), nav && agents), area);
        return;
    };
    // A focused panel that stopped existing hands the focus back.
    let laid = if std::mem::discriminant(&laid.focus) == std::mem::discriminant(&m.panel.listed()) {
        laid
    } else {
        m.panel = laid.focus;
        match lay_out(m, area) {
            Some(laid) => laid,
            None => return,
        }
    };
    m.areas.page = match laid.focus {
        Focused::Agents => usize::from(laid.list.area.height),
        Focused::Agent { .. } => laid
            .agent
            .as_ref()
            .map_or(0, |p| usize::from(p.area.height)),
        Focused::Changes { .. } => laid
            .changes
            .as_ref()
            .map_or(0, |(p, _)| usize::from(p.area.height)),
        Focused::Main(_) => 0,
    }
    .saturating_sub(2);
    let agent_area = laid.agent.as_ref().map_or(Rect::default(), |p| p.area);
    let changes_area = laid
        .changes
        .as_ref()
        .map_or(Rect::default(), |(p, _)| p.area);
    let list = panel(title, corner, style_of(agents), nav && agents);
    frame.render_widget(Paragraph::new(laid.list.lines).block(list), laid.list.area);
    if let Some(Panel { area, lines }) = laid.agent {
        let title = vec![Span::raw("[2] "), Span::styled("Turn", bold())];
        let block = panel(title, Vec::new(), style_of(agent), nav && agent);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }
    if let Some((Panel { area, lines }, changes)) = laid.changes {
        let (added, removed) = counts_text(changes.diff.added, changes.diff.removed);
        let mut title = changes_tabs(
            changes.scope,
            &changes.since,
            usize::from(area.width).saturating_sub(4),
        );
        for (count, colour) in [(added, palette().success), (removed, palette().error)] {
            if !count.is_empty() {
                title.push(Span::raw(" "));
                title.push(Span::styled(count, Style::new().fg(colour)));
            }
        }
        if changes.shared > 0 {
            title.push(Span::raw("  "));
            title.push(Span::styled(format!("∩ {}", changes.shared), warning()));
        }
        let title = fit(title, usize::from(area.width).saturating_sub(4));
        let block = panel(title, Vec::new(), style_of(files), nav && files);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }
    m.panel = m.panel.with_list(laid.focus);
    m.work_room = laid.room;
    m.scroll = laid.scroll;
    m.areas.rows = laid.hits;
    m.areas.agent_rows = laid.agent_rows;
    m.areas.change_rows = laid.change_rows;
    m.areas.agent_panel = agent_area;
    m.areas.changes_panel = changes_area;
}

/// Whether text `wide` columns wide fits beside `title` in a border
/// `width` wide: two corners, both titles' padding and one `─` between
/// them, so neither overwrites the other and they never touch.
pub(super) fn fits_beside(title: &[Span<'static>], wide: usize, width: usize) -> bool {
    line_width(title) + 7 + wide <= width
}

/// `spans` with a space each side, as the border titles have.
pub(super) fn pad_title(mut spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    if !spans.is_empty() {
        spans.insert(0, Span::raw(" "));
        spans.push(Span::raw(" "));
    }
    spans
}

#[cfg(test)]
mod tests {
    use agent_core::ports::Lines;

    fn flat(facts: &WorktreeFacts) -> Vec<TreeRow> {
        crate::tree::build(
            &facts.changed,
            crate::tree::ChangesView::Flat,
            &std::collections::HashSet::new(),
        )
    }

    use std::collections::{HashMap, HashSet};

    use agent_core::activity::{AgentId, State};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Cell;
    use ratatui::style::{Color, Modifier};

    use super::*;
    use crate::fixtures::{fleet, name, reported};
    use crate::model::{Group, Overlay, View, Visibility};

    const SIZES: [(u16, u16); 3] = [(200, 50), (100, 30), (80, 24)];
    const FLEETS: [usize; 5] = [0, 1, 2, 8, 20];

    fn n(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).expect("succeeds")
    }

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn texts(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(text).collect()
    }

    /// Draws the whole screen, as the app does.
    fn render(m: &mut Model, (width, height): (u16, u16)) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a backend");
        terminal
            .draw(|frame| super::super::draw(frame, m))
            .expect("draws");
        terminal
    }

    /// The sidebar's column on a screen `size` big, as last drawn: the
    /// left edge above the status bar.
    fn column(m: &Model, (_, height): (u16, u16)) -> Rect {
        Rect::new(0, 0, m.areas.sidebar.width + 2, height - 1)
    }

    fn laid(m: &mut Model, size: (u16, u16)) -> Laid {
        render(m, size);
        lay_out(m, column(m, size)).expect("succeeds")
    }

    fn agent_lines(laid: &Laid) -> Vec<String> {
        laid.agent
            .as_ref()
            .map(|panel| texts(&panel.lines))
            .unwrap_or_default()
    }

    /// Each list row's cursor and how many lines its card takes.
    fn heights(m: &Model) -> HashMap<String, usize> {
        let area = m.areas.sidebar;
        let snapshot = m.snapshot().expect("loaded");
        let deck = Deck::new(
            m,
            snapshot,
            usize::from(area.width),
            usize::from(area.height),
        );
        let list = list(&deck, snapshot, &m.rows(), m.cursor());
        let mut heights = HashMap::new();
        for (_, cursor) in list.lines {
            if let Some(cursor) = cursor {
                *heights.entry(format!("{cursor:?}")).or_default() += 1;
            }
        }
        heights
    }

    #[test]
    fn the_tier_gives_second_lines_while_the_list_fits() {
        assert_eq!(tier(Extent { rows: 19, live: 7 }, 36), Tier::Two);
        assert_eq!(
            tier(Extent { rows: 31, live: 18 }, 41),
            Tier::Waiting { spare: 10 }
        );
        assert_eq!(tier(Extent { rows: 41, live: 3 }, 41), Tier::One);
        assert_eq!(tier(Extent { rows: 0, live: 0 }, 4), Tier::Two);
    }

    #[test]
    fn the_list_leaves_the_other_panels_their_minimums() {
        assert_eq!(list_room(49), 35);
        assert_eq!(list_room(23), 9);
        assert_eq!(
            list_room(10),
            3,
            "a short column keeps a few rows for the list"
        );
        assert_eq!(list_room(4), 2);
    }

    #[test]
    fn second_lines_keep_a_paths_end_and_a_commands_start() {
        let path = Target::Path("src/payroll/rules/calc.ts".into());
        let edit = Gist::Tool {
            name: "Edit",
            target: Some(&path),
        };
        assert_eq!(
            edit.line(Form::Long, 20),
            ("Edit  …rules/calc.ts".to_string(), Shown::Cut)
        );
        assert_eq!(
            edit.line(Form::Short, 20),
            ("Edit calc.ts".to_string(), Shown::Cut)
        );
        let command = Target::Text("pnpm test --filter triage".into());
        let bash = Gist::Tool {
            name: "Bash",
            target: Some(&command),
        };
        assert_eq!(
            bash.line(Form::Long, 20),
            ("Bash  pnpm test --f…".to_string(), Shown::Cut)
        );
        let delegating = Gist::Delegating {
            subagents: n(1),
            quiet: "22s".into(),
        };
        assert_eq!(delegating.line(Form::Long, 40).0, "↳1 subagent · quiet 22s");
        assert_eq!(delegating.message(), None, "the tree says more");
    }

    #[test]
    fn a_finished_turn_reads_its_first_line_and_says_all_of_it() {
        let done = Gist::Done(Some("## **Done**\n\nAdded `retry`.\nTests pass."));
        assert_eq!(done.line(Form::Long, 40), ("Done".to_string(), Shown::Cut));
        assert_eq!(
            done.message().as_deref(),
            Some("Done\n\nAdded retry.\nTests pass."),
            "[0] shows all of it"
        );
        assert_eq!(
            Gist::Failed(&Failure::RateLimit).line(Form::Short, 24),
            ("rate limited".to_string(), Shown::Whole)
        );
    }

    #[test]
    fn the_branch_row_reads_unpushed_and_uncommitted_and_leaves_out_zeros() {
        let read = |ahead, dirty, behind| {
            text(&branch_summary(&WorktreeFacts {
                ahead,
                dirty,
                behind,
                ..WorktreeFacts::default()
            }))
        };
        assert_eq!(read(24, 4, 0), "↑24 unpushed · 4 uncommitted");
        assert_eq!(read(0, 2, 0), "2 uncommitted");
        assert_eq!(read(3, 0, 0), "↑3 unpushed");
        assert_eq!(read(3, 0, 2), "↑3 unpushed · behind 2");
        assert_eq!(read(0, 0, 0), "nothing to commit or push");
    }

    #[test]
    fn changes_title_shows_both_tabs_while_they_fit_and_only_the_active_one_below_that() {
        use crate::tree::ChangesScope::{Since, Uncommitted};
        let title = |scope, width| text(&Line::from(changes_tabs(scope, "Since main", width)));
        assert_eq!(title(Uncommitted, 40), "[3] Uncommitted · Since main");
        assert_eq!(title(Since, 28), "[3] Uncommitted · Since main");
        assert_eq!(title(Uncommitted, 27), "[3] Uncommitted");
        assert_eq!(title(Since, 27), "[3] Since main");
        let bold_of = |scope| {
            changes_tabs(scope, "Since main", 40)
                .into_iter()
                .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
                .map(|s| s.content.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(bold_of(Uncommitted), ["Uncommitted"]);
        assert_eq!(bold_of(Since), ["Since main"]);
    }

    #[test]
    fn each_tab_totals_its_own_files_and_both_keep_the_branch_row_first() {
        use agent_core::ports::{Change, Lines};
        let change = |path: &str, added, removed| Change {
            path: path.into(),
            lines: Lines::Counted { added, removed },
            state: FileState::Committed,
        };
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.changes_view = crate::tree::ChangesView::Flat;
        let Some(Ok(facts)) = m.facts.get_mut(&name("obs-layout-4")) else {
            panic!("facts")
        };
        facts.ahead = 24;
        facts.dirty = 4;
        facts.base = Some("main".into());
        facts.changed = vec![change("a.rs", 50, 5), change("b.rs", 7, 1)];
        facts.pending = vec![change("b.rs", 2, 0)];
        let mut shown = Vec::new();
        for _ in 0..2 {
            let laid = laid(&mut m, (200, 50));
            let (panel, changes) = laid.changes.as_ref().expect("[3] shows");
            shown.push((
                texts(&panel.lines)[0].clone(),
                changes.diff.files,
                changes.diff.added,
                changes.diff.removed,
                panel.lines.len(),
            ));
            m.toggle_changes_scope();
        }
        assert_eq!(shown[0].0, " ↑24 unpushed · 4 uncommitted");
        assert_eq!(shown[1].0, shown[0].0, "the branch row leads both tabs");
        assert_eq!(shown[0].1, 1);
        assert_eq!((shown[0].2, shown[0].3), (2, 0));
        assert_eq!(shown[0].4, 2, "the branch row and one file");
        assert_eq!((shown[1].1, shown[1].2, shown[1].3), (2, 57, 6));
        assert_eq!(shown[1].4, 3);
    }

    #[test]
    fn changed_files_keep_their_meaningful_tail() {
        assert_eq!(
            tail("agent-tui/crates/core/src/activity.rs", 40),
            "core/src/activity.rs"
        );
        assert_eq!(
            tail("agent-tui/crates/core/src/activity.rs", 15),
            "src/activity.rs"
        );
        assert_eq!(
            tail("agent-tui/crates/core/src/activity.rs", 9),
            "…ivity.rs"
        );
        assert_eq!(tail("Cargo.toml", 20), "Cargo.toml");
    }

    fn facts() -> WorktreeFacts {
        let change = |path: &str, lines| agent_core::ports::Change {
            path: path.into(),
            lines,
            state: agent_core::ports::FileState::Committed,
        };
        let mut facts = WorktreeFacts {
            changed: vec![
                change(
                    "a/src/payroll/calc.ts",
                    Lines::Counted {
                        added: 98,
                        removed: 52,
                    },
                ),
                change(
                    "a/src/payroll/calc.test.ts",
                    Lines::Counted {
                        added: 24,
                        removed: 0,
                    },
                ),
                change("logo.png", Lines::Binary),
                change("notes.md", Lines::Untracked),
            ],
            ..WorktreeFacts::default()
        };
        let states = [
            FileState::Both {
                staged: Edit::Modified,
                unstaged: Edit::Modified,
            },
            FileState::Staged(Edit::Added),
            FileState::Committed,
            FileState::Untracked,
        ];
        for (change, state) in facts.changed.iter_mut().zip(states) {
            change.state = state;
        }
        facts
    }

    #[test]
    fn changes_line_up_their_counts_and_say_how_many_more() {
        let facts = facts();
        let none = HashMap::new();
        assert_eq!(
            texts(&changes(&flat(&facts), &none, None, 34, 4, None)),
            [
                " MM src/payroll/calc.ts     +98 −52",
                " A  payroll/calc.test.ts    +24    ",
                "    logo.png                bin    ",
                " ?? notes.md                new    ",
            ]
        );
        let short = changes(&flat(&facts), &none, None, 34, 3, None);
        assert_eq!(texts(&short)[2], " +2 more");
        let cursor = changes(&flat(&facts), &none, None, 34, 2, Some(3));
        assert_eq!(
            texts(&cursor),
            [
                "    logo.png                bin    ",
                "▌?? notes.md                new    ",
            ],
            "the rows scroll to keep the cursor in view"
        );
    }

    #[test]
    fn markers_colour_the_staged_and_unstaged_parts() {
        let facts = facts();
        let rows = flat(&facts);
        let colours = |row: &TreeRow| -> Vec<(String, Option<Color>)> {
            marker(row)
                .into_iter()
                .map(|s| (s.content.to_string(), s.style.fg))
                .collect()
        };
        let ok = Some(palette().success);
        let err = Some(palette().error);
        assert_eq!(
            colours(&rows[0]),
            [("M".into(), ok), ("M".into(), err), (" ".into(), None)]
        );
        assert_eq!(
            colours(&rows[1]),
            [("A".into(), ok), (" ".into(), None), (" ".into(), None)]
        );
        assert_eq!(colours(&rows[3]), [("??".into(), err), (" ".into(), None)]);
    }

    #[test]
    fn folders_mark_what_is_pending_under_them() {
        let mut facts = facts();
        facts.changed[0].path = "a/calc.ts".into();
        facts.changed[1].path = "b/new.ts".into();
        facts.changed[2].path = "c/old.png".into();
        let rows = crate::tree::build(
            &facts.changed,
            crate::tree::ChangesView::Tree,
            &HashSet::new(),
        );
        let folder = |label: &str| {
            let row = rows.iter().find(|r| r.label == label).expect("a row");
            marker(row)
                .into_iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        assert_eq!(folder("a"), "●● ");
        assert_eq!(folder("b"), "●  ");
        assert_eq!(folder("c"), "   ");
    }

    #[test]
    fn markers_never_push_a_row_past_the_width() {
        let facts = facts();
        let none = HashMap::new();
        for width in [20, 26, 34, 60] {
            for line in changes(&flat(&facts), &none, None, width, 4, None) {
                assert!(line.width() <= width + 1, "{width}: {:?}", text(&line));
            }
        }
    }

    #[test]
    fn every_size_and_fleet_fits_its_column() {
        let panels = [
            Focused::Agents,
            Focused::Agent { offset: 0 },
            Focused::Changes { file: 0 },
        ];
        for size in SIZES {
            for agents in FLEETS {
                for (view, panel) in View::ALL
                    .into_iter()
                    .flat_map(|view| panels.map(|panel| (view, panel)))
                {
                    let mut m = fleet(agents);
                    while m.view != view {
                        m.toggle_view();
                    }
                    m.panel = panel;
                    let context = format!("{size:?}, {agents} agents, {view:?}, {panel:?}");
                    let laid = laid(&mut m, size);
                    let column = column(&m, size);
                    let focused_lower = match laid.focus {
                        Focused::Agents | Focused::Main(_) => None,
                        Focused::Agent { .. } => Some(1),
                        Focused::Changes { .. } => Some(2),
                    };
                    let mut panels = vec![&laid.list];
                    panels.extend(laid.agent.as_ref());
                    panels.extend(laid.changes.as_ref().map(|(p, _)| p));
                    let mut y = column.y;
                    for (i, panel) in panels.into_iter().enumerate() {
                        let may_collapse = i > 0 && focused_lower != Some(i);
                        assert!(
                            may_collapse || panel.area.height >= 2,
                            "{context}: panel {i} is {} tall",
                            panel.area.height
                        );
                        assert_eq!(panel.area.y, y, "{context}: panels stack");
                        let collapsed = panel.area.height == 1 && panel.lines.is_empty();
                        assert!(
                            collapsed || panel.lines.len() + 2 <= usize::from(panel.area.height),
                            "{context}: {} lines in {:?}",
                            panel.lines.len(),
                            panel.area
                        );
                        for line in &panel.lines {
                            assert!(
                                line.width() < usize::from(column.width) - 1,
                                "{context}: {:?} is too wide",
                                text(line)
                            );
                        }
                        y += panel.area.height;
                    }
                    assert_eq!(y, column.y + column.height, "{context}: the panels fill it");
                    if agents > 0 {
                        assert!(
                            texts(&laid.list.lines).iter().any(|l| l.starts_with('▌')),
                            "{context}: the selection shows"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn moving_the_selection_never_changes_a_cards_height() {
        for size in SIZES {
            for agents in FLEETS {
                let mut m = fleet(agents);
                m.show_view(View::Overview);
                render(&mut m, size);
                for _ in 0..agents.min(12) {
                    let before = heights(&m);
                    m.move_in_panel(1);
                    render(&mut m, size);
                    for (cursor, height) in &heights(&m) {
                        if let Some(was) = before.get(cursor) {
                            assert_eq!(was, height, "{size:?}, {agents} agents: {cursor:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_tier_holds_while_agents_change_state() {
        for ((_, height), view) in SIZES
            .into_iter()
            .flat_map(|size| View::ALL.map(|view| (size, view)))
        {
            let rows = usize::from(list_room(height - 1));
            let mut m = fleet(20);
            while m.view != view {
                m.toggle_view();
            }
            let before = tier(m.extent(), rows);
            let everyone_done: HashMap<_, _> = m
                .snapshot()
                .expect("succeeds")
                .entries()
                .map(|(_, e)| (e.name().clone(), reported(State::Done { summary: None }, 5)))
                .collect();
            m.activities = Some(everyone_done);
            m.revalidate();
            assert_eq!(tier(m.extent(), rows), before, "{rows} rows, {view:?}");
        }
    }

    #[test]
    fn the_title_strip_follows_the_width_not_the_selection() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("heucristic0")));
        let text =
            |spans: &[Span<'_>]| spans.iter().map(|s| s.content.as_ref()).collect::<String>();
        assert_eq!(
            text(&super::super::sidebar_title(&m, 29)),
            "[1] heucristic0"
        );
        assert_eq!(
            text(&super::super::sidebar_title(&m, 30)),
            "[1] Agents · heucris… · Pinned"
        );
        assert_eq!(
            text(&super::super::sidebar_title(&m, 40)),
            "[1] Agents · heucristic0 · Pinned"
        );
        m.show_view(View::Overview);
        assert_eq!(text(&super::super::sidebar_title(&m, 29)), "[1] Agents");
        m.show_view(View::Pinned);
        assert_eq!(text(&super::super::sidebar_title(&m, 29)), "[1] Pinned");
        for width in [29, 30, 40] {
            let before = text(&super::super::sidebar_title(&m, width)).contains('·');
            m.set_cursor(Cursor::Entry(name("agent-tui1")));
            let after = text(&super::super::sidebar_title(&m, width)).contains('·');
            assert_eq!(before, after, "{width} wide");
        }
    }

    fn after_gutter(line: &str) -> String {
        line.chars().skip(1).collect()
    }

    #[test]
    fn repo_children_are_indented_under_their_repo() {
        let mut m = fleet(8);
        m.show_view(View::Overview);
        let lines = texts(&laid(&mut m, (200, 50)).list.lines);
        let repo = lines
            .iter()
            .position(|l| l.contains("▾ agent-tui0"))
            .expect("succeeds");
        assert!(
            after_gutter(&lines[repo + 1]).starts_with("  ")
                && lines[repo + 1].contains("agent-tui0"),
            "{}",
            lines.join("\n")
        );
        assert!(
            after_gutter(&lines[repo + 2]).starts_with("    "),
            "line 2 sits under the name"
        );
        assert!(lines[1].starts_with("   ◆ heucristic0"), "{}", lines[1]);
    }

    #[test]
    fn a_selection_leaving_needs_you_keeps_its_row_when_the_list_scrolls() {
        let size = (80, 24);
        let mut m = fleet(80);
        m.show_view(View::Overview);
        render(&mut m, size);
        let agent = name("auth-refresh-2");
        m.set_cursor(Cursor::Entry(agent.clone()));
        assert_eq!(
            m.cursor(),
            Some(&Cursor::Grouped(Group::NeedsYou, agent.clone()))
        );
        render(&mut m, size);
        let row = |m: &Model| {
            m.areas
                .rows
                .iter()
                .find(|(_, c)| c.entry_name() == Some(&agent))
                .map(|(y, _)| *y)
        };
        let before = row(&m);
        assert!(before.is_some());
        let mut activities = m.activities.clone().expect("succeeds");
        activities.insert(agent.clone(), reported(State::Idle, 1));
        m.activities = Some(activities);
        m.revalidate();
        assert_eq!(m.cursor(), Some(&Cursor::Entry(agent.clone())));
        render(&mut m, size);
        assert_eq!(row(&m), before);
    }

    #[test]
    fn the_agent_panel_shows_state_tree_and_staleness() {
        let mut m = fleet(8);
        m.show_view(View::Overview);
        let mut activities = m.activities.clone().expect("succeeds");
        activities.insert(
            name("payroll-fix-3"),
            reported(State::Working { tools: Vec::new() }, 400),
        );
        m.activities = Some(activities);
        let list = laid(&mut m, (200, 50)).list.lines;
        let failed = texts(&list)
            .iter()
            .position(|l| l.contains("! rate-limit-1"))
            .expect("succeeds");
        assert_eq!(texts(&list)[failed + 1].trim(), "rate limited");
        let stale = list
            .iter()
            .find(|l| text(l).contains("payroll-fix-3"))
            .expect("succeeds");
        let time: Vec<_> = stale.spans.iter().rev().take(2).collect();
        assert_eq!(time[0].content, "?");
        assert_eq!(time[0].style.fg, Some(Color::Yellow));
        assert_eq!(time[1].content, "6m");

        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let lines = agent_lines(&laid(&mut m, (200, 50)));
        assert!(
            lines[0].trim_start().starts_with("› go"),
            "the card already says what it's doing, so the prompt leads\n{}",
            lines.join("\n")
        );
        let tree: Vec<&String> = lines
            .iter()
            .filter(|l| l.trim_start().starts_with(['├', '└']))
            .collect();
        assert_eq!(tree.len(), 3, "{}", lines.join("\n"));
        assert!(tree[2].contains("Adversarial review:…  general-purpose  30s"));
        assert!(
            tree[0].contains("general-purpose") && tree[0].trim_end().ends_with("50s"),
            "a subagent with no description shows its kind in the description column\n{}",
            tree[0]
        );
    }

    #[test]
    fn shared_files_are_marked_in_changes_and_named_in_the_diffs_title() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("rate-limit-1")));
        let first = laid(&mut m, (200, 50));
        let (panel, changes) = first.changes.as_ref().expect("[3] shows");
        assert_eq!(changes.shared, 1);
        let flow = panel
            .lines
            .iter()
            .find(|l| text(l).contains("flow.ts"))
            .expect("flow.ts is listed");
        let marker = flow
            .spans
            .iter()
            .find(|s| s.content == "∩ ")
            .expect("a shared marker");
        assert_eq!(marker.style.fg, Some(Color::Yellow));
        assert!(
            !agent_lines(&first).iter().any(|l| l.contains("overlap")),
            "[2] no longer carries git"
        );

        assert_eq!(
            others_on(&m, &name("rate-limit-1"), Path::new("src/triage/flow.ts")),
            ["auth-refresh-2", "obs-layout-4", "payroll-fix-3"].map(String::from),
            "who else changed it, for the diff's title"
        );
    }

    #[test]
    fn readiness_opens_changes_and_shows_without_files() {
        let mut m = fleet(8);
        m.show_view(View::Overview);
        m.set_cursor(Cursor::Entry(name("payroll-fix-3")));
        let first = laid(&mut m, (200, 50));
        let (panel, _) = first.changes.as_ref().expect("[3] shows");
        let lines = texts(&panel.lines);
        assert!(
            lines[0].starts_with(' ') && lines[0].contains("uncommitted"),
            "readiness leads [3], behind the gutter\n{}",
            lines.join("\n")
        );
        assert!(
            !agent_lines(&first)
                .iter()
                .any(|l| l.contains("uncommitted")),
            "and left [2]"
        );

        let done = name("auth-refresh-2");
        m.set_cursor(Cursor::Entry(done.clone()));
        m.set_facts(
            &done,
            Ok(WorktreeFacts {
                ahead: 0,
                ..WorktreeFacts::default()
            }),
        );
        let first = laid(&mut m, (200, 50));
        let (panel, changes) = first.changes.as_ref().expect("[3] shows");
        assert_eq!(changes.diff.files, 0);
        assert_eq!(texts(&panel.lines), [" nothing to commit or push"]);
        assert!(
            panel.lines[0].spans[1]
                .style
                .add_modifier
                .contains(Modifier::DIM),
            "a worktree row isn't a file"
        );
        assert_eq!(first.focus, Focused::Agents);
    }

    #[test]
    fn the_turn_panel_is_a_row_for_the_turn_then_one_per_subagent() {
        let mut m = fleet(8);
        m.show_view(View::Overview);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let first = laid(&mut m, (200, 50));
        let lines = agent_lines(&first);
        assert!(
            lines[0].trim_start().starts_with("› go"),
            "{}",
            lines.join("\n")
        );
        assert!(lines[1].trim_start().starts_with('├'));
        assert!(lines[3].trim_start().starts_with('└'));
        assert_eq!(lines.len(), 4, "{}", lines.join("\n"));
        assert!(
            !lines.iter().any(|l| l.contains('▌')),
            "no cursor mark while [1] has the keys"
        );

        m.panel = Focused::Agent { offset: 0 };
        let lines = agent_lines(&laid(&mut m, (200, 50)));
        assert!(
            lines[0].starts_with('▌'),
            "the cursor starts on the turn\n{}",
            lines.join("\n")
        );
        m.subagent = Some(AgentId::new("s1"));
        let lines = agent_lines(&laid(&mut m, (200, 50)));
        assert!(lines[2].starts_with('▌'), "{}", lines.join("\n"));
        assert_eq!(lines.iter().filter(|l| l.starts_with('▌')).count(), 1);

        m.subagent = Some(AgentId::new("s0"));
        for height in [14, 16, 20] {
            let lines = agent_lines(&laid(&mut m, (80, height)));
            assert!(
                lines.iter().any(|l| l.starts_with('▌') && l.contains('└')),
                "{height} rows: the cursor's row stays in view\n{}",
                lines.join("\n")
            );
        }
    }

    #[test]
    fn every_colour_comes_from_the_palette() {
        let palette = crate::palette::Palette {
            accent: Color::Rgb(1, 1, 1),
            dim: crate::palette::Secondary::Colour(Color::Rgb(2, 2, 2)),
            warning: Color::Rgb(3, 3, 3),
            error: Color::Rgb(4, 4, 4),
            success: Color::Rgb(5, 5, 5),
        };
        let ours = [1, 2, 3, 4, 5].map(|n| Some(Color::Rgb(n, n, n)));
        for size in SIZES {
            for (panel, overlay) in [
                (Focused::Agents, Overlay::None),
                (Focused::Agent { offset: 0 }, Overlay::None),
                (
                    Focused::Agents,
                    Overlay::Help(crate::model::HelpState::default()),
                ),
            ] {
                let mut m = fleet(8);
                m.palette = palette;
                m.panel = panel;
                m.overlay = overlay;
                m.error("failed".to_string());
                let terminal = render(&mut m, size);
                let buffer = terminal.backend().buffer();
                let mut used = HashSet::new();
                for cell in buffer.content() {
                    assert!(
                        !cell.modifier.contains(Modifier::DIM),
                        "{:?} is DIM at {size:?}",
                        cell.symbol()
                    );
                    if cell.fg != Color::Reset {
                        assert!(
                            ours.contains(&Some(cell.fg)),
                            "{:?} is {:?} at {size:?}",
                            cell.symbol(),
                            cell.fg
                        );
                        used.insert(cell.fg);
                    }
                }
                for (n, role) in [(1, "accent"), (2, "dim"), (3, "warning"), (4, "error")] {
                    assert!(used.contains(&Color::Rgb(n, n, n)), "no {role} at {size:?}");
                }
            }
        }
    }

    #[test]
    fn focusing_a_lower_panel_never_moves_the_list() {
        let mut m = fleet(20);
        m.show_view(View::Overview);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let before = laid(&mut m, (80, 24));
        let offset = m.scroll.offset;
        let row = before
            .list
            .lines
            .iter()
            .position(|l| text(l).contains("obs-layout-4"))
            .expect("selected");
        m.panel = Focused::Agent { offset: 0 };
        laid(&mut m, (80, 24));
        assert_eq!(m.scroll.offset, offset, "the 3-row window isn't remembered");
        m.panel = Focused::Agents;
        let after = laid(&mut m, (80, 24));
        let row_after = after
            .list
            .lines
            .iter()
            .position(|l| text(l).contains("obs-layout-4"))
            .expect("selected");
        assert_eq!((m.scroll.offset, row_after), (offset, row));
    }

    #[test]
    fn the_readiness_row_never_eats_the_file_rows() {
        for height in 12..=24 {
            let mut m = fleet(8);
            m.show_view(View::Overview);
            m.set_cursor(Cursor::Entry(name("payroll-fix-3")));
            let laid = laid(&mut m, (80, height));
            let Some((panel, changes)) = laid.changes.as_ref() else {
                continue;
            };
            let rows = texts(&panel.lines);
            let head = usize::from(rows[0].contains("uncommitted"));
            assert_eq!(head, 1, "payroll-fix-3 is dirty in the fixture");
            let files = rows[head..]
                .iter()
                .filter(|l| !l.trim_start().starts_with('+'))
                .count();
            assert!(
                files >= CHANGES_MIN.min(changes.diff.files),
                "{height} rows: {files} file rows under readiness\n{}",
                rows.join("\n")
            );
            let turn = laid.agent.as_ref().expect("[2] shows");
            let list_clamped = list_room(height - 1) <= LIST_MIN;
            assert!(
                list_clamped || usize::from(turn.area.height) >= 3,
                "{height} rows: [2] is {} tall",
                turn.area.height
            );
        }
    }

    #[test]
    fn border_text_needs_a_gap_beside_the_title() {
        let title = vec![Span::raw("[2] "), Span::raw("obs-layout-4")];
        assert!(fits_beside(&title, 11, 16 + 7 + 11));
        assert!(!fits_beside(&title, 11, 16 + 6 + 11));
    }

    #[test]
    fn shared_rows_keep_the_counts_aligned_and_git_errors_are_one_row() {
        let facts = facts();
        let fleet_model = fleet(8);
        let (_, entry) = fleet_model
            .snapshot()
            .expect("loaded")
            .entry(&name("auth-refresh-2"))
            .expect("in the fleet");
        let shared = HashMap::from([(PathBuf::from("logo.png"), vec![entry])]);
        let lines = texts(&changes(&flat(&facts), &shared, None, 34, 4, None));
        let column = |line: &str, at: &str| {
            line.find(at)
                .map(|byte| line[..byte].chars().count())
                .expect("the count is there")
        };
        assert_eq!(
            column(&lines[0], "+98"),
            column(&lines[2], "bin"),
            "{}",
            lines.join("\n")
        );
        assert!(lines[2].contains("∩ bin") && !lines[0].contains('∩'));

        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("rate-limit-1")));
        m.facts
            .insert(name("rate-limit-1"), Err("fatal: not a repo\nmore".into()));
        let laid = laid(&mut m, (200, 50));
        let (panel, changes) = laid.changes.as_ref().expect("[3] shows");
        assert_eq!(texts(&panel.lines), [" git: fatal: not a repo"]);
        assert_eq!(changes.diff.files, 0);
    }

    #[test]
    fn focus_lists_who_needs_you_in_other_repos() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("agent-tui0")));
        let lines = texts(&laid(&mut m, (200, 50)).list.lines);
        let header = lines
            .iter()
            .position(|l| l.contains("Needs you"))
            .expect("found");
        assert!(
            lines[header + 1].contains("◆ heucristic0"),
            "{}",
            lines.join("\n")
        );
        assert!(lines[header + 2].contains("rate-limit-1"));
        assert!(
            !m.items()
                .iter()
                .any(|c| c.entry_name() == Some(&name("rate-limit-1"))),
            "they can't be selected in Focus"
        );
    }

    #[test]
    fn a_hidden_panel_hands_focus_back_to_the_list() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("rate-limit-1")));
        m.panel = Focused::Changes { file: 9 };
        let clamped = laid(&mut m, (200, 50));
        assert_eq!(
            clamped.focus,
            Focused::Changes { file: 1 },
            "the readiness row and one file, so the last row"
        );
        m.facts.remove(&name("rate-limit-1"));
        assert_eq!(laid(&mut m, (200, 50)).focus, Focused::Agents);
    }

    /// The screen as rows of text.
    fn screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
        let buffer = terminal.backend().buffer();
        let width = usize::from(buffer.area.width);
        buffer
            .content()
            .chunks(width)
            .map(|row| row.iter().map(Cell::symbol).collect::<String>())
            .collect()
    }

    #[test]
    fn the_list_says_how_many_it_hides_and_stops_once_everything_shows() {
        let mut m = fleet(10);
        let hidden = m.hidden();
        assert!(hidden > 0, "the fleet has exited and idle checkouts");
        let rows = screen(&render(&mut m, (200, 50)));
        let corner = format!("+{hidden} hidden · ");
        assert!(
            rows.iter().any(|r| r.contains(&corner)),
            "{}",
            rows.join("\n")
        );
        assert!(
            !rows.iter().any(|r| r.contains("○")),
            "no exited checkout is listed"
        );

        m.visibility = Visibility::All;
        let rows = screen(&render(&mut m, (200, 50)));
        assert!(!rows.iter().any(|r| r.contains(" hidden")));
        assert!(rows.iter().any(|r| r.contains("○")));
    }

    #[test]
    fn the_turn_cursor_marks_a_subagent_and_scrolls_to_keep_it_in_view() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let rows = screen(&render(&mut m, (200, 50)));
        assert!(
            !rows
                .iter()
                .any(|r| r.contains('▌') && r.contains(['├', '└']))
        );

        m.panel = Focused::Agent { offset: 0 };
        m.subagent = Some(AgentId::new("s1"));
        let rows = screen(&render(&mut m, (200, 50)));
        let marked: Vec<&String> = rows
            .iter()
            .filter(|r| r.contains('▌') && r.contains(['├', '└']))
            .collect();
        assert_eq!(marked.len(), 1, "{}", rows.join("\n"));
        assert!(marked[0].contains("Reverting memo edit"), "{}", marked[0]);

        // In a short column [2] scrolls, and the cursor's row stays on screen.
        m.subagent = Some(AgentId::new("s0"));
        for height in [14, 17, 20] {
            let rows = screen(&render(&mut m, (100, height)));
            assert!(
                rows.iter()
                    .any(|r| r.contains('▌') && r.contains(['├', '└'])),
                "height {height}\n{}",
                rows.join("\n")
            );
        }
    }

    #[test]
    fn a_file_under_changes_cursor_replaces_the_terminal_with_its_diff_and_the_pty_keeps_its_size()
    {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let before = {
            render(&mut m, (100, 30));
            m.areas.terminal
        };
        m.panel = Focused::Changes { file: 1 };
        m.settle_main();
        std::thread::sleep(crate::document::REST + std::time::Duration::from_millis(20));
        let key = m.docs.due(std::time::Instant::now()).expect("due");
        m.docs.loaded(
            &key,
            Ok(crate::document::Doc::Diff(
                agent_core::ports::FileDiff::Text {
                    lines: vec![
                        "@@ -1,2 +1,3 @@".into(),
                        " keep".into(),
                        "-gone".into(),
                        "+added".into(),
                    ],
                    more: 4000,
                },
            )),
        );
        let terminal = render(&mut m, (100, 30));
        assert_eq!(
            m.areas.terminal, before,
            "[0] changing content never resizes the pty"
        );
        let rows = screen(&terminal);
        let text = rows.join("\n");
        assert!(text.contains("[0] src/triage/flow.ts +9 −3"), "{text}");
        assert!(text.contains("+added") && text.contains("-gone"), "{text}");
        assert!(text.contains("4,000 more lines · e open"), "{text}");
        assert!(
            rows[0].contains("∩ auth-refresh-2 +2") && rows[0].contains(" obs-layout-4 ╮"),
            "the title yields names to the agent's glyph, name and time\n{}",
            rows[0]
        );
        let wide = screen(&render(&mut m, (200, 30)));
        assert!(
            wide[0].contains("∩ auth-refresh-2, payroll-fix-3, rate-limit-1")
                && wide[0].contains("obs-layout-4  ↳3"),
            "the agent's status sits top right when there's room\n{}",
            wide[0]
        );
        let buffer = terminal.backend().buffer();
        let added = rows
            .iter()
            .position(|r| r.contains("+added"))
            .expect("a row");
        let at = rows[added].find("+added").expect("a column");
        let col = rows[added][..at].chars().count();
        assert_eq!(
            buffer[(
                u16::try_from(col).expect("fits"),
                u16::try_from(added).expect("fits")
            )]
                .fg,
            Color::Green
        );
    }

    #[test]
    fn an_asking_agent_opens_a_banner_over_its_diff_and_a_focused_document_takes_the_border() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("rate-limit-9")));
        m.panel = Focused::Changes { file: 1 };
        m.settle_main();
        std::thread::sleep(crate::document::REST + std::time::Duration::from_millis(20));
        let key = m.docs.due(std::time::Instant::now()).expect("due");
        m.docs.loaded(
            &key,
            Ok(crate::document::Doc::Diff(
                agent_core::ports::FileDiff::Text {
                    lines: vec!["@@ -1 +1 @@".into(), "+added".into()],
                    more: 0,
                },
            )),
        );
        let terminal = render(&mut m, (100, 30));
        let rows = screen(&terminal);
        let at = rows
            .iter()
            .position(|r| r.contains("i answer"))
            .expect("a banner");
        assert!(rows[at].contains("◆ Bash: pnpm test"), "{}", rows[at]);
        assert!(
            rows[at + 2].contains("@@ -1 +1 @@"),
            "the diff follows after a blank row\n{}",
            rows.join("\n")
        );
        let area = m.areas.terminal;
        let border = |terminal: &Terminal<TestBackend>| {
            terminal.backend().buffer()[(area.x - 2, area.y - 1)].fg
        };
        assert_ne!(border(&terminal), Color::Cyan, "[3] has the keys");
        m.panel = m.panel.into_main();
        let terminal = render(&mut m, (100, 30));
        assert_eq!(border(&terminal), Color::Cyan, "[0] has them now");
    }

    fn log_of(entries: usize, finished: bool) -> agent_core::ports::SubagentDetail {
        use agent_core::ports::LogEntry;
        let mut log = Vec::new();
        for i in 0..entries {
            log.push(LogEntry::Called {
                tool: "Read".into(),
                target: Some(format!("src/file{i}.rs")),
                result: Some(format!("{i} lines")),
            });
            if i % 4 == 3 {
                log.push(LogEntry::Said(format!("Checked file {i}.")));
            }
        }
        log.push(LogEntry::Called {
            tool: "Bash".into(),
            target: Some("cargo test".into()),
            result: None,
        });
        if finished {
            log.push(LogEntry::Said("All done, three call sites.".into()));
        }
        agent_core::ports::SubagentDetail {
            prompt: "Find every call site of roundCurrency.".into(),
            finished,
            log,
            ..Default::default()
        }
    }

    #[test]
    fn the_turn_document_shows_the_whole_prompt_message_and_subagents() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
        m.panel = Focused::Agent { offset: 0 };
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("[0] Turn"), "{text}");
        assert!(text.contains("› go"), "{text}");
        assert!(
            text.contains("Added token refresh with retry and a 401 interceptor."),
            "{text}"
        );
        assert!(
            text.contains("14 tests pass."),
            "the message in full, not its first line\n{text}"
        );

        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::Agent { offset: 0 };
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("Subagents"), "{text}");
        assert!(text.contains("Reverting memo edit"), "{text}");
    }

    #[test]
    fn a_subagents_log_follows_its_tail_and_pauses_when_scrolled_up() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::Agent { offset: 0 };
        m.subagent = Some(AgentId::new("s1"));
        let loading = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(
            loading.contains("[0] general-purpose · Reverting memo edit"),
            "{loading}"
        );

        m.subagent_logs.insert(
            (name("obs-layout-4"), AgentId::new("s1")),
            Ok(Some(log_of(40, false))),
        );
        let rows = screen(&render(&mut m, (100, 30)));
        let text = rows.join("\n");
        assert!(text.contains("following"), "{text}");
        assert!(text.contains("cargo test"), "the tail is on screen\n{text}");
        assert!(
            text.contains("running"),
            "the call without a result is running\n{text}"
        );
        assert!(
            !text.contains("Find every call site"),
            "the prompt scrolled off\n{text}"
        );
        let call = rows
            .iter()
            .find(|r| r.contains("src/file38.rs"))
            .expect("a call row");
        assert!(
            call.contains("38 lines"),
            "its result sits beside it\n{call}"
        );

        m.docs.scroll_by(-5);
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("paused "), "{text}");
        assert!(!text.contains("following"));

        m.docs.scroll_to_end();
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("following"), "G resumes\n{text}");

        m.subagent_logs.insert(
            (name("obs-layout-4"), AgentId::new("s1")),
            Ok(Some(log_of(2, true))),
        );
        m.docs.scroll_to_start();
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(
            text.contains("Find every call site of roundCurrency."),
            "{text}"
        );
        let rule = text
            .lines()
            .position(|l| l.contains("────────"))
            .expect("a rule");
        let result = text
            .lines()
            .position(|l| l.contains("All done, three call sites."))
            .expect("the result");
        assert!(rule < result, "the final result follows a rule\n{text}");
    }

    #[test]
    fn a_subagent_without_a_transcript_says_so() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::Agent { offset: 0 };
        m.subagent = Some(AgentId::new("s1"));
        m.subagent_logs
            .insert((name("obs-layout-4"), AgentId::new("s1")), Ok(None));
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("no transcript for this subagent"), "{text}");
    }

    #[test]
    fn the_readiness_row_reads_the_branch_and_full_screen_hides_the_sidebar_with_the_position_in_the_title()
     {
        use agent_core::ports::BranchLog;
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::Changes { file: 0 };
        m.settle_main();
        std::thread::sleep(crate::document::REST + std::time::Duration::from_millis(20));
        let key = m.docs.due(std::time::Instant::now()).expect("due");
        m.docs.loaded(
            &key,
            Ok(crate::document::Doc::Branch(BranchLog {
                commits: vec!["abc1234 Fix the rounding".into()],
                uncommitted: vec![(" M".into(), "src/a.rs".into())],
            })),
        );
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("[0] Branch ↑1"), "{text}");
        assert!(
            text.contains("Commits (1)") && text.contains("abc1234 Fix the rounding"),
            "{text}"
        );
        assert!(
            text.contains("Uncommitted (1)") && text.contains(" M src/a.rs"),
            "{text}"
        );

        m.screen = crate::model::Screen::Full;
        let rows = screen(&render(&mut m, (100, 30)));
        assert!(rows[0].starts_with("╭ [0] 1 of 5 · Branch"), "{}", rows[0]);
        assert!(!rows.iter().any(|r| r.contains("[1]")), "no sidebar");
        assert!(
            matches!(m.panel, Focused::Changes { file: 0 }),
            "the focus survives"
        );
    }

    fn click(m: &mut Model, x: u16, y: u16, kind: crossterm::event::MouseEventKind) {
        crate::update::update(
            m,
            crate::worker::Msg::Mouse(crossterm::event::MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: crossterm::event::KeyModifiers::NONE,
            }),
        );
    }

    fn left() -> crossterm::event::MouseEventKind {
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
    }

    #[test]
    fn clicks_and_the_wheel_land_on_the_rows_drawn_in_two_and_three_and_in_the_document() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        render(&mut m, (100, 30));
        let (y, row) = m.areas.change_rows[2];
        assert_eq!(row, 2);
        click(&mut m, 5, y, left());
        assert_eq!(
            m.panel,
            Focused::Changes { file: 2 },
            "[3] has the keys on that file"
        );
        assert!(matches!(m.hover(), Some(crate::document::Hover::Diff(_))));
        let (y0, _) = m.areas.change_rows[0];
        click(&mut m, 5, y0, left());
        assert!(
            matches!(m.hover(), Some(crate::document::Hover::Branch(_))),
            "the readiness row reads the branch"
        );
        click(&mut m, 5, y, crossterm::event::MouseEventKind::ScrollDown);
        assert_eq!(
            m.panel,
            Focused::Changes { file: 1 },
            "the wheel moves the cursor a row"
        );

        render(&mut m, (100, 30));
        let (y, row) = m.areas.agent_rows[2];
        assert_eq!(row, 2);
        click(&mut m, 5, y, left());
        assert!(matches!(m.panel, Focused::Agent { .. }));
        assert_eq!(m.row_at(), 2, "the second subagent row");
        assert!(matches!(
            m.hover(),
            Some(crate::document::Hover::Subagent(..))
        ));
        let turn_y = m.areas.agent_rows[0].0;
        click(&mut m, 5, turn_y, left());
        assert!(matches!(m.hover(), Some(crate::document::Hover::Turn(_))));

        render(&mut m, (100, 30));
        let terminal = m.areas.terminal;
        click(&mut m, terminal.x + 2, terminal.y + 2, left());
        assert!(m.main_focused(), "a click in the document focuses it");
        click(
            &mut m,
            terminal.x + 2,
            terminal.y + 2,
            crossterm::event::MouseEventKind::ScrollDown,
        );
        assert_eq!(m.docs.scroll, 2, "the wheel scrolls it");
    }

    #[test]
    fn the_wheel_scrolls_only_the_focused_panel_and_never_moves_focus() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        render(&mut m, (100, 30));
        let (y, _) = m.areas.change_rows[2];
        click(&mut m, 5, y, crossterm::event::MouseEventKind::ScrollDown);
        assert!(
            matches!(m.panel, Focused::Agents),
            "a wheel over an unfocused [3] leaves focus on the list: {:?}",
            m.panel
        );
    }

    #[test]
    fn clicks_after_the_panel_scrolls_land_on_the_drawn_row() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::Changes { file: 4 };
        render(&mut m, (100, 16));
        let rows = m.areas.change_rows.clone();
        assert!(
            rows.first().is_some_and(|(_, first)| *first > 0),
            "scrolled: {rows:?}"
        );
        let (y, row) = rows[1];
        click(&mut m, 5, y, left());
        assert_eq!(m.panel, Focused::Changes { file: row });
    }

    #[test]
    fn the_peek_floats_over_a_dimmed_screen_without_resizing_the_pty() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::Changes { file: 1 };
        let area = {
            render(&mut m, (100, 30));
            m.areas.terminal
        };
        m.peek = Some(name("obs-layout-4"));
        let terminal = render(&mut m, (100, 30));
        let rows = screen(&terminal);
        let title = rows.iter().find(|r| r.contains("╔")).expect("a float");
        assert!(title.contains("obs-layout-4"), "{title}");
        assert!(title.contains("⏎ attach · s send · Esc close"), "{title}");
        assert_eq!(m.areas.terminal, area, "the pty keeps its size");
        let behind = terminal.backend().buffer()[(0, 5)].modifier;
        assert!(behind.contains(Modifier::DIM), "what's behind is dimmed");
        let footer = rows.last().expect("a footer");
        assert!(
            footer.contains("PEEK") && footer.contains("s send") && footer.contains("⏎ attach"),
            "{footer}"
        );
    }

    #[test]
    fn a_card_says_what_is_queued_for_its_agent() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let outbox = m.outboxes.entry(name("obs-layout-4")).or_default();
        for text in ["rebase on main", "then run the tests"] {
            outbox.queue.push_back(text.parse().expect("a prompt"));
        }
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("queued: rebase on ma"), "{text}");
        assert!(text.contains("+1"), "{text}");
        assert!(text.contains("2 queued"), "the status bar says so\n{text}");
    }

    #[test]
    fn a_repo_row_shows_its_checkouts_in_the_main_view() {
        let mut m = fleet(10);
        m.show_view(View::Overview);
        let key = m
            .snapshot()
            .expect("loaded")
            .repos()
            .iter()
            .find(|r| r.name() == "heucristic0")
            .expect("a repo")
            .key()
            .clone();
        m.set_cursor(Cursor::Repo(key));
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("[0] heucristic0"), "{text}");
        assert!(text.contains("obs-layout-4"), "{text}");
        assert!(text.contains("files +"), "what each has changed\n{text}");
    }

    #[test]
    fn a_running_call_reads_how_long_it_has_run() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::Agent { offset: 0 };
        m.subagent = Some(AgentId::new("s1"));
        let mut all = m.activities.take().expect("reported");
        let sub = all
            .get_mut(&name("obs-layout-4"))
            .and_then(|a| a.subagents.get_mut(&AgentId::new("s1")))
            .expect("a subagent");
        sub.tools.push(agent_core::activity::Tool {
            id: agent_core::activity::ToolId::new("t"),
            name: "Bash".into(),
            target: Some("cargo test".into()),
            since: SystemTime::now() - std::time::Duration::from_secs(12),
        });
        m.activities = Some(all);
        m.subagent_logs.insert(
            (name("obs-layout-4"), AgentId::new("s1")),
            Ok(Some(log_of(2, false))),
        );
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(
            text.contains("running 1"),
            "about 12s, from the hooks\n{text}"
        );
    }

    fn footer_of(m: &mut Model, size: (u16, u16)) -> String {
        screen(&render(m, size)).pop().expect("a footer")
    }

    fn hint_words(footer: &str) -> Vec<String> {
        footer
            .split("  ")
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .collect()
    }

    #[test]
    fn the_footer_ranks_hints_fits_what_it_can_and_never_repeats() {
        for panel in [
            Focused::Agents,
            Focused::Agent { offset: 0 },
            Focused::Changes { file: 0 },
        ] {
            let mut counts = Vec::new();
            for size in [(80, 24), (100, 30), (200, 50)] {
                let mut m = fleet(10);
                m.set_cursor(Cursor::Entry(name("obs-layout-4")));
                m.panel = panel;
                let footer = footer_of(&mut m, size);
                let words = hint_words(&footer);
                let mut sorted = words.clone();
                sorted.sort();
                sorted.dedup();
                assert_eq!(sorted.len(), words.len(), "{size:?} {footer}");
                assert!(footer.contains("? keys"), "{size:?} {footer}");
                assert!(footer.contains("NAV"), "{footer}");
                counts.push(words.len());
            }
            assert!(
                counts[0] <= counts[1] && counts[1] <= counts[2],
                "{counts:?}"
            );
        }
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let narrow = footer_of(&mut m, (80, 24));
        let wide = footer_of(&mut m, (200, 50));
        assert!(wide.contains("m pin") || wide.contains("m unpin"), "{wide}");
        assert!(wide.contains("s send"), "{wide}");
        if narrow.contains("m pin") {
            assert!(narrow.contains("s send"), "lower ranks go first\n{narrow}");
        }
    }

    #[test]
    fn the_send_box_says_queue_when_it_will_queue() {
        let mut m = fleet(10);
        m.overlay = Overlay::Talk {
            to: name("obs-layout-4"),
            text: String::new(),
        };
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("Send to obs-layout-4"), "{text}");
        let queues = text.contains("queues until it's at rest");
        assert_eq!(text.contains("⏎ queue"), queues, "{text}");
    }

    fn open_help(panel: Focused) -> Model {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = panel;
        m.overlay = Overlay::Help(crate::model::HelpState::default());
        m
    }

    #[test]
    fn the_help_lists_local_keys_before_global_ones_for_each_panel() {
        for (panel, title) in [
            (Focused::Agents, "[1] Agents"),
            (Focused::Agent { offset: 0 }, "[2] Turn"),
            (Focused::Changes { file: 0 }, "[3] Changes"),
        ] {
            let mut m = open_help(panel);
            let text = screen(&render(&mut m, (100, 30))).join("\n");
            let local = text.find("Local").expect("a Local section");
            let global = text.find("Global").expect("a Global section");
            assert!(local < global, "{text}");
            assert!(text.contains(title), "{title}\n{text}");
            assert!(text.contains("Keybindings") && text.contains("(type to filter)"));
            assert!(text.contains("1 of "), "{text}");
        }
    }

    #[test]
    fn typing_filters_the_help_and_escape_clears_then_closes() {
        use crate::update::update;
        use crate::worker::Msg;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let press = |m: &mut Model, code| {
            update(m, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        };
        let mut m = open_help(Focused::Agents);
        for c in "PEEK".chars() {
            press(&mut m, KeyCode::Char(c));
        }
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("filter: PEEK"), "{text}");
        assert!(text.contains("1 of 1"), "{text}");
        assert!(!text.contains("quit"), "{text}");
        press(&mut m, KeyCode::Backspace);
        press(&mut m, KeyCode::Esc);
        assert!(matches!(&m.overlay, Overlay::Help(h) if h.filter.is_empty()));
        press(&mut m, KeyCode::Esc);
        assert!(matches!(m.overlay, Overlay::None));
        let mut m = open_help(Focused::Agents);
        press(&mut m, KeyCode::Char('/'));
        press(&mut m, KeyCode::Char('k'));
        assert!(matches!(&m.overlay, Overlay::Help(h) if h.filter == "k"));
    }

    #[test]
    fn enter_keeps_the_help_filter_and_hands_j_k_back_and_q_or_question_mark_close() {
        use crate::update::update;
        use crate::worker::Msg;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let press = |m: &mut Model, code| {
            update(m, Msg::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        };
        let mut m = open_help(Focused::Agents);
        for c in "peek".chars() {
            press(&mut m, KeyCode::Char(c));
        }
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("keep filter"), "{text}");
        press(&mut m, KeyCode::Enter);
        assert!(
            matches!(&m.overlay, Overlay::Help(h) if h.filter == "peek" && !h.typing),
            "the filter stays and typing stops"
        );
        press(&mut m, KeyCode::Char('j'));
        assert!(matches!(&m.overlay, Overlay::Help(h) if h.filter == "peek"));
        for code in [KeyCode::Char('q'), KeyCode::Char('?'), KeyCode::Esc] {
            let mut m = open_help(Focused::Agents);
            press(&mut m, KeyCode::Char('x'));
            press(&mut m, KeyCode::Enter);
            press(&mut m, code);
            assert!(matches!(m.overlay, Overlay::None), "{code:?} closes");
        }
        let mut m = open_help(Focused::Agents);
        press(&mut m, KeyCode::Char('/'));
        press(&mut m, KeyCode::Char('q'));
        assert!(
            matches!(&m.overlay, Overlay::Help(h) if h.filter == "q"),
            "q is a letter while typing"
        );
    }

    #[test]
    fn the_help_scrolls_at_80_by_24_and_follows_the_selection() {
        use crate::update::update;
        use crate::worker::Msg;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut m = open_help(Focused::Agents);
        let first = screen(&render(&mut m, (80, 24)));
        assert!(first.iter().any(|r| r.contains("Local")), "{first:?}");
        assert!(
            !first.join("\n").contains("quit"),
            "overflow scrolls out of view"
        );
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
        );
        let last = screen(&render(&mut m, (80, 24)));
        assert_eq!(last.len(), 24);
        let text = last.join("\n");
        assert!(
            text.contains("PgDn") || text.contains("Description"),
            "{text}"
        );
        assert!(text.contains("attach next needing you"), "{text}");
        assert!(!text.contains("Local"), "the top scrolled away\n{text}");
        assert!(text.contains("Description"), "{text}");
    }

    #[test]
    fn the_help_shows_every_key_when_filtered_by_each_word() {
        let mut m = open_help(Focused::Agents);
        let text = screen(&render(&mut m, (100, 40))).join("\n");
        for expected in ["attach", "edit checkout", "send", "peek", "switch", "new"] {
            assert!(text.contains(expected), "{expected}\n{text}");
        }
    }

    fn tree_fleet() -> Model {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.changes_view = crate::tree::ChangesView::Tree;
        let Some(Ok(facts)) = m.facts.get_mut(&name("obs-layout-4")) else {
            panic!("facts")
        };
        facts.changed = [
            ("crates/tui/src/lib.rs", 2, 1),
            ("crates/tui/src/model.rs", 10, 2),
            ("crates/tui/src/view/sidebar.rs", 4, 0),
            ("docs/design.md", 1, 1),
        ]
        .into_iter()
        .map(|(p, added, removed)| agent_core::ports::Change {
            path: p.into(),
            lines: Lines::Counted { added, removed },
            state: agent_core::ports::FileState::Committed,
        })
        .collect();
        facts.pending = facts.changed.clone();
        m
    }

    #[test]
    fn the_tree_compresses_chains_indents_and_sums_folder_counts() {
        let mut m = tree_fleet();
        let text = screen(&render(&mut m, (100, 50))).join("\n");
        assert!(text.contains("▼ crates/tui/src"), "{text}");
        assert!(text.contains("▼ view"), "{text}");
        assert!(text.contains("▼ docs"), "{text}");
        assert!(text.contains("+16 −3"), "the folder sums its files\n{text}");
        let rows = screen(&render(&mut m, (100, 50)));
        let indent = |needle: &str| rows.iter().find_map(|r| r.find(needle)).expect(needle);
        assert!(indent("lib.rs") > indent("crates/tui/src") - 2);
        assert!(indent("sidebar.rs") > indent("lib.rs"));
    }

    #[test]
    fn folder_counts_are_dim_and_file_counts_are_coloured() {
        let mut m = tree_fleet();
        let terminal = render(&mut m, (100, 50));
        let buffer = terminal.backend().buffer();
        let rows = screen(&terminal);
        let at = |needle: &str| {
            let y = rows.iter().position(|r| r.contains(needle)).expect(needle);
            let x = rows[y].rfind('+').expect("a count");
            buffer[(u16::try_from(x).expect("x"), u16::try_from(y).expect("y"))].clone()
        };
        let folder = at("▼ crates/tui/src");
        let file = at("lib.rs");
        assert_ne!(folder.fg, file.fg);
    }

    #[test]
    fn a_folder_in_main_lists_the_files_under_it_with_counts() {
        let mut m = tree_fleet();
        m.panel = Focused::Changes { file: 1 };
        let text = screen(&render(&mut m, (140, 40))).join("\n");
        assert!(text.contains("[0] crates/tui/src/"), "{text}");
        assert!(text.contains("3 files"), "{text}");
        assert!(text.contains("view/sidebar.rs"), "{text}");
        assert!(
            !text.contains("docs/design.md ") || !text.contains("design.md  +1"),
            "only its own files"
        );
    }

    #[test]
    fn clicks_and_the_wheel_work_on_tree_rows() {
        let mut m = tree_fleet();
        render(&mut m, (100, 50));
        let (y, row) = m.areas.change_rows[3];
        assert_eq!(row, 3);
        click(&mut m, 5, y, left());
        assert_eq!(m.panel, Focused::Changes { file: 3 });
        assert_eq!(
            m.change_row(3).map(|r| r.label),
            Some("sidebar.rs".to_string())
        );
        assert!(matches!(m.hover(), Some(crate::document::Hover::Diff(_))));
        let (y, _) = m.areas.change_rows[1];
        click(&mut m, 5, y, left());
        assert!(matches!(m.hover(), Some(crate::document::Hover::Folder(_))));
        click(&mut m, 5, y, crossterm::event::MouseEventKind::ScrollDown);
        assert_eq!(m.panel, Focused::Changes { file: 2 });
    }

    #[test]
    fn a_folded_folder_shows_a_closed_arrow_and_the_rows_below_move_up() {
        let mut m = tree_fleet();
        m.panel = Focused::Changes { file: 1 };
        crate::update::update(
            &mut m,
            crate::worker::Msg::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            )),
        );
        let rows = screen(&render(&mut m, (100, 50)));
        let text = rows
            .iter()
            .map(|r| r.chars().take(30).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("▶ crates/tui/src"), "{text}");
        assert!(!text.contains("lib.rs"), "{text}");
        assert!(text.contains("▼ docs"), "{text}");
    }
}
