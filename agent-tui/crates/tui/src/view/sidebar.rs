//! The sidebar, three panels stacked lazygit-style: [1] the fleet as
//! cards, [2] the selected agent's activity, [3] its work in git.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agent_core::activity::{Subagent, SubagentStatus};
use agent_core::model::{Entry, Head, Repo, RepoKey, Snapshot};
use agent_core::names::SessionName;
use agent_core::ports::{Compared, DiffStat, Edit, FileState, WorktreeFacts};
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
use crate::cause::Cause;
use crate::model::{
    ActivityRow, AgentStatus, Cursor, Doing, Drawn, Extent, Failure, Fold, ListScroll, Listed,
    Model, Panel as Focused, Placement, Pulse, Row, Target, WorkRoom,
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
    Interrupted,
}

impl<'a> Gist<'a> {
    /// Settled agents (idle, exited, no session) have none. `heard` is when
    /// the agent or any of its subagents last reported.
    fn of(status: &'a AgentStatus, heard: SystemTime, now: SystemTime) -> Option<Self> {
        match status {
            AgentStatus::NoSession | AgentStatus::Exited | AgentStatus::Idle => None,
            AgentStatus::Interrupted { .. } => Some(Self::Interrupted),
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
            (Self::Asking(question), _) => gist_of(question),
            (Self::Failed(failure), _) => gist_of(failure_text(failure)),
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
            (Self::Done(Some(summary)), _) => gist_of(&without_markdown(summary)),
            (Self::Done(None), _) => ("done".to_string(), Shown::Whole),
            (Self::Interrupted, _) => ("interrupted".to_string(), Shown::Whole),
        };
        let fitted = truncate_end(&text, width);
        let shown = if fitted == text { shown } else { Shown::Cut };
        (fitted, shown)
    }

    /// All of what the agent says about this, for [0]'s activity document.
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
            Self::Done(Some(summary)) => without_markdown(summary),
            Self::Done(None) => "done".to_string(),
            Self::Interrupted => "interrupted".to_string(),
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
fn gist_of(text: &str) -> (String, Shown) {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or_default().to_string();
    match lines.next() {
        Some(_) => (first, Shown::Cut),
        None => (first, Shown::Whole),
    }
}

/// `text` without the markdown marks that read as noise outside a
/// renderer.
fn without_markdown(text: &str) -> String {
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
            if let Some(Ok(facts)) = m.facts(entry.name()) {
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
        let heard = m.activity(entry.name()).map_or(m.clock.wall, |a| a.heard);
        let gist = Gist::of(&status, heard, m.clock.wall)
            .filter(|_| !matches!(title, Title::Elsewhere(_)) && self.has_line2(entry.name()));
        let subagents = match gist {
            Some(Gist::Delegating { .. }) => None,
            _ => m.subagents(entry),
        };
        let indent = title.indent();
        let line1 = self.line1(entry, &status, title, subagents, selected);
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
        let mut left = vec![
            Span::raw(title.indent()),
            glyph(status, m.clock.wall),
            Span::raw(" "),
        ];
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
        let mut time = time_in_state(status, m.clock.wall);
        let mut markers = Vec::new();
        markers.extend(elsewhere_marker(m, entry.name()));
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
        let Some(Ok(mine)) = self.m.facts(entry.name()) else {
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

    /// [2]'s rows: the activity, then one per subagent, and the row the cursor
    /// is on while [2] has the keys. What they say is [0]'s to show.
    fn activity_rows(&self, entry: &Entry) -> ActivityRows {
        let m = self.m;
        let Some(activity) = m.activity(entry.name()) else {
            return ActivityRows::Vacant;
        };
        let cursor = match m.panel.list() {
            Listed::Agent { offset, row } => Some(ActivityCursor {
                at: m.row_at(),
                offset,
                row: row.clone(),
            }),
            Listed::Agents | Listed::Changes { .. } => None,
        };
        let at = cursor.as_ref().map(|c| c.at);
        let first = Line::raw(activity_row(
            activity.prompt.as_deref(),
            self.content,
            at == Some(0),
        ));
        let rest = tree(
            activity.subagents.values().collect(),
            self.form,
            self.content,
            m.clock.wall,
            at.and_then(|row| row.checked_sub(1)),
        );
        ActivityRows::Listed {
            first,
            rest,
            cursor,
        }
    }
}

/// What [2] has to show: the activity's rows, or one row saying it has none.
enum ActivityRows {
    Listed {
        /// The activity row, then a row per subagent.
        first: Line<'static>,
        rest: Vec<Line<'static>>,
        /// Where [2]'s cursor is, while [2] has the keys.
        cursor: Option<ActivityCursor>,
    },
    Vacant,
}

/// [2]'s cursor: the row it is on, and the panel's position as the focus
/// has it.
struct ActivityCursor {
    at: usize,
    offset: usize,
    row: ActivityRow,
}

impl ActivityRows {
    fn rows(&self) -> NonZeroUsize {
        match self {
            Self::Listed { rest, .. } => NonZeroUsize::MIN.saturating_add(rest.len()),
            Self::Vacant => NonZeroUsize::MIN,
        }
    }
}

/// The activity row: `› ` and the prompt's first line, cut to `width`. The mark
/// for the cursor takes the place of the panel's padding.
fn activity_row(prompt: Option<&str>, width: usize, selected: bool) -> String {
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
        | AgentStatus::Done { since, .. }
        | AgentStatus::Interrupted { since } => vec![Span::styled(ago(*since, now), dim())],
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
            AgentStatus::Idle | AgentStatus::Working { .. } | AgentStatus::Interrupted { .. } => {
                (live + 1, exited)
            }
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
            let (glyph, since) = match subagent.status {
                SubagentStatus::Finished { at } => (
                    Span::styled("✓", success().add_modifier(Modifier::BOLD)),
                    ago(subagent.since, at),
                ),
                SubagentStatus::Running { .. } => {
                    (Span::raw(spinner(now)), ago(subagent.since, now))
                }
            };
            let kind_only = subagent.label().is_none();
            let label = subagent.title();
            let right = match (kind_only, form) {
                (false, Form::Long) => format!(" {}  {since}", subagent.kind),
                (true, _) | (false, Form::Short) => format!(" {since}"),
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

/// The branch row on the second tab, short enough for the narrow sidebar:
/// `↑27 · 4 uncommitted`, `↓n` when behind. [0] says what is compared.
fn compared_summary(facts: &WorktreeFacts) -> Line<'static> {
    let mut parts: Vec<String> = Vec::new();
    if facts.ahead > 0 {
        parts.push(format!("↑{}", facts.ahead));
    }
    if facts.behind > 0 {
        parts.push(format!("↓{}", facts.behind));
    }
    if facts.dirty() > 0 {
        parts.push(format!("{} uncommitted", facts.dirty()));
    }
    if parts.is_empty() {
        let empty = match facts.compared {
            Compared::Trunk { .. } => "nothing to push",
            _ => "no changes",
        };
        return Line::from(Span::raw(empty));
    }
    let mut spans = Vec::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(Span::raw(part));
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

pub(super) fn edit_letter(edit: Edit) -> &'static str {
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
    let mut spans = match row.node {
        Node::Folder { .. } => vec![
            marker_cell((row.counts.staged > 0).then_some("●"), palette().success),
            marker_cell((row.counts.unstaged > 0).then_some("●"), palette().error),
        ],
        Node::File(_) => file_code(row.state),
    };
    spans.push(Span::raw(" "));
    spans
}

fn marker_cell(text: Option<&'static str>, colour: ratatui::style::Color) -> Span<'static> {
    match text {
        Some(text) => Span::styled(text, Style::new().fg(colour)),
        None => Span::raw(" "),
    }
}

fn file_code(state: FileState) -> Vec<Span<'static>> {
    let (palette_ok, palette_err) = (palette().success, palette().error);
    match state {
        FileState::Committed => vec![Span::raw("  ")],
        FileState::Untracked => vec![Span::styled("??", Style::new().fg(palette_err))],
        FileState::Conflicted => vec![Span::styled("UU", Style::new().fg(palette().warning))],
        state => vec![
            marker_cell(state.staged().map(edit_letter), palette_ok),
            marker_cell(state.unstaged().map(edit_letter), palette_err),
        ],
    }
}

/// A file's lazygit code and a space, for lists outside [3].
pub(super) fn file_marker(state: FileState) -> Vec<Span<'static>> {
    let mut spans = file_code(state);
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
    /// What git read, once it has been.
    counted: Option<Counted>,
    scope: ChangesScope,
    /// The second tab's name: `Branch` or `Unpushed`.
    second: &'static str,
}

/// The totals, and how many of the files are shared.
#[derive(Clone, Copy)]
struct Counted {
    diff: DiffStat,
    shared: usize,
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
                        place: Placement::Solo,
                        ..
                    } => Title::Repo(repo_name),
                    Row::Elsewhere(_) => Title::Elsewhere(repo_name),
                    Row::Entry {
                        place: Placement::Pinned,
                        ..
                    } if repo_name != entry.label() => Title::Pinned(repo_name),
                    Row::Grouped(..) if repo_name != entry.label() => Title::Grouped(repo_name),
                    Row::Grouped(..)
                    | Row::Entry {
                        place: Placement::Nested,
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
    /// [2], while an agent with an activity is selected.
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
fn lay_out(m: &Model, area: Rect, room: usize) -> Option<Laid> {
    let snapshot = m.snapshot()?;
    let height = usize::from(area.height);
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

/// What [3] has to show: the files and readiness, or the reason it has no
/// row to put a cursor on.
enum Work<'a> {
    Facts {
        facts: &'a WorktreeFacts,
        rows: FactRows,
    },
    Vacant(Vacancy<'a>),
}

/// What [3] lists when it lists anything: the branch row with the files
/// after it, or only files.
enum FactRows {
    Head {
        head: Line<'static>,
        list: std::sync::Arc<Vec<TreeRow>>,
    },
    Files(Files),
}

/// A listing of at least one file.
struct Files(std::sync::Arc<Vec<TreeRow>>);

impl Files {
    fn new(list: std::sync::Arc<Vec<TreeRow>>) -> Option<Self> {
        (!list.is_empty()).then_some(Self(list))
    }
}

impl FactRows {
    fn rows(&self) -> NonZeroUsize {
        match self {
            Self::Head { list, .. } => NonZeroUsize::MIN.saturating_add(list.len()),
            Self::Files(Files(list)) => {
                NonZeroUsize::MIN.saturating_add(list.len().saturating_sub(1))
            }
        }
    }

    /// The branch row, if there is one, and the files.
    fn into_parts(self) -> (Option<Line<'static>>, std::sync::Arc<Vec<TreeRow>>) {
        match self {
            Self::Head { head, list } => (Some(head), list),
            Self::Files(Files(list)) => (None, list),
        }
    }
}

/// Why [3] has nothing to list, so its one row can say.
enum Vacancy<'a> {
    /// The facts were read and the tab lists nothing.
    NoChanges(&'a WorktreeFacts),
    /// The facts were asked for and haven't come back.
    Reading,
    /// It has a checkout and nothing has asked for its facts yet.
    NotRead,
    /// It has no checkout to read.
    NoCheckout,
    Unreadable(Cause),
}

impl Vacancy<'_> {
    fn text(&self) -> String {
        match self {
            Self::NoChanges(_) => "no changes".to_string(),
            Self::Reading => "reading git…".to_string(),
            Self::NotRead => "git not read yet".to_string(),
            Self::NoCheckout => "no checkout".to_string(),
            Self::Unreadable(why) => format!("git: {}", gist_of(&why.to_string()).0),
        }
    }
}

impl Work<'_> {
    /// The rows [3] draws, at least the one that says why it's empty.
    fn rows(&self) -> NonZeroUsize {
        match self {
            Self::Facts { rows, .. } => rows.rows(),
            Self::Vacant(_) => NonZeroUsize::MIN,
        }
    }
}

/// What [3] has to say about the selected agent.
fn work_of<'m>(m: &'m Model, entry: &Entry) -> Work<'m> {
    match m.facts(entry.name()) {
        Some(Ok(facts)) => {
            let list = m.change_list();
            let rows = if m.branch_row() {
                Some(FactRows::Head {
                    head: compared_summary(facts),
                    list,
                })
            } else {
                Files::new(list).map(FactRows::Files)
            };
            match rows {
                Some(rows) => Work::Facts { facts, rows },
                None => Work::Vacant(Vacancy::NoChanges(facts)),
            }
        }
        Some(Err(e)) => Work::Vacant(Vacancy::Unreadable(e.clone())),
        None => {
            let asked = m.tracked(entry.name()).and_then(|t| t.inspection);
            Work::Vacant(match (entry.head(), asked) {
                (None, _) => Vacancy::NoCheckout,
                (Some(_), Some(_)) => Vacancy::Reading,
                (Some(_), None) => Vacancy::NotRead,
            })
        }
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

/// How many of `rest` rows [2] and [3] get, for an activity with `activity_rows`
/// rows and `work_rows` rows of work. [2] is sized to its rows up to
/// [`AGENT_ROWS`], and [3] has what's left. [3] keeps its readiness row and
/// three files, or what it has of them, and [2] a row, so when both can't
/// fit [2] takes everything.
fn budget(rest: usize, activity_rows: NonZeroUsize, work_rows: NonZeroUsize) -> (usize, usize) {
    let want_activity = 2 + activity_rows.get().min(AGENT_ROWS);
    let activity_floor = 3;
    let work_least = (2 + work_rows.get()).min(2 + READINESS_ROW + CHANGES_MIN);
    if rest >= activity_floor + work_least {
        let activity_height = want_activity.min(rest - work_least);
        (activity_height, rest - activity_height)
    } else if rest >= activity_floor {
        (rest, 0)
    } else {
        (0, 0)
    }
}

/// [2] and [3] in `area`: [2] sized to its rows up to [`AGENT_ROWS`], [3]
/// in what's left, and [2] in all of it when the column is too short for
/// both. Each says why when it has nothing to list.
fn lower_panels(deck: &Deck<'_>, repo: &Repo, entry: &Entry, area: Rect) -> Lower {
    let m = deck.m;
    let rest = usize::from(area.height);
    let detail = deck.activity_rows(entry);
    let work = work_of(m, entry);
    let (activity_height, work_height) = budget(rest, detail.rows(), work.rows());
    let at = |y: usize, height: usize| {
        Rect::new(
            area.x,
            area.y + u16::try_from(y).unwrap_or(u16::MAX),
            area.width,
            u16::try_from(height).unwrap_or(u16::MAX),
        )
    };
    let agent = (activity_height > 0).then(|| activity_panel(detail, at(0, activity_height)));
    let changes = (work_height > 0)
        .then(|| changes_panel(deck, repo, entry, work, at(activity_height, work_height)));
    let (agent, agent_rows, agent_focus) = match agent {
        Some(a) => (Some(a.panel), a.rows, a.focus),
        None => (None, Vec::new(), None),
    };
    let (changes, change_rows, changes_focus) = match changes {
        Some(c) => (Some((c.panel, c.title)), c.rows, c.focus),
        None => (None, Vec::new(), None),
    };
    Lower {
        agent,
        agent_rows,
        changes,
        change_rows,
        focus: agent_focus.or(changes_focus).unwrap_or(Focused::Agents),
        room: if work_height > 0 {
            WorkRoom::Fits
        } else {
            WorkRoom::TooShort
        },
    }
}

/// [2] as drawn: its panel, the screen row of each of its rows with the
/// index it stands for, and where the focus is when the keys are on it.
struct ActivityPanel {
    panel: Panel,
    rows: Vec<(u16, usize)>,
    focus: Option<Focused>,
}

fn activity_panel(detail: ActivityRows, area: Rect) -> ActivityPanel {
    let inside = usize::from(area.height).saturating_sub(2);
    match detail {
        ActivityRows::Listed {
            first: activity,
            rest,
            cursor,
        } => {
            let lines: Vec<Line<'static>> = std::iter::once(activity).chain(rest).collect();
            let (first, focus) = match cursor {
                Some(ActivityCursor { at, offset, row }) => {
                    let first = scroll(offset, at, inside, lines.len());
                    (first, Some(Focused::Agent { offset: first, row }))
                }
                None => (0, None),
            };
            let lines: Vec<Line<'static>> = lines.into_iter().skip(first).take(inside).collect();
            let top = area.y + 1;
            let rows = (0..lines.len())
                .map(|i| (top + u16::try_from(i).unwrap_or(0), first + i))
                .collect();
            ActivityPanel {
                panel: pad(area, lines),
                rows,
                focus,
            }
        }
        ActivityRows::Vacant => ActivityPanel {
            panel: pad(area, vec![Line::styled("no activity recorded", dim())]),
            rows: Vec::new(),
            focus: None,
        },
    }
}

/// [3] as drawn: its panel and title, the screen row of each of its rows
/// with the index it stands for, and where the focus is when the keys are
/// on it.
struct ChangesPanel {
    panel: Panel,
    title: Changes,
    rows: Vec<(u16, usize)>,
    focus: Option<Focused>,
}

fn changes_panel(
    deck: &Deck<'_>,
    repo: &Repo,
    entry: &Entry,
    work: Work<'_>,
    area: Rect,
) -> ChangesPanel {
    let m = deck.m;
    let inside = usize::from(area.height).saturating_sub(2);
    match work {
        Work::Facts { facts, rows } => {
            let total = rows.rows().get();
            let (head, list) = rows.into_parts();
            let (cursor, focus) = match m.panel.list() {
                Listed::Changes { file } => {
                    let row = file.min(total.saturating_sub(1));
                    (Some(row), Some(Focused::Changes { file: row }))
                }
                _ => (None, None),
            };
            let top = area.y + 1;
            let (first, listed) = window(total, inside, cursor);
            let rows = (0..listed)
                .map(|i| (top + u16::try_from(i).unwrap_or(0), first + i))
                .collect();
            let shared = deck.overlaps(repo, entry);
            let lines = changes(&list, &shared, head, deck.content, inside, cursor);
            ChangesPanel {
                panel: Panel { area, lines },
                title: summary(facts, &shared, m.changes_scope),
                rows,
                focus,
            }
        }
        Work::Vacant(vacancy) => {
            let row = worktree_row(Line::raw(vacancy.text()), deck.content);
            let title = match vacancy {
                Vacancy::NoChanges(facts) => {
                    summary(facts, &deck.overlaps(repo, entry), m.changes_scope)
                }
                Vacancy::Reading
                | Vacancy::NotRead
                | Vacancy::NoCheckout
                | Vacancy::Unreadable(_) => Changes {
                    counted: None,
                    scope: m.changes_scope,
                    second: BRANCH_TAB,
                },
            };
            let focus = matches!(m.panel.list(), Listed::Changes { .. })
                .then_some(Focused::Changes { file: 0 });
            ChangesPanel {
                panel: Panel {
                    area,
                    lines: vec![row],
                },
                title,
                rows: Vec::new(),
                focus,
            }
        }
    }
}

const BRANCH_TAB: &str = "Branch";

/// The name of [3]'s second tab: what the checkout has to push on the
/// trunk, otherwise what the branch changes.
pub(super) fn second_tab(compared: &Compared) -> &'static str {
    match compared {
        Compared::Trunk { .. } => "Unpushed",
        Compared::Branch { .. } | Compared::Detached { .. } | Compared::NoBase { .. } => BRANCH_TAB,
    }
}

/// What [3]'s title says about `facts`.
fn summary(
    facts: &WorktreeFacts,
    shared: &HashMap<PathBuf, Vec<&Entry>>,
    scope: ChangesScope,
) -> Changes {
    Changes {
        counted: Some(Counted {
            diff: match scope {
                ChangesScope::Uncommitted => facts.pending_diff(),
                ChangesScope::Since => facts.diff(),
            },
            shared: shared.len(),
        }),
        scope,
        second: second_tab(&facts.compared),
    }
}

/// `[3]` then its tabs as [1]'s title has them: the active one BOLD, a DIM
/// `·` between. They all show while they fit `width`, else only the active
/// one, so moving the cursor never changes the form.
fn changes_tabs(scope: ChangesScope, second: &str, width: usize) -> Vec<Span<'static>> {
    let tabs = [
        (ChangesScope::Uncommitted, "Uncommitted".to_string()),
        (ChangesScope::Since, second.to_string()),
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

/// `⇄` when another window shows the agent: DIM while the TUI leaves it
/// alone, Yellow while the TUI is typing into it and it squishes that.
fn elsewhere_marker(m: &Model, name: &SessionName) -> Option<Span<'static>> {
    if m.squished(name).is_some() {
        Some(Span::styled("⇄", warning()))
    } else {
        m.company(name)
            .is_elsewhere()
            .then(|| Span::styled("⇄", dim()))
    }
}

/// The other agents in the repo that changed `path` too, by label.
pub(super) fn others_on(m: &Model, name: &SessionName, path: &Path) -> Vec<String> {
    let Some((repo, _)) = m.snapshot().and_then(|s| s.entry(name)) else {
        return Vec::new();
    };
    repo.entries()
        .iter()
        .filter(|e| e.name() != name)
        .filter(|e| match m.facts(e.name()) {
            Some(Ok(facts)) => facts.changed.iter().any(|c| c.path == path),
            _ => false,
        })
        .map(|e| e.label().to_string())
        .collect()
}

/// What the selected agent says about what it's doing, in full.
pub(crate) fn activity_message(m: &Model, entry: &Entry) -> Option<String> {
    let status = m.status(entry);
    let heard = m.activity(entry.name()).map_or(m.clock.wall, |a| a.heard);
    Gist::of(&status, heard, m.clock.wall)?.message()
}

/// The agent's subagents as tree rows, for [0]'s activity document.
pub(super) fn subagent_rows(m: &Model, entry: &Entry, width: usize) -> Vec<Line<'static>> {
    m.activity(entry.name()).map_or_else(Vec::new, |a| {
        tree(
            a.subagents.values().collect(),
            Form::Long,
            width,
            m.clock.wall,
            None,
        )
    })
}

/// The selected agent's state as one line: its glyph, name, what it's doing
/// and for how long, cut to `width` by dropping what it's doing first.
pub(super) fn status_line(m: &Model, entry: &Entry, width: usize) -> Vec<Span<'static>> {
    let status = m.status(entry);
    let heard = m.activity(entry.name()).map_or(m.clock.wall, |a| a.heard);
    let gist = Gist::of(&status, heard, m.clock.wall);
    // A delegating gist already ends in how long it's been quiet.
    let time = match gist {
        Some(Gist::Delegating { .. }) => Vec::new(),
        _ => time_in_state(&status, m.clock.wall),
    };
    let mut spans = vec![
        glyph(&status, m.clock.wall),
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
pub(super) fn draw(frame: &mut Frame<'_>, m: &Model, area: Rect, border: Style, out: &mut Drawn) {
    let nav = sidebar_focused(m);
    let style_of = |focused: bool| if focused { border } else { dim() };
    let agents = matches!(m.panel, Focused::Agents);
    let agent = matches!(m.panel.list(), Listed::Agent { .. });
    let files = matches!(m.panel.list(), Listed::Changes { .. });
    let title = sidebar_title(m, usize::from(area.width).saturating_sub(4));
    let corner = sidebar_count(
        m,
        style_of(agents),
        usize::from(area.width).saturating_sub(4),
    );
    let Some(laid) = lay_out(m, area, usize::from(out.areas.sidebar.height)) else {
        out.work_room = Some(WorkRoom::TooShort);
        frame.render_widget(panel(title, corner, style_of(agents), nav && agents), area);
        return;
    };
    out.areas.page = match laid.focus {
        Focused::Agents => usize::from(laid.list.area.height),
        Focused::Agent { .. } => laid
            .agent
            .as_ref()
            .map_or(0, |p| usize::from(p.area.height)),
        Focused::Changes { .. } => laid
            .changes
            .as_ref()
            .map_or(0, |(p, _)| usize::from(p.area.height)),
        Focused::Main(..) => 0,
    }
    .saturating_sub(2);
    out.areas.agent_panel = laid.agent.as_ref().map_or(Rect::default(), |p| p.area);
    out.areas.changes_panel = laid
        .changes
        .as_ref()
        .map_or(Rect::default(), |(p, _)| p.area);
    let list = panel(title, corner, style_of(agents), nav && agents);
    frame.render_widget(Paragraph::new(laid.list.lines).block(list), laid.list.area);
    if let Some(Panel { area, lines }) = laid.agent {
        let title = vec![Span::raw("[2] "), Span::styled("Activity", bold())];
        let block = panel(title, Vec::new(), style_of(agent), nav && agent);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }
    if let Some((Panel { area, lines }, changes)) = laid.changes {
        let (added, removed) = changes.counted.map_or_else(Default::default, |c| {
            counts_text(c.diff.added, c.diff.removed)
        });
        let mut title = changes_tabs(
            changes.scope,
            changes.second,
            usize::from(area.width).saturating_sub(4),
        );
        for (count, colour) in [(added, palette().success), (removed, palette().error)] {
            if !count.is_empty() {
                title.push(Span::raw(" "));
                title.push(Span::styled(count, Style::new().fg(colour)));
            }
        }
        let shared = changes.counted.map_or(0, |c| c.shared);
        if shared > 0 {
            title.push(Span::raw("  "));
            title.push(Span::styled(format!("∩ {shared}"), warning()));
        }
        let title = fit(title, usize::from(area.width).saturating_sub(4));
        let corner = changes
            .counted
            .map_or_else(Vec::new, |Counted { diff, .. }| {
                vec![Span::styled(
                    format!(
                        "{} {}",
                        diff.files,
                        if diff.files == 1 { "file" } else { "files" }
                    ),
                    style_of(files),
                )]
            });
        let block = panel(title, corner, style_of(files), nav && files);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }
    out.work_room = Some(laid.room);
    out.scroll = Some(laid.scroll);
    out.areas.rows = laid.hits;
    out.areas.agent_rows = laid.agent_rows;
    out.areas.change_rows = laid.change_rows;
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
    use crate::keymap::Ctx;
    use crate::model::{Company, Mode};
    use agent_core::ports::Lines;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn rows(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).expect("a row")
    }

    #[test]
    fn the_activity_is_sized_to_its_rows_and_the_work_gets_the_rest() {
        assert_eq!(budget(30, rows(3), rows(20)), (5, 25));
        assert_eq!(budget(30, rows(40), rows(20)), (8, 22));
        assert_eq!(budget(30, rows(1), rows(20)), (3, 27));
    }

    #[test]
    fn the_work_keeps_its_least_before_the_activity_takes_more_than_its_floor() {
        assert_eq!(budget(9, rows(10), rows(20)), (3, 6));
        assert_eq!(budget(10, rows(10), rows(20)), (4, 6));
    }

    #[test]
    fn the_activity_takes_everything_when_both_cannot_fit() {
        assert_eq!(budget(8, rows(10), rows(20)), (8, 0));
        assert_eq!(budget(5, rows(1), rows(1)), (5, 0));
        assert_eq!(budget(3, rows(1), rows(20)), (3, 0));
    }

    #[test]
    fn a_short_work_list_asks_for_no_more_than_its_rows() {
        assert_eq!(budget(30, rows(2), rows(2)), (4, 26));
    }

    #[test]
    fn a_panel_with_only_its_reason_to_show_still_gets_a_title_and_a_row() {
        assert_eq!(budget(6, rows(1), rows(1)), (3, 3));
        assert_eq!(budget(7, rows(1), rows(1)), (3, 4));
    }

    #[test]
    fn nothing_is_shown_when_the_room_is_under_one_panel() {
        assert_eq!(budget(2, rows(4), rows(1)), (0, 0));
        assert_eq!(budget(0, rows(1), rows(1)), (0, 0));
    }

    fn flat(facts: &WorktreeFacts) -> Vec<TreeRow> {
        crate::tree::build(
            &facts.changed,
            crate::tree::ChangesView::Flat,
            &std::collections::HashSet::new(),
        )
    }

    use std::collections::{HashMap, HashSet};

    use agent_core::activity::{Activity, AgentId, Common, Reported, State, ToolId};
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

    /// What [0] asks to read once the cursor has rested, by moving the
    /// clock instead of waiting.
    fn due_after_rest(m: &mut Model) -> crate::document::Key {
        m.settle();
        m.docs
            .due(m.clock.mono + crate::document::REST)
            .expect("due")
    }

    /// Draws the whole screen, as the app does.
    fn render(m: &mut Model, (width, height): (u16, u16)) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a backend");
        crate::update::resized(m, Rect::new(0, 0, width, height));
        super::super::paint(&mut terminal, m).expect("draws");
        terminal
    }

    /// The sidebar's column on a screen `size` big, as last drawn: the
    /// left edge above the status bar.
    fn column(m: &Model, (_, height): (u16, u16)) -> Rect {
        Rect::new(0, 0, m.areas.sidebar.width + 2, height - 1)
    }

    fn laid(m: &mut Model, size: (u16, u16)) -> Laid {
        render(m, size);
        lay_out(m, column(m, size), usize::from(m.areas.sidebar.height)).expect("succeeds")
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
    fn a_finished_activity_reads_its_first_line_and_says_all_of_it() {
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

    fn branch_name(name: &str) -> agent_core::names::BranchName {
        name.parse().expect("parses")
    }

    fn detached(sha: &str) -> agent_core::ports::Checkout {
        agent_core::ports::Checkout::Detached(sha.into())
    }

    #[test]
    fn the_second_tab_row_is_short() {
        let branch = || Compared::Branch {
            branch: branch_name("fix-thing"),
            base: "origin/main".parse().expect("parses"),
        };
        let trunk = || Compared::Trunk {
            branch: branch_name("main"),
            base: "origin/main".parse().expect("parses"),
        };
        let read = |compared, ahead, dirty, behind| {
            text(&compared_summary(&WorktreeFacts {
                ahead,
                behind,
                pending: crate::fixtures::pending_files(dirty),
                ..WorktreeFacts::unchanged(compared)
            }))
        };
        assert_eq!(read(branch(), 3, 2, 0), "↑3 · 2 uncommitted");
        assert_eq!(read(branch(), 1, 0, 2), "↑1 · ↓2");
        assert_eq!(read(branch(), 0, 0, 0), "no changes");
        assert_eq!(read(trunk(), 27, 4, 0), "↑27 · 4 uncommitted");
        assert_eq!(read(trunk(), 0, 4, 0), "4 uncommitted");
        assert_eq!(read(trunk(), 0, 0, 0), "nothing to push");
        assert_eq!(
            read(
                Compared::NoBase {
                    checkout: agent_core::ports::Checkout::Branch(branch_name("wip"))
                },
                0,
                1,
                0
            ),
            "1 uncommitted"
        );
    }

    #[test]
    fn the_second_tab_is_named_for_where_the_checkout_is() {
        assert_eq!(
            second_tab(&Compared::Trunk {
                branch: branch_name("main"),
                base: "origin/main".parse().expect("parses")
            }),
            "Unpushed"
        );
        for other in [
            Compared::Branch {
                branch: branch_name("x"),
                base: "origin/main".parse().expect("parses"),
            },
            Compared::Detached {
                sha: "abc".into(),
                base: "origin/main".parse().expect("parses"),
            },
            Compared::NoBase {
                checkout: detached("x"),
            },
        ] {
            assert_eq!(second_tab(&other), "Branch");
        }
    }

    #[test]
    fn changes_title_shows_both_tabs_while_they_fit_and_only_the_active_one_below_that() {
        use crate::tree::ChangesScope::{Since, Uncommitted};
        let title = |scope, width| text(&Line::from(changes_tabs(scope, "Branch", width)));
        assert_eq!(title(Uncommitted, 40), "[3] Uncommitted · Branch");
        assert_eq!(title(Since, 24), "[3] Uncommitted · Branch");
        assert_eq!(title(Uncommitted, 23), "[3] Uncommitted");
        assert_eq!(title(Since, 23), "[3] Branch");
        assert_eq!(
            text(&Line::from(changes_tabs(Since, "Unpushed", 40))),
            "[3] Uncommitted · Unpushed"
        );
        let bold_of = |scope, second| {
            changes_tabs(scope, second, 40)
                .into_iter()
                .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
                .map(|s| s.content.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(bold_of(Uncommitted, "Branch"), ["Uncommitted"]);
        assert_eq!(bold_of(Since, "Unpushed"), ["Unpushed"]);
    }

    #[test]
    fn each_tab_totals_its_own_files_and_only_the_second_leads_with_the_branch_row() {
        use agent_core::ports::{Change, Lines, TrackedState};
        let change = |path: &str, added, removed| {
            Change::tracked(
                path,
                Lines::Counted { added, removed },
                TrackedState::Committed,
            )
        };
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.changes_view = crate::tree::ChangesView::Flat;
        let Some(Ok(read)) = m
            .track(&name("obs-layout-4"))
            .facts
            .as_mut()
            .map(|f| &mut f.result)
        else {
            panic!("facts")
        };
        let facts = &mut read.facts;
        facts.ahead = 24;
        facts.compared = Compared::Branch {
            branch: branch_name("fix"),
            base: "origin/main".parse().expect("parses"),
        };
        facts.changed = vec![change("a.rs", 50, 5), change("b.rs", 7, 1)];
        facts.pending = vec![change("b.rs", 2, 0)];
        let mut shown = Vec::new();
        for _ in 0..2 {
            let laid = laid(&mut m, (200, 50));
            let (panel, changes) = laid.changes.as_ref().expect("[3] shows");
            let diff = changes.counted.expect("git was read").diff;
            shown.push((
                texts(&panel.lines)[0].clone(),
                diff.files,
                diff.added,
                diff.removed,
                panel.lines.len(),
            ));
            m.toggle_changes_scope();
        }
        assert_eq!(
            shown[0].0.trim(),
            "b.rs                                     +2"
        );
        assert_eq!(shown[1].0, " ↑24 · 1 uncommitted");
        assert_eq!(shown[0].1, 1);
        assert_eq!((shown[0].2, shown[0].3), (2, 0));
        assert_eq!(shown[0].4, 1, "one file");
        assert_eq!((shown[1].1, shown[1].2, shown[1].3), (2, 57, 6));
        assert_eq!(shown[1].4, 3, "the branch row and two files");
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
        use agent_core::ports::{Change, TrackedState};
        WorktreeFacts {
            changed: vec![
                Change::tracked(
                    "a/src/payroll/calc.ts",
                    Lines::Counted {
                        added: 98,
                        removed: 52,
                    },
                    TrackedState::Both {
                        staged: Edit::Modified,
                        unstaged: Edit::Modified,
                    },
                ),
                Change::tracked(
                    "a/src/payroll/calc.test.ts",
                    Lines::Counted {
                        added: 24,
                        removed: 0,
                    },
                    TrackedState::Staged(Edit::Added),
                ),
                Change::tracked("logo.png", Lines::Binary, TrackedState::Committed),
                Change::untracked("notes.md"),
            ],
            ..crate::fixtures::unchanged()
        }
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
    fn a_conflicted_file_shows_uu_in_the_warning_colour() {
        let spans = file_marker(FileState::Conflicted);
        assert_eq!(text(&Line::from(spans.clone())), "UU ");
        assert_eq!(spans[0].style.fg, Some(palette().warning));
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
        let panels = || {
            [
                Focused::Agents,
                Focused::activity(),
                Focused::Changes { file: 0 },
            ]
        };
        for size in SIZES {
            for agents in FLEETS {
                for (view, panel) in View::ALL
                    .into_iter()
                    .flat_map(|view| panels().map(|panel| (view, panel)))
                {
                    let mut m = fleet(agents);
                    while m.view != view {
                        m.toggle_view();
                    }
                    m.panel = panel.clone();
                    let context = format!("{size:?}, {agents} agents, {view:?}, {panel:?}");
                    let laid = laid(&mut m, size);
                    let column = column(&m, size);
                    let focused_lower = match laid.focus {
                        Focused::Agents | Focused::Main(..) => None,
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
        assert_eq!(changes.counted.map(|c| c.shared), Some(1));
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
        m.toggle_changes_scope();
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
        m.set_test_facts(
            &done,
            Ok(WorktreeFacts {
                ahead: 0,
                ..crate::fixtures::unchanged()
            }),
        );
        let first = laid(&mut m, (200, 50));
        let (panel, changes) = first.changes.as_ref().expect("[3] shows");
        assert_eq!(changes.counted.map(|c| c.diff.files), Some(0));
        assert_eq!(texts(&panel.lines), [" no changes"]);
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
    fn the_keymap_reads_a_subagent_row_exactly_when_one_is_selected() {
        let reads_subagent =
            |m: &Model| crate::keymap::Situation::of(m).reading == crate::keymap::Reading::Subagent;
        let mut m = fleet(8);
        m.show_view(View::Overview);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::activity();
        m.point_at_subagent("s1");
        assert!(m.subagent_selected().is_some());
        assert!(reads_subagent(&m));
        m.activities
            .as_mut()
            .expect("loaded")
            .remove(&name("obs-layout-4"));
        assert!(m.subagent_selected().is_none());
        assert!(!reads_subagent(&m));
    }

    #[test]
    fn the_activity_panel_is_a_row_for_the_activity_then_one_per_subagent() {
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

        m.panel = Focused::activity();
        let lines = agent_lines(&laid(&mut m, (200, 50)));
        assert!(
            lines[0].starts_with('▌'),
            "the cursor starts on the activity\n{}",
            lines.join("\n")
        );
        m.point_at_subagent("s1");
        let lines = agent_lines(&laid(&mut m, (200, 50)));
        assert!(lines[2].starts_with('▌'), "{}", lines.join("\n"));
        assert_eq!(lines.iter().filter(|l| l.starts_with('▌')).count(), 1);

        m.point_at_subagent("s0");
        for height in [14, 16, 20] {
            let lines = agent_lines(&laid(&mut m, (80, height)));
            assert!(
                lines.iter().any(|l| l.starts_with('▌') && l.contains('└')),
                "{height} rows: the cursor's row stays in view\n{}",
                lines.join("\n")
            );
        }
    }

    type Surface = (Focused, fn(&Model) -> Overlay);

    fn checkout_of_a_link_to_another_repo(m: &Model) -> Overlay {
        let mut form = crate::model::NewAgent::new(
            crate::model::FormKind::Checkout,
            m.selected()
                .and_then(|(repo, _)| repo.key().root().cloned()),
        );
        form.task = "https://github.com/acme/elsewhere/pull/7".into();
        Overlay::New(Box::new(form))
    }

    #[test]
    fn a_name_over_the_session_hosts_limit_says_so_without_naming_the_host() {
        let mut m = fleet(8);
        let mut form = crate::model::NewAgent::new(
            crate::model::FormKind::New,
            m.selected()
                .and_then(|(repo, _)| repo.key().root().cloned()),
        );
        form.name = "a".repeat(27);
        m.overlay = Overlay::New(Box::new(form));
        let terminal = render(&mut m, (120, 40));
        let buffer = terminal.backend().buffer();
        let screen: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            screen.contains("7 characters over the session host's limit"),
            "{screen}"
        );
        assert!(screen.contains("27/20"));
        assert!(!screen.to_lowercase().contains("zellij"));
    }

    const SENTINEL: crate::palette::Palette = crate::palette::Palette {
        accent: Color::Rgb(1, 1, 1),
        dim: crate::palette::Secondary::Colour(Color::Rgb(2, 2, 2)),
        warning: Color::Rgb(3, 3, 3),
        error: Color::Rgb(4, 4, 4),
        success: Color::Rgb(5, 5, 5),
    };

    #[test]
    fn the_checkout_form_warns_about_a_link_to_another_repo_in_the_warning_colour() {
        let mut m = fleet(8);
        m.palette = SENTINEL;
        m.overlay = checkout_of_a_link_to_another_repo(&m);
        let terminal = render(&mut m, (120, 40));
        let buffer = terminal.backend().buffer();
        let warned: Vec<_> = buffer
            .content()
            .windows(12)
            .filter(|cells| cells.iter().map(Cell::symbol).collect::<String>() == "the link is ")
            .collect();
        assert_eq!(warned.len(), 1, "the form says where the link points");
        assert!(warned[0].iter().all(|c| c.fg == Color::Rgb(3, 3, 3)));
    }

    #[test]
    fn every_colour_comes_from_the_palette() {
        let palette = SENTINEL;
        let ours = [1, 2, 3, 4, 5].map(|n| Some(Color::Rgb(n, n, n)));
        for size in SIZES {
            let cases: [Surface; 4] = [
                (Focused::Agents, |_| Overlay::None),
                (Focused::activity(), |_| Overlay::None),
                (Focused::Agents, |_| {
                    Overlay::Help(crate::model::HelpState::default())
                }),
                (Focused::Agents, checkout_of_a_link_to_another_repo),
            ];
            for (panel, overlay) in cases {
                let mut m = fleet(8);
                m.palette = palette;
                m.panel = panel;
                m.overlay = overlay(&m);
                m.error("failed");
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
        m.panel = Focused::activity();
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

    fn changes_rows(m: &mut Model) -> Vec<String> {
        let laid = laid(m, (80, 40));
        let (panel, _) = laid.changes.as_ref().expect("[3] shows");
        texts(&panel.lines)
    }

    #[test]
    fn changes_stays_on_screen_and_says_no_changes_for_a_clean_worktree() {
        let mut m = fleet(8);
        let who = name("auth-refresh-2");
        m.set_cursor(Cursor::Entry(who.clone()));
        m.set_test_facts(&who, Ok(crate::fixtures::unchanged()));
        assert_eq!(changes_rows(&mut m), [" no changes"]);
    }

    #[test]
    fn changes_says_it_is_reading_git_only_while_the_facts_are_asked_for() {
        let mut m = fleet(8);
        let who = name("auth-refresh-2");
        m.set_cursor(Cursor::Entry(who.clone()));
        m.track(&who).facts = None;
        m.track(&who).inspection = None;
        assert_eq!(changes_rows(&mut m), [" git not read yet"]);
        m.track(&who).inspection = Some(crate::model::Inspection {
            events: None,
            at: std::time::Instant::now(),
        });
        assert_eq!(changes_rows(&mut m), [" reading git…"]);
    }

    #[test]
    fn changes_keeps_the_focus_when_it_becomes_vacant_under_the_cursor() {
        let mut m = fleet(8);
        let who = name("payroll-fix-3");
        m.set_cursor(Cursor::Entry(who.clone()));
        m.panel = Focused::Changes { file: 1 };
        m.set_test_facts(&who, Ok(crate::fixtures::unchanged()));
        let laid = laid(&mut m, (80, 40));
        assert_eq!(laid.focus, Focused::Changes { file: 0 });
    }

    #[test]
    fn changes_stays_on_screen_and_gives_git_s_error_when_it_cannot_be_read() {
        let mut m = fleet(8);
        let who = name("auth-refresh-2");
        m.set_cursor(Cursor::Entry(who.clone()));
        m.set_test_facts(&who, Err(Cause::message("fatal: not a repo".into())));
        assert_eq!(changes_rows(&mut m), [" git: fatal: not a repo"]);
    }

    #[test]
    fn activity_stays_on_screen_for_an_agent_that_has_reported_nothing() {
        let mut m = fleet(8);
        let who = name("auth-refresh-2");
        m.set_cursor(Cursor::Entry(who.clone()));
        m.activities.as_mut().expect("loaded").remove(&who);
        let laid = laid(&mut m, (80, 40));
        assert_eq!(agent_lines(&laid), [" no activity recorded"]);
        assert!(
            laid.agent_rows.is_empty(),
            "there is no row to put a cursor on"
        );
    }

    #[test]
    fn the_readiness_row_never_eats_the_file_rows() {
        for height in 12..=24 {
            let mut m = fleet(8);
            m.show_view(View::Overview);
            m.set_cursor(Cursor::Entry(name("payroll-fix-3")));
            m.toggle_changes_scope();
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
                files >= CHANGES_MIN.min(changes.counted.map_or(0, |c| c.diff.files)),
                "{height} rows: {files} file rows under readiness\n{}",
                rows.join("\n")
            );
            let detail = laid.agent.as_ref().expect("[2] shows");
            let list_clamped = list_room(height - 1) <= LIST_MIN;
            assert!(
                list_clamped || usize::from(detail.area.height) >= 3,
                "{height} rows: [2] is {} tall",
                detail.area.height
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
        m.set_facts(
            &name("rate-limit-1"),
            Err(Cause::message("fatal: not a repo\nmore".into())),
        );
        let laid = laid(&mut m, (200, 50));
        let (panel, changes) = laid.changes.as_ref().expect("[3] shows");
        assert_eq!(texts(&panel.lines), [" git: fatal: not a repo"]);
        assert!(changes.counted.is_none(), "nothing was counted");
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
            Focused::Changes { file: 0 },
            "one file, so the last row"
        );
        let short = laid(&mut m, (80, 9));
        assert!(short.changes.is_none(), "no room for [3]");
        assert_eq!(short.focus, Focused::Agents);
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
    fn a_paused_agents_activity_is_shown_under_one_elsewhere_banner() {
        let mut m = crate::fixtures::elsewhere_on("obs-layout-4");
        assert!(
            m.activity(&name("obs-layout-4")).is_some(),
            "has an activity"
        );
        let rows = screen(&render(&mut m, (200, 50)));
        let banner = rows
            .iter()
            .filter(|r| r.contains("⇄ in nvim · ⏎ move here"))
            .count();
        assert_eq!(banner, 1);
        assert!(rows.iter().any(|r| r.contains("[0] Activity")));
        assert!(rows.iter().all(|r| !r.contains("attaching to")));
    }

    #[test]
    fn a_paused_agent_without_an_activity_says_so_in_the_middle() {
        let mut m = crate::fixtures::elsewhere_on("obs-layout-4");
        m.activities.get_or_insert_default().clear();
        let rows = screen(&render(&mut m, (200, 50)));
        assert!(rows.iter().any(|r| r.contains("⇄ in nvim · ⏎ move here")));
    }

    fn typing_into(m: &mut Model, who: &str, company: Company) {
        m.record_company(&name(who), company);
        crate::fixtures::connect(m, who, Mode::Agent);
    }

    #[test]
    fn the_card_of_an_agent_open_elsewhere_is_marked_dim_and_a_squished_one_yellow() {
        let marker = |m: &Model| elsewhere_marker(m, &name("obs-layout-4")).map(|s| s.style);
        let mut m = crate::fixtures::elsewhere_on("obs-layout-4");
        assert_eq!(marker(&m), Some(dim()));
        typing_into(&mut m, "obs-layout-4", crate::fixtures::in_nvim());
        assert_eq!(marker(&m), Some(warning()));
        typing_into(&mut m, "obs-layout-4", Company::Alone);
        assert_eq!(marker(&m), None);
    }

    #[test]
    fn every_card_of_an_agent_open_elsewhere_is_marked_not_only_the_hovered_one() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.record_company(&name("rate-limit-1"), crate::fixtures::in_nvim());
        let marker = |m: &Model, who: &str| elsewhere_marker(m, &name(who)).map(|s| s.style);
        assert_eq!(marker(&m, "rate-limit-1"), Some(dim()));
        assert_eq!(marker(&m, "obs-layout-4"), None);
        let rows = screen(&render(&mut m, (200, 50)));
        let card = |who: &str| rows.iter().find(|r| r.contains(who)).expect("a card");
        assert!(card("rate-limit-1").contains("⇄"));
        assert!(!card("obs-layout-4").contains("⇄"));
    }

    #[test]
    fn typing_into_a_shared_agent_names_where_else_it_is_open_in_the_corner() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        typing_into(&mut m, "obs-layout-4", crate::fixtures::in_nvim());
        let rows = screen(&render(&mut m, (200, 50)));
        assert!(rows.iter().any(|r| r.contains("⇄ in nvim")));
        typing_into(&mut m, "obs-layout-4", Company::Alone);
        let rows = screen(&render(&mut m, (200, 50)));
        assert!(rows.iter().all(|r| !r.contains("⇄")));
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
    fn the_activity_cursor_marks_a_subagent_and_scrolls_to_keep_it_in_view() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let rows = screen(&render(&mut m, (200, 50)));
        assert!(
            !rows
                .iter()
                .any(|r| r.contains('▌') && r.contains(['├', '└']))
        );

        m.panel = Focused::activity();
        m.point_at_subagent("s1");
        let rows = screen(&render(&mut m, (200, 50)));
        let marked: Vec<&String> = rows
            .iter()
            .filter(|r| r.contains('▌') && r.contains(['├', '└']))
            .collect();
        assert_eq!(marked.len(), 1, "{}", rows.join("\n"));
        assert!(marked[0].contains("Reverting memo edit"), "{}", marked[0]);

        // In a short column [2] scrolls, and the cursor's row stays on screen.
        m.point_at_subagent("s0");
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
        m.panel = Focused::Changes { file: 0 };
        let key = due_after_rest(&mut m);
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
            std::time::Instant::now(),
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
        let key = due_after_rest(&mut m);
        m.docs.loaded(
            &key,
            Ok(crate::document::Doc::Diff(
                agent_core::ports::FileDiff::Text {
                    lines: vec!["@@ -1 +1 @@".into(), "+added".into()],
                    more: 0,
                },
            )),
            std::time::Instant::now(),
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
        m.focus_document();
        let terminal = render(&mut m, (100, 30));
        assert_eq!(border(&terminal), Color::Cyan, "[0] has them now");
    }

    fn log_of(entries: usize, finished: bool) -> agent_core::ports::Log {
        use crate::log::fixtures::{bash, said};
        use agent_core::ports::{CallState, LogEntry, Summary, Target};
        let mut log = Vec::new();
        for i in 0..entries {
            log.push(LogEntry::Called(Box::new(crate::log::fixtures::call(
                "Edit",
                Target::Path {
                    path: format!("src/file{i}.rs"),
                    range: None,
                },
                CallState::Done {
                    summary: Summary::Diff {
                        added: i,
                        removed: 0,
                    },
                    took: None,
                },
            ))));
            if i % 4 == 3 {
                log.push(said(&format!("Checked file {i}.")));
            }
        }
        log.push(bash("cargo test", CallState::Running));
        agent_core::ports::Log {
            prompt: Some("Find every call site of roundCurrency.".into()),
            outcome: if finished {
                agent_core::ports::Outcome::Finished {
                    report: Some("All done, three call sites.".to_string()),
                }
            } else {
                agent_core::ports::Outcome::Running
            },
            log,
            ..crate::log::fixtures::detail(Vec::new())
        }
    }

    #[test]
    fn the_activity_document_shows_the_whole_prompt_message_and_subagents() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
        m.panel = Focused::activity();
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("[0] Activity"), "{text}");
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
        m.panel = Focused::activity();
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("Subagents"), "{text}");
        assert!(text.contains("Reverting memo edit"), "{text}");
    }

    #[test]
    fn a_stopped_subagent_says_stopped_and_why() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::activity();
        m.point_at_subagent("s1");
        let mut detail = log_of(2, false);
        detail.outcome = agent_core::ports::Outcome::Stopped {
            reason: "API Error: overloaded".into(),
        };
        m.set_log((name("obs-layout-4"), AgentId::new("s1")), Ok(Some(detail)));
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("stopped"), "{text}");
        assert!(text.contains("API Error: overloaded"), "{text}");
        assert!(!text.contains("no report"), "{text}");
    }

    #[test]
    fn a_subagents_log_follows_its_tail_and_pauses_when_scrolled_up() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::activity();
        m.point_at_subagent("s1");
        let loading = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(
            loading.contains("[0] Reverting memo edit · general-purpose"),
            "{loading}"
        );

        m.set_log(
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
            call.contains("+38") && !call.contains("−0"),
            "its result sits beside it\n{call}"
        );

        m.docs.scroll_by(-5);
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("paused "), "{text}");
        assert!(!text.contains("following"));

        m.docs.scroll_to_end();
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("following"), "G resumes\n{text}");

        m.set_log(
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
            .position(|l| l.contains("Calls ·"))
            .expect("an activity rule");
        let report = text
            .lines()
            .position(|l| l.contains("All done, three call sites."))
            .expect("the report");
        assert!(
            report < rule,
            "the report comes before the activity\n{text}"
        );
        assert!(text.contains("✓ finished"), "{text}");
    }

    #[test]
    fn a_subagent_without_a_transcript_says_so() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::activity();
        m.point_at_subagent("s1");
        m.set_log((name("obs-layout-4"), AgentId::new("s1")), Ok(None));
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(text.contains("no transcript for this subagent"), "{text}");
    }

    #[test]
    fn the_readiness_row_reads_the_branch_and_full_screen_hides_the_sidebar_with_the_position_in_the_title()
     {
        use agent_core::ports::BranchLog;
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.toggle_changes_scope();
        m.panel = Focused::Changes { file: 0 };
        let key = due_after_rest(&mut m);
        m.docs.loaded(
            &key,
            Ok(crate::document::Doc::Branch(BranchLog {
                commits: vec!["abc1234 Fix the rounding".into()],
                uncommitted: vec![agent_core::ports::Uncommitted {
                    path: "src/a.rs".into(),
                    state: FileState::Unstaged(Edit::Modified),
                }],
            })),
            std::time::Instant::now(),
        );
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(
            text.contains("[0] Branch · feature vs origin/main"),
            "{text}"
        );
        assert!(!text.contains("since it left"), "{text}");
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
        m.toggle_changes_scope();
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
        let activity_y = m.areas.agent_rows[0].0;
        click(&mut m, 5, activity_y, left());
        assert!(matches!(
            m.hover(),
            Some(crate::document::Hover::Activity(_))
        ));

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
        m.toggle_changes_scope();
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
        crate::fixtures::connect(&mut m, "obs-layout-4", crate::model::Mode::Nav);
        m.peek = Some(crate::model::Peek::Pane);
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
    fn the_peek_of_an_agent_open_elsewhere_names_where() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.record_company(&name("obs-layout-4"), crate::fixtures::in_nvim());
        m.pane = crate::model::Pane::Elsewhere {
            name: name("obs-layout-4"),
        };
        m.peek = Some(crate::model::Peek::Pane);
        let rows = screen(&render(&mut m, (100, 30)));
        let title = rows.iter().find(|r| r.contains("╔")).expect("a float");
        assert!(title.contains("⇄ in nvim"), "{title}");
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
        m.panel = Focused::activity();
        m.point_at_subagent("s1");
        let mut all = m.activities.take().expect("reported");
        let agent = name("obs-layout-4");
        let started = Reported::subagent(
            AgentId::new("s1"),
            Common::ToolStarted {
                id: ToolId::new("t"),
                tool: "Bash".into(),
                target: Some("cargo test".into()),
            },
        );
        let at = SystemTime::now() - std::time::Duration::from_secs(12);
        let activity = Activity::next(all.remove(&agent), started, at);
        all.insert(agent, activity);
        m.activities = Some(all);
        m.set_log(
            (name("obs-layout-4"), AgentId::new("s1")),
            Ok(Some(log_of(2, false))),
        );
        let text = screen(&render(&mut m, (100, 30))).join("\n");
        assert!(
            text.lines()
                .any(|l| l.contains("cargo test") && (l.contains("11s") || l.contains("12s"))),
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
            Focused::activity(),
            Focused::Changes { file: 0 },
        ] {
            let mut counts = Vec::new();
            for size in [(80, 24), (100, 30), (200, 50)] {
                let mut m = fleet(10);
                m.set_cursor(Cursor::Entry(name("obs-layout-4")));
                m.panel = panel.clone();
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

    fn talking(m: &mut Model, quote: Option<crate::log::Quote>, text: &str) {
        let to = name("obs-layout-4");
        m.track(&to).compose = Some(crate::track::Compose::Editing {
            quote,
            text: text.into(),
        });
        m.overlay = Overlay::Talk(to);
    }

    fn send_box(m: &mut Model) -> String {
        talking(m, None, "");
        screen(&render(m, (100, 30))).join("\n")
    }

    #[test]
    fn the_send_box_says_send_while_the_agent_works() {
        let mut m = fleet(10);
        m.activities.get_or_insert_default().insert(
            name("obs-layout-4"),
            crate::fixtures::reported(State::Working { tools: Vec::new() }, 30),
        );
        let text = send_box(&mut m);
        assert!(text.contains("Send to obs-layout-4"), "{text}");
        assert!(text.contains("⏎ send") && !text.contains("held"), "{text}");
    }

    fn replying(m: &mut Model, quote: &str, text: &str) -> String {
        use crate::log::{Entry, Quote, Whose};
        talking(m, Quote::of(&Entry::Said(quote), Whose::Activity), text);
        screen(&render(m, (100, 30))).join("\n")
    }

    #[test]
    fn a_reply_box_shows_the_quote_above_the_input_and_what_was_typed_apart_from_it() {
        let mut m = fleet(10);
        let text = replying(&mut m, "Bash cargo test", "why did it fail?");
        assert!(text.contains("Reply to obs-layout-4"), "{text}");
        let rows: Vec<&str> = text.lines().collect();
        let quoted = rows.iter().position(|r| r.contains("> Bash cargo test"));
        let typed = rows.iter().position(|r| r.contains("› why did it fail?"));
        assert!(
            matches!((quoted, typed), (Some(q), Some(t)) if q + 1 == t),
            "{text}"
        );
        assert!(!rows[typed.expect("typed")].contains("Bash"), "{text}");
    }

    #[test]
    fn a_long_quote_is_cut_to_the_box_and_the_data_keeps_all_of_it() {
        let mut m = fleet(10);
        let long = "word ".repeat(40);
        let text = replying(&mut m, &long, "");
        let row = text
            .lines()
            .find(|r| r.contains("> word"))
            .expect("the quote row");
        assert!(row.contains('…'), "{row}");
    }

    #[test]
    fn a_reply_with_review_comments_drafted_is_titled_a_review() {
        let mut m = fleet(10);
        m.track(&name("obs-layout-4")).draft.push(
            crate::review::Comment::on(
                &[&crate::fixtures::line_info("a.rs", "+x", 1)],
                None,
                "note",
            )
            .expect("a comment"),
        );
        let text = replying(&mut m, "one", "");
        assert!(text.contains("Send review to obs-layout-4"), "{text}");
        assert!(text.contains("> one"), "{text}");
    }

    #[test]
    fn a_reply_box_squeezed_by_a_short_or_narrow_screen_keeps_its_input_and_the_quote() {
        use crate::log::{Entry, Quote, Whose};
        let mut m = fleet(10);
        m.track(&name("obs-layout-4")).draft.push(
            crate::review::Comment::on(
                &[&crate::fixtures::line_info("a.rs", "+x", 1)],
                None,
                "note",
            )
            .expect("a comment"),
        );
        talking(
            &mut m,
            Quote::of(&Entry::Said("the quote"), Whose::Activity),
            "typed",
        );
        for height in [3, 4, 5, 6, 8] {
            let text = screen(&render(&mut m, (100, height))).join("\n");
            assert!(text.contains("› typed"), "{height}: {text}");
            assert!(text.contains("the quote"), "{height}: {text}");
        }
        for width in [6, 9, 12] {
            for height in [3, 4] {
                let text = screen(&render(&mut m, (width, height))).join("\n");
                assert!(text.contains('›'), "{width}x{height}: {text}");
            }
        }
    }

    #[test]
    fn a_reply_box_with_room_shows_the_quote_above_the_input_not_in_the_title() {
        let mut m = fleet(10);
        let text = replying(&mut m, "the quote", "typed");
        let title = text
            .lines()
            .find(|r| r.contains("Reply to"))
            .expect("a title");
        assert!(!title.contains("the quote"), "{title}");
    }

    #[test]
    fn the_footer_says_reply_for_s_on_a_log_entry_and_send_elsewhere() {
        let mut m = reading_a_log();
        let footer = footer_of(&mut m, (200, 40));
        assert!(footer.contains("s reply"), "{footer}");
        assert!(footer.contains("message"), "{footer}");
        let mut m = fleet(10);
        let footer = footer_of(&mut m, (200, 40));
        assert!(
            footer.contains("s send") && !footer.contains("s reply"),
            "{footer}"
        );
    }

    #[test]
    fn the_footer_says_send_in_a_peek_over_a_log_because_s_there_sends_without_a_quote() {
        let mut m = reading_a_log();
        m.peek = Some(crate::model::Peek::Pane);
        m.settle();
        let footer = footer_of(&mut m, (200, 40));
        assert!(
            footer.contains("s send") && !footer.contains("s reply"),
            "{footer}"
        );
    }

    #[test]
    fn the_send_box_says_why_while_a_question_is_open() {
        let mut m = fleet(10);
        crate::fixtures::ask(&mut m, "obs-layout-4");
        let text = send_box(&mut m);
        assert!(text.contains("Send to obs-layout-4 · asking"), "{text}");
    }

    #[test]
    fn a_card_shows_an_interrupted_agent_as_stopped_not_working() {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.activities.get_or_insert_default().insert(
            name("obs-layout-4"),
            crate::fixtures::reported(State::Interrupted, 90),
        );
        let rows = screen(&render(&mut m, (100, 30)));
        let at = rows
            .iter()
            .position(|r| r.contains("obs-layout-4") && r.contains('■'))
            .unwrap_or_else(|| panic!("a ■ on its row\n{}", rows.join("\n")));
        assert!(rows[at].contains("1m"), "how long ago\n{}", rows[at]);
        assert!(
            rows[at + 1].contains("interrupted"),
            "{}\n{}",
            rows[at],
            rows[at + 1]
        );
        assert!(
            !super::super::SPINNER.iter().any(|f| rows[at].contains(f)),
            "{}",
            rows[at]
        );
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
            (Focused::activity(), "[2] Activity"),
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
        let Some(Ok(read)) = m
            .track(&name("obs-layout-4"))
            .facts
            .as_mut()
            .map(|f| &mut f.result)
        else {
            panic!("facts")
        };
        let facts = &mut read.facts;
        facts.changed = [
            ("crates/tui/src/lib.rs", 2, 1),
            ("crates/tui/src/model.rs", 10, 2),
            ("crates/tui/src/view/sidebar.rs", 4, 0),
            ("docs/design.md", 1, 1),
        ]
        .into_iter()
        .map(|(p, added, removed)| {
            agent_core::ports::Change::tracked(
                p,
                Lines::Counted { added, removed },
                agent_core::ports::TrackedState::Committed,
            )
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
        m.panel = Focused::Changes { file: 0 };
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
    fn a_folder_lists_its_files_then_a_section_per_file_and_says_how_many_were_left_out() {
        use crate::document::{Doc, FolderDocs, FolderFile};
        use agent_core::ports::FileDiff;
        let mut m = tree_fleet();
        m.panel = Focused::Changes { file: 0 };
        m.settle();
        let before = screen(&render(&mut m, (140, 40))).join("\n");
        assert!(
            before.contains("view/sidebar.rs"),
            "the list shows at once\n{before}"
        );
        assert!(!before.contains("@@"), "{before}");

        std::thread::sleep(crate::document::REST + std::time::Duration::from_millis(20));
        let key = m.docs.due(std::time::Instant::now()).expect("due");
        let crate::document::Key::Folder(folder) = &key else {
            panic!("a folder is read as a folder");
        };
        assert_eq!(
            folder.base,
            agent_core::ports::DiffBase::Head,
            "the Uncommitted tab reads against HEAD"
        );
        let files = folder
            .files
            .iter()
            .map(|change| FolderFile {
                change: change.clone(),
                diff: Ok(FileDiff::Text {
                    lines: vec!["@@ -1 +1 @@".into(), "-old".into(), "+new".into()],
                    more: 0,
                }),
            })
            .collect();
        m.docs.loaded(
            &key,
            Ok(Doc::Folder(FolderDocs {
                files,
                more_files: 4,
            })),
            std::time::Instant::now(),
        );
        m.settle();
        let text = screen(&render(&mut m, (140, 80))).join("\n");
        assert_eq!(text.matches("@@ -1 +1 @@").count(), 3, "{text}");
        assert!(text.contains("+new"), "{text}");
        assert!(
            text.contains("+4 more files, open one to see its diff"),
            "{text}"
        );
    }

    /// Keys a handler answers only to say they moved.
    const RETIRED_KEYS: [&str; 1] = ["t"];

    fn shown_as(key: &KeyEvent) -> String {
        let name = match key.code {
            KeyCode::Char(' ') => "Space".into(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "⏎".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "⇧Tab".into(),
            KeyCode::PageDown => "PgDn".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            other => unreachable!("not probed: {other:?}"),
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            format!("⌃{}", name.to_uppercase())
        } else {
            name
        }
    }

    fn probed_keys() -> Vec<KeyEvent> {
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let named = [
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::PageDown,
            KeyCode::PageUp,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
        ];
        let controls = ['d', 'u'].map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        (' '..='~')
            .map(KeyCode::Char)
            .chain(named)
            .map(plain)
            .chain(controls)
            .collect()
    }

    fn reading_a_log() -> Model {
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::activity();
        m.point_at_subagent("s1");
        m.set_log(
            (name("obs-layout-4"), AgentId::new("s1")),
            Ok(Some(log_of(2, false))),
        );
        render(&mut m, (140, 40));
        m.focus_document();
        m.settle();
        m
    }

    fn in_panel(panel: Focused) -> Model {
        let mut m = tree_fleet();
        m.panel = panel;
        m.settle();
        m
    }

    #[test]
    fn nav_keys_do_something_exactly_where_the_keymap_table_says() {
        use crate::model::{ActivityRow, Origin};
        let contexts: [(Ctx, fn() -> Model); 7] = [
            (Ctx::Agents, tree_fleet),
            (Ctx::Activity, || in_panel(Focused::activity())),
            (Ctx::Changes, || in_panel(Focused::Changes { file: 0 })),
            (Ctx::Main, || {
                in_panel(Focused::Main(
                    Origin::Activity {
                        offset: 0,
                        row: ActivityRow::Main,
                    },
                    None,
                ))
            }),
            (Ctx::Main, on_a_loaded_diff),
            (Ctx::Main, reading_a_log),
            (Ctx::Peek, || {
                let mut m = tree_fleet();
                let selected = m.selected_entry().expect("an agent").name().clone();
                crate::fixtures::connect(&mut m, selected.as_str(), crate::model::Mode::Nav);
                m.peek = Some(crate::model::Peek::Pane);
                m
            }),
        ];
        let keys = probed_keys();
        let mut wrong = Vec::new();
        for ctx in [
            Ctx::Agents,
            Ctx::Activity,
            Ctx::Changes,
            Ctx::Main,
            Ctx::Peek,
        ] {
            let mut handled = std::collections::BTreeSet::new();
            for (_, build) in contexts.iter().filter(|(c, _)| *c == ctx) {
                for key in &keys {
                    if crate::update::nav_or_peek_command(&mut build(), key).is_some() {
                        handled.insert(shown_as(key));
                    }
                }
            }
            let table = crate::keymap::keys_in(ctx);
            for key in &handled {
                let said = table.contains(&key.as_str()) || RETIRED_KEYS.contains(&key.as_str());
                if !said {
                    wrong.push(format!(
                        "{key} does something in {ctx:?}, the table is silent"
                    ));
                }
            }
            for key in &table {
                // The nav chord is taken by on_key before nav_or_peek_command sees it.
                let does = handled.contains(*key) || key.contains("{nav}");
                if !does {
                    wrong.push(format!(
                        "{key} is in the table for {ctx:?} but does nothing"
                    ));
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    fn press(m: &mut Model, code: crossterm::event::KeyCode) -> Vec<crate::update::Effect> {
        crate::update::update(
            m,
            crate::worker::Msg::Key(crossterm::event::KeyEvent::new(
                code,
                crossterm::event::KeyModifiers::NONE,
            )),
        )
    }

    /// [3] on a file whose diff is `lines`, loaded and drawn, with [0] not
    /// focused.
    fn with_a_loaded_diff(lines: &[String]) -> Model {
        use crate::document::Doc;
        use agent_core::ports::FileDiff;
        let mut m = tree_fleet();
        m.panel = Focused::Changes { file: 2 };
        let key = due_after_rest(&mut m);
        m.docs.loaded(
            &key,
            Ok(Doc::Diff(FileDiff::Text {
                lines: lines.to_vec(),
                more: 0,
            })),
            std::time::Instant::now(),
        );
        m.settle();
        render(&mut m, (140, 40));
        m
    }

    fn on_a_loaded_diff() -> Model {
        let lines = [
            "@@ -10,3 +20,4 @@",
            " ctx",
            "-gone",
            "+one",
            "+two",
            "@@ -40 +50 @@",
            "+far",
        ];
        let mut m = with_a_loaded_diff(&lines.map(String::from));
        m.focus_document();
        m
    }

    #[test]
    fn the_diff_cursor_starts_on_a_line_that_shows_when_the_diff_was_scrolled_before_it_got_the_keys()
     {
        use crossterm::event::KeyCode;
        let lines: Vec<String> = std::iter::once("@@ -1,60 +1,60 @@".to_string())
            .chain((0..60).map(|n| format!("+line {n}")))
            .collect();
        let mut m = with_a_loaded_diff(&lines);
        m.docs.scroll_by(30);
        render(&mut m, (140, 24));
        m.focus_document();
        let top = m.areas.main.as_ref().expect("a document").top(&m.docs);
        let line = &m.docs.lines()[m.diff_view.cursor];
        assert!(
            line.row >= top && line.row < top + 10,
            "the cursor is on row {} and the screen starts at {top}",
            line.row
        );
        press(&mut m, KeyCode::Char('j'));
        render(&mut m, (140, 24));
        let after = m.areas.main.as_ref().expect("a document").top(&m.docs);
        assert!(
            after.abs_diff(top) <= 1,
            "the screen stays: {top} to {after}"
        );
    }

    #[test]
    fn the_diff_cursor_walks_lines_jumps_hunks_and_edits_at_the_line() {
        use crossterm::event::KeyCode;
        let mut m = on_a_loaded_diff();
        let text = screen(&render(&mut m, (140, 40))).join("\n");
        assert!(text.contains("▌ ctx"), "{text}");
        press(&mut m, KeyCode::Char('j'));
        press(&mut m, KeyCode::Char('j'));
        let effects = press(&mut m, KeyCode::Char('e'));
        assert!(
            matches!(effects.as_slice(), [crate::update::Effect::Edit(crate::update::Open::At { file, line, .. })]
                if file.ends_with("sidebar.rs") && line.get() == 21),
            "the added line opens at its place in the new file"
        );
        press(&mut m, KeyCode::Char('k'));
        let effects = press(&mut m, KeyCode::Char('e'));
        assert!(
            matches!(
                effects.as_slice(),
                [crate::update::Effect::Edit(crate::update::Open::At { line, .. })]
                    if line.get() == 21
            ),
            "a removed line opens where it was"
        );
        press(&mut m, KeyCode::Char(']'));
        let text = screen(&render(&mut m, (140, 40))).join("\n");
        assert!(
            text.contains("▌+far"),
            "] lands on the next hunk's first line\n{text}"
        );
        press(&mut m, KeyCode::Char('['));
        press(&mut m, KeyCode::Char('['));
        let text = screen(&render(&mut m, (140, 40))).join("\n");
        assert!(text.contains("▌ ctx"), "[ goes back a hunk\n{text}");
        press(&mut m, KeyCode::Char('G'));
        let text = screen(&render(&mut m, (140, 40))).join("\n");
        assert!(text.contains("▌+far"), "{text}");
    }

    #[test]
    fn c_drafts_a_comment_on_the_line_and_the_footer_fits_the_diff_keys_at_100_columns() {
        use crossterm::event::KeyCode;
        let mut m = on_a_loaded_diff();
        let footer = footer_of(&mut m, (100, 30));
        for hint in ["c comment", "e edit line", "[/] hunk", "Esc back"] {
            assert!(footer.contains(hint), "{hint}: {footer}");
        }
        press(&mut m, KeyCode::Char('j'));
        press(&mut m, KeyCode::Char('c'));
        assert!(matches!(m.overlay, crate::model::Overlay::Comment(_)));
        let text = screen(&render(&mut m, (140, 40))).join("\n");
        assert!(
            text.contains("Comment · crates/tui/src/view/sidebar.rs:"),
            "{text}"
        );
        for c in "Why?".chars() {
            press(&mut m, KeyCode::Char(c));
        }
        press(&mut m, KeyCode::Enter);
        assert!(matches!(m.overlay, crate::model::Overlay::None));
        assert_eq!(m.drafted(), 1);
        press(&mut m, KeyCode::Char('k'));
        let text = screen(&render(&mut m, (140, 40))).join("\n");
        assert!(text.contains("✎"), "a commented line is marked\n{text}");
        assert!(text.contains("1 comment"), "{text}");
    }

    fn a_commits_lines_are_not_edited_and_comments_name_the_commit(m: &mut Model) {
        use crossterm::event::KeyCode;
        let effects = press(m, KeyCode::Char('e'));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, crate::update::Effect::Edit(_))),
            "e doesn't open the working tree's file for a commit's line"
        );
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("e opens the checkout")),
            "and says why"
        );
        press(m, KeyCode::Char('c'));
        press(m, KeyCode::Char('x'));
        press(m, KeyCode::Enter);
        let draft: Vec<_> = m.agents.values().flat_map(|t| &t.draft).collect();
        assert_eq!(draft.len(), 1);
        assert!(
            draft[0].label.starts_with("src/a.rs@abc1234:"),
            "{}",
            draft[0].label
        );
    }

    #[test]
    fn enter_on_a_commit_in_the_branch_opens_its_files_and_escape_goes_back() {
        use crate::document::{Doc, FolderDocs, FolderFile, Hover, Key};
        use agent_core::ports::{BranchLog, Change, FileDiff, FileState, Lines, TrackedState};
        use crossterm::event::KeyCode;
        let mut m = fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.toggle_changes_scope();
        m.panel = Focused::Changes { file: 0 };
        let key = due_after_rest(&mut m);
        m.docs.loaded(
            &key,
            Ok(Doc::Branch(BranchLog {
                commits: vec![
                    "abc1234 Fix the rounding".into(),
                    "def5678 Add tests".into(),
                ],
                uncommitted: vec![agent_core::ports::Uncommitted {
                    path: "src/a.rs".into(),
                    state: FileState::Unstaged(Edit::Modified),
                }],
            })),
            std::time::Instant::now(),
        );
        render(&mut m, (140, 40));
        m.focus_document();
        let text = screen(&render(&mut m, (140, 40))).join("\n");
        assert!(
            text.contains("▌abc1234"),
            "the cursor starts on the first commit\n{text}"
        );
        press(&mut m, KeyCode::Char('j'));
        press(&mut m, KeyCode::Char('k'));
        let effects = press(&mut m, KeyCode::Enter);
        let _ = effects;
        let Some(Hover::Commit(commit)) = m.hover() else {
            panic!("a commit opens");
        };
        assert_eq!(commit.sha, "abc1234");
        m.settle();
        std::thread::sleep(crate::document::REST + std::time::Duration::from_millis(20));
        let key = m
            .docs
            .due(std::time::Instant::now())
            .expect("the commit is read");
        assert!(matches!(key, Key::Commit(_)));
        let change = Change::tracked(
            "src/a.rs",
            Lines::Counted {
                added: 1,
                removed: 0,
            },
            TrackedState::Committed,
        );
        m.docs.loaded(
            &key,
            Ok(Doc::Folder(FolderDocs {
                files: vec![FolderFile {
                    change,
                    diff: Ok(FileDiff::Text {
                        lines: vec!["@@ -1 +1,2 @@".into(), " a".into(), "+b".into()],
                        more: 0,
                    }),
                }],
                more_files: 0,
            })),
            std::time::Instant::now(),
        );
        m.settle();
        let text = screen(&render(&mut m, (140, 40))).join("\n");
        assert!(
            text.contains("[0] commit abc1234 Fix the rounding"),
            "{text}"
        );
        assert!(text.contains("+b"), "{text}");
        a_commits_lines_are_not_edited_and_comments_name_the_commit(&mut m);
        press(&mut m, KeyCode::Char('j'));
        press(&mut m, KeyCode::Esc);
        assert!(
            matches!(m.hover(), Some(Hover::Branch(_))),
            "Esc goes back one level"
        );
        assert!(m.main_focused(), "and [0] keeps the keys");
        press(&mut m, KeyCode::Esc);
        assert!(!m.main_focused());
    }

    #[test]
    fn v_selects_a_range_for_one_comment_and_j_and_k_in_capitals_change_file() {
        use crossterm::event::KeyCode;
        let mut m = on_a_loaded_diff();
        press(&mut m, KeyCode::Char('V'));
        press(&mut m, KeyCode::Char('j'));
        press(&mut m, KeyCode::Char('j'));
        let text = screen(&render(&mut m, (140, 40))).join("\n");
        assert!(text.matches('▌').count() >= 3, "{text}");
        press(&mut m, KeyCode::Char('c'));
        for c in "range".chars() {
            press(&mut m, KeyCode::Char(c));
        }
        press(&mut m, KeyCode::Enter);
        let draft: Vec<_> = m.agents.values().flat_map(|t| &t.draft).collect();
        assert_eq!(draft.len(), 1);
        assert!(draft[0].label.ends_with(":20-21"), "{}", draft[0].label);
        assert_eq!(draft[0].quoted.len(), 3);

        press(&mut m, KeyCode::Char('V'));
        press(&mut m, KeyCode::Esc);
        assert!(
            m.main_focused(),
            "Esc drops a selection before it leaves [0]"
        );
        press(&mut m, KeyCode::Esc);
        assert!(!m.main_focused());

        let mut m = on_a_loaded_diff();
        let file = |m: &Model| match m.panel.list() {
            Listed::Changes { file } => file,
            other => panic!("[3] has the keys: {other:?}"),
        };
        let before = file(&m);
        press(&mut m, KeyCode::Char('J'));
        assert_ne!(file(&m), before, "J moves [3]'s cursor to the next file");
        assert!(m.main_focused());
        press(&mut m, KeyCode::Char('K'));
        assert_eq!(file(&m), before);
    }

    #[test]
    fn y_copies_the_place_the_selected_lines_a_sha_and_a_log_entry() {
        use crate::update::Effect;
        use crossterm::event::KeyCode;
        let copied = |effects: Vec<Effect>| match effects.as_slice() {
            [Effect::Copy(text)] => text.clone(),
            other => panic!("expected one copy, got {} effects", other.len()),
        };
        let mut m = on_a_loaded_diff();
        press(&mut m, KeyCode::Char('j'));
        press(&mut m, KeyCode::Char('j'));
        assert_eq!(
            copied(press(&mut m, KeyCode::Char('y'))),
            "crates/tui/src/view/sidebar.rs:21"
        );
        press(&mut m, KeyCode::Char('V'));
        press(&mut m, KeyCode::Char('j'));
        assert_eq!(copied(press(&mut m, KeyCode::Char('y'))), "one\ntwo");
        assert!(m.diff_view.anchor.is_none());
    }

    #[test]
    fn clicks_and_the_wheel_work_on_tree_rows() {
        let mut m = tree_fleet();
        render(&mut m, (100, 50));
        let (y, row) = m.areas.change_rows[2];
        assert_eq!(row, 2);
        click(&mut m, 5, y, left());
        assert_eq!(m.panel, Focused::Changes { file: 2 });
        assert_eq!(
            m.change_row(2).map(|r| r.label),
            Some("sidebar.rs".to_string())
        );
        assert!(matches!(m.hover(), Some(crate::document::Hover::Diff(_))));
        let (y, _) = m.areas.change_rows[0];
        click(&mut m, 5, y, left());
        assert!(matches!(m.hover(), Some(crate::document::Hover::Folder(_))));
        click(&mut m, 5, y, crossterm::event::MouseEventKind::ScrollDown);
        assert_eq!(m.panel, Focused::Changes { file: 1 });
    }

    #[test]
    fn a_folded_folder_shows_a_closed_arrow_and_the_rows_below_move_up() {
        let mut m = tree_fleet();
        m.panel = Focused::Changes { file: 0 };
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

    mod detail {
        use std::time::Duration;

        use agent_core::activity::{Answer, Ask, Origin, State, Tool, ToolId};
        use agent_core::ports::{
            ActivityDetail, Asked, CallState, Choice, LogEntry, Outcome, Progress, Question,
            Summary, Target,
        };

        use super::*;
        use crate::log::fixtures::{activity_at, before, call_at, done, said_at, story};

        fn at(m: &mut Model, agent: &str, size: (u16, u16)) -> SessionName {
            let who = name(agent);
            m.set_cursor(Cursor::Entry(who.clone()));
            m.panel = Focused::activity();
            let _ = size;
            who
        }

        /// The agent's [0] as rows, after the sidebar, with its bottom border.
        fn page(m: &mut Model, size: (u16, u16)) -> Vec<String> {
            let terminal = render(m, size);
            let area = m.areas.terminal;
            screen(&terminal)
                .into_iter()
                .skip(usize::from(area.y))
                .take(usize::from(area.height) + 1)
                .map(|row| {
                    row.chars()
                        .skip(usize::from(area.x))
                        .take(usize::from(area.width))
                        .collect()
                })
                .collect()
        }

        fn working(m: &mut Model, agent: &str) -> SessionName {
            let who = at(m, agent, (140, 40));
            let activity = m
                .activities
                .as_mut()
                .and_then(|a| a.get_mut(&who))
                .expect("reports");
            activity.state = State::Working {
                tools: vec![Tool {
                    id: ToolId::new("b2"),
                    name: "Bash".into(),
                    target: Some("nvim --headless".into()),
                    since: m.clock.wall - Duration::from_secs(12),
                }],
            };
            activity.since = m.clock.wall - Duration::from_secs(372);
            let detail = activity_at(
                m.clock.wall,
                story(m.clock.wall),
                Some(call_at(
                    m.clock.wall,
                    "b2",
                    "Bash",
                    Target::Command(
                        "nvim --headless -c 'lua require(\"heucristic.status\")' -c q".into(),
                    ),
                    CallState::Running,
                    12,
                )),
            );
            m.set_activity(&who, Ok(Some(detail)));
            who
        }

        #[test]
        fn a_path_under_home_reads_from_the_models_home_not_the_environment() {
            let mut m = fleet(10);
            let who = working(&mut m, "payroll-fix-3");
            let now = m.clock.wall;
            let read = call_at(
                now,
                "r9",
                "Read",
                crate::log::fixtures::path("/home/dev/notes/todo.md"),
                done(Summary::Lines(3)),
                20,
            );
            m.set_activity(&who, Ok(Some(activity_at(now, vec![read], None))));
            m.home = Some("/home/dev".into());
            let text = page(&mut m, (140, 40)).join("\n");
            assert!(text.contains("~/notes/todo.md"), "{text}");
            m.home = None;
            let text = page(&mut m, (140, 40)).join("\n");
            assert!(text.contains("/home/dev/notes/todo.md"), "{text}");
        }

        #[test]
        fn a_working_activity_shows_progress_older_phases_folded_and_the_current_one_whole() {
            let mut m = fleet(10);
            working(&mut m, "payroll-fix-3");
            for size in [(140, 40), (70, 26)] {
                let rows = page(&mut m, size);
                let text = rows.join("\n");
                assert!(text.contains("working 6m12s"), "{size:?}\n{text}");
                assert!(text.contains("calls"), "{size:?}\n{text}");
                assert!(text.contains("1 err"), "{size:?}\n{text}");
                assert!(
                    size.0 < 100 || text.contains("› Are you able to run manual QA"),
                    "the prompt is on top until a narrow log follows its tail off it\n{text}"
                );
                for folded in ["I'll read how th", "The provider need"] {
                    let row = rows
                        .iter()
                        .find(|r| r.contains(folded))
                        .unwrap_or_else(|| panic!("{folded}\n{text}"));
                    assert!(row.contains('▸') && row.contains("call"), "{row}");
                }
                assert!(
                    !text.contains("while I write the module."),
                    "an older phase is a heading, not its whole text\n{text}"
                );
                assert!(
                    text.contains("guarding it and running it again."),
                    "the current phase in full\n{text}"
                );
                assert!(text.contains("E5113"), "the failure's first line\n{text}");
                assert!(text.contains("running"), "the live call\n{text}");
                assert!(text.contains("following"), "{text}");
            }
        }

        #[test]
        fn a_phase_with_two_subagents_says_so_and_a_diff_phase_its_counts() {
            let mut m = fleet(10);
            working(&mut m, "payroll-fix-3");
            let text = page(&mut m, (140, 40)).join("\n");
            assert!(text.contains("+4 −2"), "{text}");
            assert!(text.contains("↳2"), "{text}");
        }

        #[test]
        fn opening_a_folded_phase_shows_what_it_said_and_did() {
            let mut m = fleet(10);
            let who = working(&mut m, "payroll-fix-3");
            m.focus_document();
            render(&mut m, (140, 40));
            let second = {
                let detail = m.activity_detail(&who).expect("read");
                crate::log::activity_entries(&detail.log, None, &[]).activity[1].1
            };
            m.track(&who)
                .activity_view
                .watch
                .nav
                .open
                .insert(crate::log::EntryKey::Log(second));
            let text = page(&mut m, (140, 40)).join("\n");
            assert!(text.contains("wiring it into lsp.lua."), "{text}");
            assert!(text.contains("lsp.lua") && text.contains("Edit"), "{text}");
        }

        fn asking(
            m: &mut Model,
            agent: &str,
            questions: Vec<Question>,
            running: LogEntry,
        ) -> SessionName {
            let who = at(m, agent, (140, 40));
            let activity = m
                .activities
                .as_mut()
                .and_then(|a| a.get_mut(&who))
                .expect("reports");
            let call = match &running {
                LogEntry::Called(call) => call.id.clone(),
                other => panic!("{other:?}"),
            };
            activity.asks.push(Ask {
                asker: Origin::Main,
                question: "Meta-session: drop the grid?".into(),
                answered_by: Answer::ToolEnding(call.clone()),
                since: m.clock.wall - Duration::from_secs(120),
            });
            let log = vec![
                said_at(
                    m.clock.wall,
                    "Listing what still references the meta-session.",
                    100,
                ),
                call_at(
                    m.clock.wall,
                    "g",
                    "Grep",
                    Target::Search {
                        pattern: "meta_session".into(),
                        path: None,
                    },
                    done(Summary::Matches(14)),
                    95,
                ),
                said_at(
                    m.clock.wall,
                    "Three callers still use it. Before deleting them I want to confirm you don't still use the grid anywhere.",
                    90,
                ),
            ];
            let mut detail = activity_at(m.clock.wall, log, Some(running));
            if !questions.is_empty() {
                detail.progress = Progress::Running {
                    asked: Some(Asked { call, questions }),
                };
            }
            m.set_activity(&who, Ok(Some(detail)));
            who
        }

        fn grid() -> Vec<Question> {
            vec![Question {
                header: "Meta-session".into(),
                text: "The TUI replaces the agents meta-session/grid. Should the binary drop everything built around it (agent restore, agent reset, hide's meta-pane handling)?".into(),
                options: vec![
                    Choice {
                        label: "Drop it (Recommended)".into(),
                        description: "No grid anymore. `hide` becomes a TUI-side filter stored in a state file.".into(),
                    },
                    Choice {
                        label: "Keep for now".into(),
                        description: "Port restore/reset/hide meta behaviour too, so the old grid keeps working.".into(),
                    },
                ],
                multi: false,
            }]
        }

        #[test]
        fn an_asking_activity_pins_the_whole_question_with_its_options_and_replaces_the_banner() {
            let mut m = fleet(10);
            let ask = call_at(
                m.clock.wall,
                "q",
                "AskUserQuestion",
                Target::None,
                CallState::Running,
                120,
            );
            asking(&mut m, "payroll-fix-3", grid(), ask);
            for size in [(140, 40), (70, 26)] {
                let rows = page(&mut m, size);
                let text = rows.join("\n");
                assert!(rows[0].contains("asking 2m · Me"), "{size:?}\n{text}");
                assert!(
                    text.contains("The TUI replaces the agents"),
                    "{size:?}\n{text}"
                );
                assert!(text.contains("Drop it (Recommended)"), "{size:?}\n{text}");
                assert!(text.contains("Keep for now"), "{size:?}\n{text}");
                assert!(text.contains("Other"), "{size:?}\n{text}");
                assert_eq!(
                    rows.iter().filter(|r| r.contains("◆")).count(),
                    2,
                    "the status row and the call that waits, with no banner above them\n{text}"
                );
                let waiting = rows
                    .iter()
                    .find(|r| r.contains("waiting"))
                    .unwrap_or_else(|| panic!("{text}"));
                assert!(waiting.contains("AskUserQuestion"), "{waiting}");
            }
        }

        #[test]
        fn at_any_width_from_one_to_thirty_an_activity_draws_without_overflowing_its_pane() {
            let mut working_agent = fleet(10);
            working(&mut working_agent, "payroll-fix-3");
            let mut asking_agent = fleet(10);
            let ask = call_at(
                asking_agent.clock.wall,
                "q",
                "AskUserQuestion",
                Target::None,
                CallState::Running,
                120,
            );
            asking(&mut asking_agent, "payroll-fix-3", grid(), ask);
            let mut done_agent = fleet(10);
            touching(
                &mut done_agent,
                "auth-refresh-2",
                Progress::Finished {
                    took: Some(Duration::from_secs(520)),
                },
            );
            for (which, m) in [
                ("working", &mut working_agent),
                ("asking", &mut asking_agent),
                ("done with touched files", &mut done_agent),
            ] {
                for width in 1..=30u16 {
                    let rows = page(m, (width, 24));
                    for row in &rows {
                        assert!(
                            row.chars().count() <= usize::from(width),
                            "{which} at {width}: {row:?}"
                        );
                    }
                }
            }
        }

        #[test]
        fn a_permission_prompt_pins_what_was_said_and_the_whole_call() {
            let mut m = fleet(10);
            let command = "cargo build --release --workspace --all-features && cp target/release/agent ~/.local/bin/agent && echo installed";
            let run = call_at(
                m.clock.wall,
                "p",
                "Bash",
                Target::Command(command.into()),
                CallState::Running,
                120,
            );
            asking(&mut m, "payroll-fix-3", Vec::new(), run);
            let rows = page(&mut m, (70, 26));
            let text = rows.join("\n");
            assert!(text.contains("asking 2m"), "{text}");
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                flat.contains("Before deleting them I want to confirm"),
                "what it said before\n{text}"
            );
            assert!(
                flat.contains("cargo build --release --workspace --all-features && cp target/release/agent ~/.local/bin/agent && echo installed"),
                "the whole command, wrapped\n{text}"
            );
        }

        /// A running Bash call and the main agent's permission ask for it,
        /// beside another call that runs without asking.
        fn parallel(m: &mut Model) -> SessionName {
            let wall = m.clock.wall;
            let run = |id: &str, command: &str| {
                call_at(
                    wall,
                    id,
                    "Bash",
                    Target::Command(command.into()),
                    CallState::Running,
                    30,
                )
            };
            let who = asking(m, "payroll-fix-3", Vec::new(), run("p1", "rm -rf target"));
            let detail = m.track(&who).activity_view.watch.log.take();
            let Some(Ok(Some(mut detail))) = detail else {
                panic!("an activity");
            };
            detail.log.log.insert(0, run("p0", "sleep 600"));
            m.set_activity(&who, Ok(Some(detail)));
            who
        }

        #[test]
        fn with_calls_in_parallel_the_ask_belongs_to_the_one_it_names_by_id() {
            let mut m = fleet(10);
            parallel(&mut m);
            let rows = page(&mut m, (140, 40));
            let text = rows.join("\n");
            assert!(text.contains("rm -rf target"), "pinned\n{text}");
            let waiting: Vec<_> = rows.iter().filter(|r| r.contains("waiting")).collect();
            assert_eq!(waiting.len(), 1, "{text}");
            assert!(waiting[0].contains("rm -rf target"), "{text}");
            let other = rows
                .iter()
                .find(|r| r.contains("sleep 600"))
                .expect("shown");
            assert!(!other.contains('◆'), "{other}");
        }

        #[test]
        fn a_subagents_question_is_drawn_as_theirs_and_leaves_the_agents_calls_alone() {
            let mut m = fleet(10);
            let who = parallel(&mut m);
            let wall = m.clock.wall;
            let activity = m
                .activities
                .as_mut()
                .and_then(|a| a.get_mut(&who))
                .expect("reports");
            activity.asks.clear();
            activity.subagents.insert(
                AgentId::new("s9"),
                agent_core::activity::Subagent {
                    kind: "general-purpose".into(),
                    description: Some("QA pass".into()),
                    launch: agent_core::activity::Launch::Foreground,
                    since: wall,
                    status: agent_core::activity::SubagentStatus::Running { tools: Vec::new() },
                },
            );
            activity.asks.push(Ask {
                asker: Origin::Subagent(AgentId::new("s9")),
                question: "Delete the cache directory?".into(),
                answered_by: Answer::AnyTool,
                since: wall - Duration::from_secs(60),
            });
            let rows = page(&mut m, (140, 40));
            let text = rows.join("\n");
            assert!(
                rows[0].contains("asking 1m") && rows[0].contains("QA pass"),
                "{text}"
            );
            assert!(text.contains("↳ QA pass asks"), "{text}");
            assert!(text.contains("Delete the cache directory?"), "{text}");
            assert_eq!(
                rows.iter().filter(|r| r.contains('◆')).count(),
                1,
                "only the status row, no call is waiting\n{text}"
            );
        }

        #[test]
        fn a_question_call_the_transcript_hasnt_parsed_yet_shows_the_asked_text_not_its_input() {
            let mut m = fleet(10);
            let ask = call_at(
                m.clock.wall,
                "q",
                "AskUserQuestion",
                Target::None,
                CallState::Running,
                20,
            );
            let who = asking(&mut m, "payroll-fix-3", Vec::new(), ask);
            let Some(Ok(Some(detail))) = m.track(&who).activity_view.watch.log.as_mut() else {
                panic!("an activity");
            };
            let LogEntry::Called(call) = detail.log.log.last_mut().expect("a call") else {
                panic!("a call");
            };
            call.input = r#"{"questions":[{"question":"raw"}]}"#.into();
            let text = page(&mut m, (140, 40)).join("\n");
            assert!(!text.contains("{\"questions\""), "{text}");
            assert!(text.contains("Meta-session: drop the grid?"), "{text}");
        }

        #[test]
        fn the_elapsed_time_of_a_working_activity_is_the_agents_not_the_old_prompts() {
            let mut m = fleet(10);
            let who = working(&mut m, "payroll-fix-3");
            let activity = m
                .activities
                .as_mut()
                .and_then(|a| a.get_mut(&who))
                .expect("reports");
            activity.since = m.clock.wall - Duration::from_secs(25);
            let rows = page(&mut m, (140, 40));
            assert!(
                rows[0].contains("working 25s"),
                "after a hand-back the prompt is 7m old\n{}",
                rows[0]
            );
        }

        #[test]
        fn transcript_text_is_cleaned_and_markdown_emphasis_dropped_from_the_closing_message() {
            let mut m = fleet(10);
            let who = at(&mut m, "auth-refresh-2", (140, 40));
            let wall = m.clock.wall;
            let mut detail = activity_at(
                wall,
                vec![said_at(wall, "reading \u{1b}[31mred\u{1b}[0m text", 30)],
                None,
            );
            detail.log.prompt = Some("do it \u{7}now".into());
            m.set_activity(&who, Ok(Some(detail)));
            let text = page(&mut m, (140, 40)).join("\n");
            assert!(
                !text.contains('\u{1b}') && !text.contains('\u{7}'),
                "{text:?}"
            );
            assert!(
                text.contains("a 401 interceptor"),
                "emphasis is stripped\n{text}"
            );
            assert!(!text.contains("**"), "{text}");
        }

        #[test]
        fn a_finished_activity_leads_with_its_closing_message_and_folds_all_its_phases() {
            let mut m = fleet(10);
            let who = at(&mut m, "auth-refresh-2", (140, 40));
            let detail = ActivityDetail {
                progress: Progress::Finished {
                    took: Some(Duration::from_secs(520)),
                },
                ..activity_at(m.clock.wall, story(m.clock.wall), None)
            };
            m.set_activity(&who, Ok(Some(detail)));
            for size in [(140, 40), (70, 26)] {
                let rows = page(&mut m, size);
                let text = rows.join("\n");
                assert!(rows[0].contains("✓ finished"), "{size:?}\n{text}");
                assert!(rows[0].contains("8m40s"), "{size:?}\n{text}");
                assert!(
                    text.contains("14 tests pass."),
                    "the message in full\n{text}"
                );
                let message = rows
                    .iter()
                    .position(|r| r.contains("14 tests pass."))
                    .expect("shown");
                let activity = rows
                    .iter()
                    .position(|r| r.contains("Calls ·"))
                    .unwrap_or_else(|| panic!("{text}"));
                assert!(message < activity, "{text}");
                assert!(
                    !text.contains("guarding it and running it again."),
                    "every phase is a row\n{text}"
                );
                assert!(text.contains("▸ The headless"), "{size:?}\n{text}");
                assert!(!text.contains("following"), "{text}");
            }
        }

        fn touching(m: &mut Model, agent: &str, state: Progress) -> SessionName {
            use agent_core::ports::{Change, Lines, TrackedState};
            let who = at(m, agent, (140, 40));
            let detail = ActivityDetail {
                progress: state,
                ..activity_at(m.clock.wall, story(m.clock.wall), None)
            };
            m.set_activity(&who, Ok(Some(detail)));
            let pending = vec![
                Change::untracked("nvim/lua/heucristic/status.lua"),
                Change::tracked(
                    "nvim/plugin/lsp.lua",
                    Lines::Counted {
                        added: 1,
                        removed: 1,
                    },
                    TrackedState::Unstaged(Edit::Modified),
                ),
            ];
            m.set_test_facts(&who, Ok(crate::fixtures::facts(pending, Vec::new())));
            who
        }

        #[test]
        fn a_finished_activity_lists_the_files_it_touched_with_changes_markers_and_counts_before_its_activity()
         {
            let mut m = fleet(10);
            touching(
                &mut m,
                "auth-refresh-2",
                Progress::Finished {
                    took: Some(Duration::from_secs(520)),
                },
            );
            for size in [(140, 40), (70, 26)] {
                let rows = page(&mut m, size);
                let text = rows.join("\n");
                let at = |needle: &str| {
                    rows.iter()
                        .position(|r| r.contains(needle))
                        .unwrap_or_else(|| panic!("{needle}\n{text}"))
                };
                let (message, heading, activity) =
                    (at("14 tests pass."), at("Touched files"), at("Calls ·"));
                assert!(message < heading && heading < activity, "{size:?}\n{text}");
                let lsp = at("lsp.lua");
                let status = at("/status.lua");
                assert!(heading < status && status < lsp && lsp < activity, "{text}");
                assert!(
                    rows[lsp].contains(" M nvim/plugin/lsp.lua"),
                    "{}",
                    rows[lsp]
                );
                assert!(rows[lsp].contains("+1 −1"), "{}", rows[lsp]);
                assert!(rows[status].contains("?? "), "{}", rows[status]);
                assert!(
                    size.0 < 100 || rows[status].contains("nvim/lua/heucristic/status.lua"),
                    "{}",
                    rows[status]
                );
                assert!(rows[status].trim_end().ends_with("new"), "{}", rows[status]);
                assert!(size.0 < 100 || rows[0].contains("2 files"), "{text}");
            }
        }

        #[test]
        fn a_working_activity_only_counts_its_touched_files_on_the_status_row() {
            let mut m = fleet(10);
            touching(&mut m, "payroll-fix-3", Progress::Running { asked: None });
            let activity = m
                .activities
                .as_mut()
                .and_then(|a| a.get_mut(&name("payroll-fix-3")))
                .expect("reports");
            activity.state = State::Working { tools: Vec::new() };
            let text = page(&mut m, (140, 40)).join("\n");
            assert!(!text.contains("Touched files"), "{text}");
            assert!(text.contains("2 files"), "{text}");
        }

        #[test]
        fn a_touched_file_git_no_longer_differs_in_says_so_and_a_cursor_on_one_is_marked() {
            let mut m = fleet(10);
            let who = touching(&mut m, "auth-refresh-2", Progress::Finished { took: None });
            m.set_test_facts(&who, Ok(crate::fixtures::facts(Vec::new(), Vec::new())));
            m.focus_document();
            render(&mut m, (140, 40));
            m.track(&who).activity_view.watch.nav.cursor = Some(crate::log::EntryKey::Touched(
                "nvim/lua/heucristic/status.lua".into(),
            ));
            let rows = page(&mut m, (140, 40));
            let row = rows
                .iter()
                .find(|r| r.contains("nvim/lua/heucristic/status.lua"))
                .expect("listed");
            assert!(row.contains("no difference"), "{row}");
            assert!(row.starts_with('▌'), "the cursor's gutter\n{row}");
        }

        #[test]
        fn at_180_columns_text_wraps_at_the_panes_width_and_counts_sit_at_its_right_edge() {
            let mut m = fleet(10);
            let who = at(&mut m, "auth-refresh-2", (180, 40));
            let long = "word ".repeat(40).trim_end().to_string();
            m.activities
                .as_mut()
                .and_then(|a| a.get_mut(&who))
                .expect("reports")
                .state = State::Done {
                summary: Some(long.clone()),
            };
            m.set_activity(
                &who,
                Ok(Some(ActivityDetail {
                    progress: Progress::Finished { took: None },
                    ..activity_at(m.clock.wall, story(m.clock.wall), None)
                })),
            );
            let rows = page(&mut m, (180, 40));
            let width = rows[0].chars().count();
            assert!(width > 130, "the pane is wide: {width}");
            let message = rows
                .iter()
                .find(|r| r.contains("word word"))
                .expect("the message");
            let first = message.trim().chars().count();
            assert!(
                (width - 8..width).contains(&first),
                "filled to the pane's width ({width}), not wrapped at 100: {first}\n{message}"
            );
            let phase = rows
                .iter()
                .find(|r| r.contains("+4 −2"))
                .expect("a phase row");
            assert!(
                phase.trim_end().chars().count() >= width - 2,
                "the count is at the right edge\n{phase}"
            );
            let rule = rows
                .iter()
                .find(|r| r.contains("Calls ·"))
                .expect("the rule");
            assert!(rule.trim_end().chars().count() >= width - 2, "{rule}");
        }

        #[test]
        fn an_idle_agent_shows_its_last_activity_as_finished_without_the_check() {
            let mut m = fleet(10);
            let who = at(&mut m, "payroll-fix-3", (140, 40));
            let activity = m
                .activities
                .as_mut()
                .and_then(|a| a.get_mut(&who))
                .expect("reports");
            activity.state = State::Idle;
            let mut detail = activity_at(
                m.clock.wall,
                story(m.clock.wall),
                Some(said_at(m.clock.wall, "All fixed.", 10)),
            );
            detail.log.outcome = Outcome::Finished {
                report: Some("All fixed.".into()),
            };
            m.set_activity(&who, Ok(Some(detail)));
            let rows = page(&mut m, (140, 40));
            assert!(
                rows[0].contains("● finished") && !rows[0].contains('✓'),
                "{}",
                rows.join("\n")
            );
            assert!(rows.iter().any(|r| r.contains("All fixed.")));
        }

        #[test]
        fn a_failed_activity_says_why_and_keeps_its_last_calls_open() {
            let mut m = fleet(10);
            let who = at(&mut m, "rate-limit-1", (140, 40));
            m.set_activity(
                &who,
                Ok(Some(activity_at(m.clock.wall, story(m.clock.wall), None))),
            );
            let rows = page(&mut m, (140, 40));
            assert!(
                rows[0].contains("! failed") && rows[0].contains("rate limit"),
                "{}",
                rows.join("\n")
            );
            assert!(
                rows.iter().any(|r| r.contains("E5113")),
                "the call it died on stays open"
            );
        }

        #[test]
        fn an_activity_not_yet_started_says_so_in_the_middle_and_a_failed_read_says_why() {
            let mut m = fleet(10);
            let who = at(&mut m, "payroll-fix-3", (140, 40));
            let activity = m
                .activities
                .as_mut()
                .and_then(|a| a.get_mut(&who))
                .expect("reports");
            activity.prompt = None;
            activity.state = State::Idle;
            m.set_activity(&who, Ok(None));
            let rows = page(&mut m, (100, 30));
            let at = rows
                .iter()
                .position(|r| r.contains("no activity yet · s send"))
                .unwrap_or_else(|| panic!("{}", rows.join("\n")));
            assert!(at > 5 && at < rows.len() - 5, "in the middle, row {at}");
            assert!(rows[at].starts_with(' '), "centred across");

            m.set_activity(
                &who,
                Err(Cause::message("reading the transcript: denied".into())),
            );
            let rows = page(&mut m, (100, 30));
            assert!(
                rows.iter()
                    .any(|r| r.contains("reading the transcript: denied"))
            );
        }

        #[test]
        fn what_happened_since_the_user_looked_is_marked_once() {
            let mut m = fleet(10);
            let who = working(&mut m, "payroll-fix-3");
            m.track(&who).activity_view.looked.seen = Some(before(m.clock.wall, 200));
            let rows = page(&mut m, (140, 40));
            let text = rows.join("\n");
            let marker = rows
                .iter()
                .position(|r| r.contains("since you looked"))
                .unwrap_or_else(|| panic!("{text}"));
            assert!(rows[marker].contains("3m"), "{}", rows[marker]);
            let handing = rows
                .iter()
                .position(|r| r.contains("Handing QA"))
                .expect("shown");
            let after = rows
                .iter()
                .position(|r| r.contains("guarding it and running it again."))
                .expect("shown");
            assert!(
                handing < marker && marker < after,
                "between what it had done and what it did since\n{text}"
            );
            assert_eq!(text.matches("since you looked").count(), 1);

            m.track(&who).activity_view.looked.seen = Some(before(m.clock.wall, 1));
            let text = page(&mut m, (140, 40)).join("\n");
            assert!(!text.contains("since you looked"), "nothing is new\n{text}");
            m.track(&who).activity_view.looked.seen = None;
            let text = page(&mut m, (140, 40)).join("\n");
            assert!(
                !text.contains("since you looked"),
                "never looked: nothing to mark\n{text}"
            );
        }

        #[test]
        fn an_interrupted_activity_says_so_and_stops_following_its_calls() {
            let mut m = fleet(10);
            let who = working(&mut m, "payroll-fix-3");
            let activity = m
                .activities
                .as_mut()
                .and_then(|a| a.get_mut(&who))
                .expect("reports");
            activity.state = State::Interrupted;
            activity.since = m.clock.wall - Duration::from_secs(95);
            for size in [(140, 40), (70, 26)] {
                let text = page(&mut m, size).join("\n");
                assert!(text.contains("■ interrupted"), "{size:?}\n{text}");
                assert!(!text.contains("working"), "{size:?}\n{text}");
                assert!(
                    !text.contains("running 12s"),
                    "a call it was in the middle of no longer runs\n{text}"
                );
            }
        }

        /// Opens the activity of `agent` in [0] on a screen `size` and says
        /// which entry the cursor is on after `keys` are pressed once [0]
        /// has the keys, with the rows of every entry.
        fn cursor_after(
            m: &mut Model,
            who: &SessionName,
            keys: &[crossterm::event::KeyCode],
        ) -> (
            Option<crate::log::EntryKey>,
            Vec<crate::log::EntryKey>,
            usize,
        ) {
            use crossterm::event::KeyCode;
            press(m, KeyCode::Enter);
            for key in keys {
                press(m, *key);
            }
            page(m, (100, 14));
            let main = m.areas.main.as_ref().expect("a document");
            let entries = main.entries.iter().map(|e| e.key.clone()).collect();
            let cursor = m
                .nav(&crate::model::LogKey::Activity(who.clone()))
                .and_then(|nav| nav.cursor.clone());
            (cursor, entries, main.top(&m.docs))
        }

        #[test]
        fn the_cursor_of_a_log_scrolled_to_the_top_starts_on_its_first_entry_and_moves_one() {
            use crossterm::event::KeyCode;
            let mut m = fleet(10);
            let who = working(&mut m, "payroll-fix-3");
            page(&mut m, (100, 14));
            m.docs.scroll_to_start();
            page(&mut m, (100, 14));
            let (cursor, entries, top) = cursor_after(&mut m, &who, &[]);
            assert_eq!((top, cursor.as_ref()), (0, entries.first()), "on screen");
            assert!(entries.len() > 3, "the log overflows the screen");

            let (cursor, entries, top) = cursor_after(&mut m, &who, &[KeyCode::Char('j')]);
            assert_eq!(cursor.as_ref(), entries.get(1), "one entry down");
            assert_eq!(top, 0, "and the screen stays where it was");
        }

        #[test]
        fn the_cursor_of_a_log_following_its_end_starts_on_the_last_entry_showing_and_moves_up_one()
        {
            use crossterm::event::KeyCode;
            let mut m = fleet(10);
            let who = working(&mut m, "payroll-fix-3");
            page(&mut m, (100, 14));
            let (cursor, entries, top) = cursor_after(&mut m, &who, &[]);
            assert_eq!(cursor.as_ref(), entries.last(), "the end is on screen");
            assert!(top > 0, "it follows the end");

            let (cursor, entries, _) = cursor_after(&mut m, &who, &[KeyCode::Char('k')]);
            assert_eq!(cursor.as_ref(), entries.get(entries.len() - 2));
        }
    }
}
