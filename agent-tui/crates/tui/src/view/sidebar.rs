//! The sidebar, three panels stacked lazygit-style: [1] the fleet as
//! cards, [2] the selected agent's turn, [3] its work in git.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agent_core::activity::Subagent;
use agent_core::model::{Entry, EntryKind, Head, Repo, RepoKey, Snapshot};
use agent_core::names::SessionName;
use agent_core::ports::{DiffStat, Lines, Readiness, WorktreeFacts};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::{
    ACCENT, NAME_MIN, ago, bold, dim, fit, glyph, justify, line_width, name_style, panel, scroll,
    sidebar_count, sidebar_focused, sidebar_title, spinner, truncate_end, truncate_start, wrap,
};
use crate::model::{
    AgentStatus, Cursor, Doing, Extent, Failure, Fold, ListScroll, Model, Panel as Focused, Place,
    Pulse, Row, Target, Turn, WorkRoom,
};

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
const LEAD_LINES: usize = 3;
const PROMPT_LINES: usize = 2;
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

    /// The state in a word, for a state line whose text sits in the lead
    /// below it.
    fn word(&self) -> &str {
        match self {
            Self::Asking(_) => "asking",
            Self::Failed(_) => "failed",
            Self::Tool { name, .. } => name,
            Self::Thinking => "thinking",
            Self::Delegating { .. } => "delegating",
            Self::HandingBack => "handing back",
            Self::Done(_) => "done",
        }
    }

    /// All of what the second line says, for a block to lead with when the
    /// card cut or hid it. Delegating has none, since the subagent tree
    /// says more.
    fn lead(&self) -> Option<String> {
        let text = match self {
            Self::Asking(question) => (*question).to_string(),
            Self::Failed(failure) => failure_text(failure).to_string(),
            Self::Tool {
                name,
                target: Some(target),
            } => format!("{name}  {}", target_text(target)),
            Self::Tool { name, target: None } => (*name).to_string(),
            Self::Thinking => "thinking".to_string(),
            Self::Done(summary) => summary.map_or_else(|| "done".to_string(), plain),
            Self::Delegating { .. } | Self::HandingBack => return None,
        };
        Some(text.split_whitespace().collect::<Vec<_>>().join(" "))
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
            markers.push(Span::styled("⇄", Style::new().fg(Color::Yellow)));
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

    fn in_pane(&self, entry: &Entry) -> bool {
        self.m.pane.session() == Some(entry.name())
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

    /// [2], the turn: what the agent is doing unless its `card` says it
    /// whole, then the exchange the pane has scrolled past: the prompt,
    /// the agent's message or question in full, and its subagents. Items
    /// are in priority order, each placed where it reads in sequence.
    fn detail(&self, repo: &Repo, entry: &Entry, card: Shown) -> Detail {
        let m = self.m;
        let status = m.status(entry);
        let activity = m.activity(entry.name());
        let width = self.content;
        let in_pane = self.in_pane(entry);
        let heard = activity.map_or(m.now, |a| a.heard);
        let gist = Gist::of(&status, heard, m.now);
        // The card above already says when the turn ended, and the quiet
        // time is in the text.
        let time = match gist {
            Some(Gist::Delegating { .. }) => Vec::new(),
            _ => time_in_state(&status, m.now),
        };
        let room = width.saturating_sub(2 + line_width(&time) + 1);
        let (text, shown) = match (&gist, &status) {
            (Some(gist), _) => gist.line(self.form, room),
            (None, AgentStatus::Idle) => ("idle".to_string(), Shown::Whole),
            (None, AgentStatus::Exited) => ("exited".to_string(), Shown::Whole),
            (None, _) => ("no session".to_string(), Shown::Whole),
        };
        let states = card != Shown::Whole;
        let lead = gist
            .as_ref()
            .filter(|_| states && !in_pane && shown == Shown::Cut)
            .and_then(|gist| {
                let style = gist.style();
                gist.lead().map(|text| Item {
                    cap: LEAD_LINES,
                    order: Order::Lead,
                    content: Content::Text {
                        rows: wrap(&text, width),
                        style,
                        width,
                        hang: "",
                    },
                })
            });
        // With the lead carrying the text, the state line says the state
        // rather than opening with a cut copy of the row below it.
        let text = match (&gist, &lead) {
            (Some(gist), Some(_)) => gist.word().to_string(),
            _ => text,
        };
        let style = gist.as_ref().map_or_else(dim, Gist::style);
        let state = justify(
            vec![glyph(&status), Span::raw(" "), Span::styled(text, style)],
            time,
            width,
        );
        let state = states.then(|| Item::fixed(Order::State, vec![state]));
        let tree = activity.filter(|_| m.subagents(entry).is_some()).map(|a| {
            Item::fixed(
                Order::Tree,
                tree(a.subagents.values().collect(), self.form, width, m.now),
            )
        });
        let prompt = activity.and_then(|a| a.prompt.as_deref()).map(|prompt| {
            let room = width.saturating_sub(2);
            Item {
                cap: PROMPT_LINES,
                order: Order::Prompt,
                content: Content::Text {
                    rows: wrap(&format!("› {prompt}"), room),
                    style: dim(),
                    width: room,
                    hang: "  ",
                },
            }
        });
        let right = if in_pane {
            vec![Span::styled("in pane", dim())]
        } else if repo.name() != entry.label() && self.form == Form::Long {
            vec![Span::styled(repo.name().into_owned(), dim())]
        } else {
            Vec::new()
        };
        Detail {
            title: vec![
                Span::raw("[2] "),
                Span::styled(entry.label().to_string(), bold()),
            ],
            right,
            items: [state, lead, tree, prompt].into_iter().flatten().collect(),
        }
    }
}

/// [2]'s title, what its top border says on the right, and its items in
/// priority order.
struct Detail {
    title: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    items: Vec<Item>,
}

/// Only an agent worktree's commits are its own work, so any other
/// checkout shows readiness only while it has uncommitted files. `nothing
/// ahead` is news only once the agent says it's done.
fn shows_readiness(entry: &Entry, status: &AgentStatus, readiness: Readiness) -> bool {
    match (readiness, entry.kind()) {
        (Readiness::NothingAhead, EntryKind::Agent { .. }) => {
            matches!(status, AgentStatus::Done { .. })
        }
        (Readiness::Dirty { .. }, _)
        | (Readiness::Behind { .. } | Readiness::Ready { .. }, EntryKind::Agent { .. }) => true,
        (_, EntryKind::Main { .. } | EntryKind::Adopted { .. } | EntryKind::Orphan { .. }) => false,
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
            Span::styled("?", Style::new().fg(Color::Yellow)),
        ],
        AgentStatus::Asking { since, .. } => vec![Span::styled(
            ago(*since, now),
            Style::new().fg(Color::Yellow),
        )],
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
        .fold((0, 0), |(live, exited), entry| match m.status(entry) {
            AgentStatus::Idle | AgentStatus::Working { .. } => (live + 1, exited),
            AgentStatus::Exited => (live, exited + 1),
            AgentStatus::NoSession
            | AgentStatus::Asking { .. }
            | AgentStatus::Failed { .. }
            | AgentStatus::Done { .. } => (live, exited),
        })
}

/// Who else changed it: the first agent and a count of the rest, so the
/// name is never what gets cut and the count can't read as lines added.
fn others(agents: &[&Entry]) -> String {
    match agents {
        [] => String::new(),
        [only] => only.label().to_string(),
        [first, _] => format!("{}, 1 other", first.label()),
        [first, rest @ ..] => format!("{}, {} others", first.label(), rest.len()),
    }
}

/// `├ ⠹ description  kind  4m`, oldest first, the last child on `└`.
fn tree(
    mut subagents: Vec<&Subagent>,
    form: Form,
    width: usize,
    now: SystemTime,
) -> Vec<Line<'static>> {
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
            let since = ago(subagent.since, now);
            let described = subagent.description.as_deref().filter(|d| !d.is_empty());
            let (label, right) = match (described, form) {
                (Some(description), Form::Long) => {
                    (description, format!(" {}  {since}", subagent.kind))
                }
                (None, Form::Long) => ("", format!(" {}  {since}", subagent.kind)),
                (Some(description), Form::Short) => (description, format!(" {since}")),
                (None, Form::Short) => (subagent.kind.as_str(), format!(" {since}")),
            };
            justify(
                vec![
                    Span::styled(branch, dim()),
                    Span::raw(format!("{} ", spinner())),
                    Span::raw(label.to_string()),
                ],
                vec![Span::styled(right, dim())],
                width,
            )
        })
        .collect()
}

fn readiness(readiness: Readiness) -> Line<'static> {
    let ahead = |n: usize| {
        [
            Span::styled(" · ", dim()),
            Span::styled("↑", dim()),
            Span::raw(n.to_string()),
        ]
    };
    let spans: Vec<Span<'static>> = match readiness {
        Readiness::Dirty { dirty, ahead: 0 } => vec![Span::raw(format!("{dirty} uncommitted"))],
        Readiness::Dirty { dirty, ahead: n } => {
            std::iter::once(Span::raw(format!("{dirty} uncommitted")))
                .chain(ahead(n))
                .collect()
        }
        Readiness::Behind { behind, ahead: n } => {
            std::iter::once(Span::raw(format!("behind {behind}")))
                .chain(ahead(n.get()))
                .collect()
        }
        Readiness::Ready { ahead: n } => vec![
            Span::raw("ready "),
            Span::styled("↑", dim()),
            Span::raw(n.to_string()),
        ],
        Readiness::NothingAhead => vec![Span::raw("nothing ahead")],
    };
    Line::from(spans)
}

/// `+a −r`, leaving out a side that's zero.
fn counts(added: usize, removed: usize) -> (String, String) {
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

/// [3]: a row per changed file in `diff --numstat` order, its counts in
/// columns, in `rows` rows, each after a gutter. A file in `shared` (one
/// another agent changed too) carries a Yellow `∩` before its counts.
/// With a cursor, the rows scroll to keep it in view and its gutter is
/// marked. Without one, the last row says how many more when they don't
/// all fit.
fn changes(
    facts: &WorktreeFacts,
    shared: &HashMap<PathBuf, Vec<&Entry>>,
    width: usize,
    rows: usize,
    cursor: Option<usize>,
) -> Changes {
    let changed = &facts.changed;
    let cursor = cursor.map(|file| file.min(changed.len().saturating_sub(1)));
    let (first, listed) = match (cursor, changed.len()) {
        (Some(file), n) => (file.saturating_sub(rows.saturating_sub(1)), rows.min(n)),
        (None, n) if n <= rows => (0, n),
        (None, _) => (0, rows.saturating_sub(1)),
    };
    let shown = &changed[first..(first + listed).min(changed.len())];
    let green = Style::new().fg(Color::Green);
    let red = Style::new().fg(Color::Red);
    let columns: Vec<(Span<'static>, String)> = shown
        .iter()
        .map(|change| match change.lines {
            Lines::Counted { added, removed } => {
                let (added, removed) = counts(added, removed);
                (Span::styled(added, green), removed)
            }
            Lines::Binary => (Span::styled("bin", dim()), String::new()),
            Lines::Untracked => (Span::styled("new", dim()), String::new()),
        })
        .collect();
    let added_width = columns.iter().map(|(a, _)| a.width()).max().unwrap_or(0);
    let removed_width = columns.iter().map(|(_, r)| r.width()).max().unwrap_or(0);
    let any_shared = changed.iter().any(|c| shared.contains_key(&c.path));
    let mut lines: Vec<Line<'static>> = shown
        .iter()
        .zip(columns)
        .enumerate()
        .map(|(i, (change, (added, removed)))| {
            let mut right = Vec::new();
            if shared.contains_key(&change.path) {
                right.push(Span::styled("∩ ", Style::new().fg(Color::Yellow)));
            } else if any_shared {
                right.push(Span::raw("  "));
            }
            right.push(Span::styled(
                format!("{:>added_width$}", added.content),
                added.style,
            ));
            if removed_width > 0 {
                right.push(Span::styled(format!(" {removed:<removed_width$}"), red));
            }
            let room = width.saturating_sub(line_width(&right) + 1);
            let path = tail(&change.path.to_string_lossy(), room);
            let mut line = justify(vec![Span::raw(path)], right, width);
            let gutter = match cursor {
                Some(file) if file == first + i => Span::styled("▌", Style::new().fg(ACCENT)),
                _ => Span::raw(" "),
            };
            line.spans.insert(0, gutter);
            line
        })
        .collect();
    if cursor.is_none() && listed < changed.len() {
        lines.push(Line::styled(
            format!(" +{} more", changed.len() - listed),
            dim(),
        ));
    }
    Changes {
        lines,
        ..summary(facts, shared, cursor)
    }
}

/// What [3]'s title and corner say, without any rows.
fn summary(
    facts: &WorktreeFacts,
    shared: &HashMap<PathBuf, Vec<&Entry>>,
    cursor: Option<usize>,
) -> Changes {
    let with = cursor
        .and_then(|file| facts.changed.get(file))
        .and_then(|change| shared.get(&change.path))
        .map(|agents| others(agents));
    let widest_with = shared
        .values()
        .map(|agents| others(agents).width())
        .max()
        .unwrap_or(0);
    Changes {
        lines: Vec::new(),
        diff: facts.diff(),
        shared: shared.len(),
        with,
        widest_with,
    }
}

/// [3]'s rows, the totals for its title, how many files are shared, who
/// else changed the file under the cursor, and the widest that text gets
/// for any shared file, so where it goes is decided once per width.
struct Changes {
    lines: Vec<Line<'static>>,
    diff: DiffStat,
    shared: usize,
    with: Option<String>,
    widest_with: usize,
}

/// A detail item: what it shows in as many rows as it's given, how many
/// it takes when rows aren't spare, and where it reads in sequence.
struct Item {
    cap: usize,
    order: Order,
    content: Content,
}

/// Where a [2] item reads, top to bottom: the state, then the exchange
/// in the order it happened, then the subagents the agent is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Order {
    State,
    Prompt,
    Lead,
    Tree,
    /// Why git couldn't be read, only while [3] has no room to say so.
    Work,
}

/// The kinds of detail item, by how they give up rows.
enum Content {
    /// Shown whole or not at all.
    Fixed(Vec<Line<'static>>),
    /// Rows wrapped to `width`, cut short with `…` on the last one kept,
    /// and every row after the first indented by `hang`.
    Text {
        rows: Vec<String>,
        style: Style,
        width: usize,
        hang: &'static str,
    },
}

impl Item {
    /// Rows it takes shown in full.
    fn len(&self) -> usize {
        match &self.content {
            Content::Fixed(lines) => lines.len(),
            Content::Text { rows, .. } => rows.len(),
        }
    }

    fn fixed(order: Order, lines: Vec<Line<'static>>) -> Self {
        Self {
            cap: lines.len(),
            order,
            content: Content::Fixed(lines),
        }
    }

    /// Its tallest form in `rows` rows, empty when none fits.
    fn fit(&self, rows: usize) -> Vec<Line<'static>> {
        match &self.content {
            Content::Fixed(lines) if lines.len() <= rows => lines.clone(),
            Content::Fixed(_) => Vec::new(),
            Content::Text {
                rows: all,
                style,
                width,
                hang,
            } => {
                let mut kept: Vec<String> = all.iter().take(rows).cloned().collect();
                if kept.len() < all.len()
                    && let Some(last) = kept.last_mut()
                {
                    *last = truncate_end(&format!("{last} …"), *width);
                }
                kept.into_iter()
                    .enumerate()
                    .map(|(i, row)| match i {
                        0 => Line::styled(row, *style),
                        _ => Line::styled(format!("{hang}{row}"), *style),
                    })
                    .collect()
            }
        }
    }
}

/// Every item in full, in reading order.
fn fill_all(items: &[Item]) -> Vec<Line<'static>> {
    fill(items, items.iter().map(Item::len).sum())
}

/// Items at their caps in priority order, each shrunk to the rows left
/// rather than skipped, stopping at the first that can't show at all, so a
/// lower item never takes rows from a higher one. Rows still spare then let
/// them grow past their caps. The lines come out in reading order.
fn fill(items: &[Item], rows: usize) -> Vec<Line<'static>> {
    let mut heights = Vec::new();
    let mut left = rows;
    for item in items {
        let height = item.fit(item.cap.min(left)).len();
        if height == 0 {
            break;
        }
        left -= height;
        heights.push(height);
    }
    let mut placed: Vec<(Order, Vec<Line<'static>>)> = Vec::new();
    for (item, height) in items.iter().zip(heights) {
        let lines = item.fit(height + left);
        left -= lines.len().saturating_sub(height);
        placed.push((item.order, lines));
    }
    placed.sort_by_key(|(order, _)| *order);
    placed.into_iter().flat_map(|(_, lines)| lines).collect()
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
            (true, true) => Span::styled("▌", Style::new().fg(ACCENT)),
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
fn list_room(height: u16) -> u16 {
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
    /// The list scroll to remember, from the full window only. A shrunken
    /// window while a lower panel is focused derives its own and never
    /// writes it back, so unfocusing lands where the list was.
    scroll: Option<ListScroll>,
    /// [2], while an agent is selected.
    agent: Option<(Panel, Detail)>,
    /// What [2] has beyond what it shows: rows hidden while it isn't
    /// focused, or its scroll position while it is.
    turn: Turn,
    /// The focused panel, its position clamped to what it shows.
    focus: Focused,
    /// Whether the column had room for [3].
    room: WorkRoom,
    /// [3], while the selected agent has changes and there's room.
    changes: Option<(Panel, Changes)>,
}

/// Stacks [1] sized to its rows (scrolled to keep the selection in view,
/// on the same screen row while it moves between groups when scrolling
/// allows), [2] sized to what it says, and [3] in the rest. Without [3],
/// [2] takes the rest, and without an agent selected, [1] does.
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
    let panel_at = |y: usize, rows: usize| {
        let y = area.y + u16::try_from(y).unwrap_or(u16::MAX);
        Rect::new(
            area.x,
            y,
            area.width,
            u16::try_from(rows).unwrap_or(u16::MAX),
        )
    };

    // What the lower panels need decides how far the list gives way, so
    // it's known before the list is windowed. The room under the list is
    // decided here, once, and carried in the type.
    let lower = selection.and_then(|(repo, entry)| {
        let room = LowerRoom::of(rest)?;
        Some((
            repo,
            entry,
            room,
            needs(&deck, repo, entry, list.card, room),
        ))
    });
    let (list_rows, geometry) = match &lower {
        None => (visible, None),
        Some((_, _, room, needs)) => {
            let geometry = split(
                Column {
                    list: visible,
                    lower: *room,
                },
                needs.shape(*room),
                Focus::of(m.panel, &needs.shape(*room)),
            );
            (geometry.list, Some(geometry))
        }
    };
    let window = list_rows;
    let shrunk = window < visible;
    let offset = if shrunk {
        scroll(offset, selected_line, window, len)
    } else {
        offset
    };

    let mut hits = Vec::new();
    let mut lines = Vec::new();
    for (i, (line, row)) in list.lines.into_iter().enumerate().skip(offset).take(window) {
        let y = area.y + 1 + u16::try_from(i - offset).unwrap_or(0);
        hits.extend(row.map(|c| (y, c)));
        lines.push(line);
    }
    let anchor = cursor
        .cloned()
        .zip(list.selected.map(|r| r.start.saturating_sub(offset)));

    let (agent, changes, focus, turn, work_room) = match (lower, geometry) {
        (Some((repo, entry, room, needs)), Some(geometry)) => {
            let below = Rect::new(
                area.x,
                area.y + u16::try_from(list_rows + 2).unwrap_or(u16::MAX),
                area.width,
                u16::try_from(height - list_rows - 2).unwrap_or(u16::MAX),
            );
            let room = room.work_room();
            let below = lower_panels(&deck, repo, entry, needs, geometry, below);
            (
                Some(below.agent),
                below.changes,
                below.focus,
                below.turn,
                room,
            )
        }
        _ => (None, None, Focused::Agents, Turn::Whole, WorkRoom::TooShort),
    };
    // With no agent selected the list takes the whole column.
    let list_height = match geometry {
        Some(_) => list_rows + 2,
        None => height,
    };
    Some(Laid {
        list: Panel {
            area: panel_at(0, list_height),
            lines,
        },
        hits,
        // A list windowed for a focused panel derives its scroll and
        // never writes it back, so unfocusing lands where the list was.
        scroll: (!shrunk).then_some(ListScroll { offset, anchor }),
        agent,
        turn,
        focus,
        room: work_room,
        changes,
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

/// `+k more`, DIM, as [2]'s last row. The key that shows the rest is the
/// footer's to offer.
fn more_row(hidden: NonZeroUsize, width: usize) -> Line<'static> {
    Line::styled(truncate_end(&format!("+{hidden} more"), width), dim())
}

/// What the selected agent's lower panels have to show, worked out once
/// so the split can be decided before anything is windowed.
struct Needs<'m> {
    detail: Detail,
    work: Option<Work<'m>>,
}

impl Needs<'_> {
    fn shape(&self, room: LowerRoom) -> Shape {
        let turn = fill_all(&self.detail.items).len();
        let work = match (&self.work, room) {
            (Some(work), LowerRoom::Both { inside }) => {
                let (files, need) = match work {
                    Work::Facts { facts, head } => (
                        facts.changed.len(),
                        usize::from(head.is_some()) + facts.changed.len(),
                    ),
                    Work::Unreadable(_) => (0, 1),
                };
                Some(WorkShape::of(files, need, inside))
            }
            (None, _) | (Some(_), LowerRoom::TurnOnly { .. }) => None,
        };
        Shape { turn, work }
    }
}

/// Rows [2] and [3] would take to show everything, and the least [3]
/// keeps while it exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shape {
    turn: usize,
    work: Option<WorkShape>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkShape {
    /// Files the cursor can move over.
    files: usize,
    need: usize,
    /// As a sibling, [3] keeps the lesser of its content and the floor,
    /// so the floor never holds blank rows and [3] never loses files.
    /// Never more than `need`, and never the whole room.
    min: usize,
}

impl WorkShape {
    fn of(files: usize, need: usize, inside: NonZeroUsize) -> Self {
        Self {
            files,
            need,
            min: need.min(CHANGES_MIN + READINESS_ROW).min(inside.get() - 1),
        }
    }
}

/// The rows under the list: enough for a [2] (at least 3), and with
/// [`WORK_LEAST`] enough for a [3] beside it. `inside` is what's left for
/// content once the borders are taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LowerRoom {
    TurnOnly { inside: NonZeroUsize },
    Both { inside: NonZeroUsize },
}

/// Rows under the list that fit [3] with its floor and readiness row,
/// [2] with one row, and both borders.
const WORK_LEAST: usize = 2 + 1 + CHANGES_MIN + READINESS_ROW + 2;

impl LowerRoom {
    /// None when `rows` can't hold a panel at all.
    fn of(rows: usize) -> Option<Self> {
        if rows >= WORK_LEAST {
            NonZeroUsize::new(rows - 4).map(|inside| Self::Both { inside })
        } else {
            NonZeroUsize::new(rows.checked_sub(2)?).map(|inside| Self::TurnOnly { inside })
        }
    }

    fn work_room(self) -> WorkRoom {
        match self {
            Self::Both { .. } => WorkRoom::Fits,
            Self::TurnOnly { .. } => WorkRoom::TooShort,
        }
    }
}

/// The list's unfocused rows inside its border and the room under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Column {
    list: usize,
    lower: LowerRoom,
}

/// Which lower panel has the keys, if any, and where its cursor is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Neither,
    Turn { offset: usize },
    Work { file: usize },
}

impl Focus {
    /// [3] can only be focused while it has files to move through.
    fn of(panel: Focused, shape: &Shape) -> Self {
        match (panel, shape.work) {
            (Focused::Agent { offset }, _) => Self::Turn { offset },
            (Focused::Changes { file }, Some(work)) if work.files > 0 => Self::Work {
                file: file.min(work.files - 1),
            },
            (Focused::Changes { .. } | Focused::Agents, _) => Self::Neither,
        }
    }
}

/// A panel beside a focused one, or an unfocused [2]: what it has inside
/// its border, or that it's down to its title line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sibling {
    Inside(usize),
    Collapsed,
}

impl Sibling {
    fn of(rows: usize) -> Self {
        match rows {
            0 => Self::Collapsed,
            rows => Self::Inside(rows),
        }
    }

    fn height(self) -> usize {
        match self {
            Self::Inside(rows) => rows + 2,
            Self::Collapsed => 1,
        }
    }

    fn inside(self) -> usize {
        match self {
            Self::Inside(rows) => rows,
            Self::Collapsed => 0,
        }
    }
}

/// How [2] and [3] share the rows under the list, in rows inside their
/// borders, and which has the keys. The pairing is in the type: a
/// collapsed [3] only exists beside a focused [2], and the cursor travels
/// with the focus so nothing downstream reads it from the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lower {
    /// Neither focused. An empty [2] is down to its title line.
    Stacked { turn: Sibling, work: Option<usize> },
    /// [2] focused, scrolled to `offset`.
    TurnFocused {
        offset: usize,
        turn: usize,
        work: Option<Sibling>,
    },
    /// [3] focused, the cursor on `file`.
    WorkFocused {
        file: usize,
        turn: Sibling,
        work: usize,
    },
}

impl Lower {
    /// [2]'s height with its border.
    fn turn_height(self) -> usize {
        match self {
            Self::Stacked { turn, .. } | Self::WorkFocused { turn, .. } => turn.height(),
            Self::TurnFocused { turn, .. } => turn + 2,
        }
    }

    /// [3]'s height with its border, when there is one.
    #[cfg(test)]
    fn work_height(self) -> Option<usize> {
        match self {
            Self::Stacked { work, .. } => work.map(|rows| rows + 2),
            Self::TurnFocused { work, .. } => work.map(Sibling::height),
            Self::WorkFocused { work, .. } => Some(work + 2),
        }
    }
}

/// The list's rows inside its border and the lower panels' split. A pure
/// function of content, selection and focus, so nothing is remembered
/// between frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    list: usize,
    lower: Lower,
}

/// Unfocused: the list keeps its rows, [2] shows all of itself up to a
/// third of what's left (at least [`DETAIL_MIN`] where there's room),
/// growing past that only into rows [3] has no files for, and [3] keeps
/// the lesser of its content and its floor. Focusing a panel changes
/// nothing while its content fits. When it doesn't, the panel takes
/// exactly the deficit: first the sibling's rows above its minimum, then
/// the list's above [`LIST_MIN`], then the sibling collapses to its title
/// line, rows that frees beyond the deficit going back to the list. With
/// the list already at its minimum the surplus stays with the focused
/// panel. Past that the panel scrolls.
fn split(column: Column, shape: Shape, focus: Focus) -> Geometry {
    let (inside, work) = match (column.lower, shape.work) {
        (LowerRoom::Both { inside }, Some(work)) => (inside.get(), work),
        // No [3] to draw, so its border rows are [2]'s too. Focus on a [3]
        // there is nothing, so it reads as no focus.
        (LowerRoom::Both { inside }, None) | (LowerRoom::TurnOnly { inside }, _) => {
            let inside = match column.lower {
                LowerRoom::Both { .. } => inside.get() + 2,
                LowerRoom::TurnOnly { .. } => inside.get(),
            };
            let (list, turn) = match focus {
                Focus::Turn { .. } if shape.turn > inside => {
                    let from_list = (shape.turn - inside)
                        .min(column.list.saturating_sub(usize::from(LIST_MIN)));
                    (column.list - from_list, inside + from_list)
                }
                Focus::Turn { .. } | Focus::Work { .. } | Focus::Neither => (column.list, inside),
            };
            let lower = match focus {
                Focus::Turn { offset } => Lower::TurnFocused {
                    offset,
                    turn,
                    work: None,
                },
                // Alone, [2] keeps the column even with nothing to say.
                Focus::Work { .. } | Focus::Neither => Lower::Stacked {
                    turn: Sibling::Inside(turn),
                    work: None,
                },
            };
            return Geometry { list, lower };
        }
    };
    let share = inside.div_ceil(3).max(DETAIL_MIN).min(inside - work.min);
    // Rows [3] has no files for: none when it has more than fit.
    let spare = inside.saturating_sub(work.need);
    let turn = shape.turn.min(share.max(spare));
    let took = match focus {
        Focus::Neither => {
            // An empty [2] is its title line, and [3] gets the border
            // row that frees.
            let turn = Sibling::of(turn);
            return Geometry {
                list: column.list,
                lower: Lower::Stacked {
                    turn,
                    work: Some(inside - turn.inside() + usize::from(turn == Sibling::Collapsed)),
                },
            };
        }
        Focus::Turn { offset } => Took::Turn { offset },
        Focus::Work { file } => Took::Work { file },
    };
    // [3] focused beside an empty [2]: the title line stays and only the
    // list can give rows, so nothing moves while the files fit.
    if let (Took::Work { file }, Sibling::Collapsed) = (took, Sibling::of(turn)) {
        let have = inside + 1;
        let from_list = work
            .need
            .saturating_sub(have)
            .min(column.list.saturating_sub(usize::from(LIST_MIN)));
        return Geometry {
            list: column.list - from_list,
            lower: Lower::WorkFocused {
                file,
                turn: Sibling::Collapsed,
                work: have + from_list,
            },
        };
    }
    let (need, have, sibling_min) = match took {
        Took::Turn { .. } => (shape.turn, turn, work.min),
        Took::Work { .. } => (work.need, inside - turn, DETAIL_MIN.min(turn)),
    };
    let sibling = inside - have;
    let (list, focused, sibling) = if need <= have {
        (column.list, have, Sibling::Inside(sibling))
    } else {
        let mut deficit = need - have;
        let from_sibling = deficit.min(sibling - sibling_min);
        deficit -= from_sibling;
        let from_list = deficit.min(column.list.saturating_sub(usize::from(LIST_MIN)));
        deficit -= from_list;
        let mut list = column.list - from_list;
        let mut focused = have + from_sibling + from_list;
        let left = sibling - from_sibling;
        let sibling = if deficit > 0 {
            // The sibling's rows and one of its border rows come free.
            focused += left + 1;
            let back = focused.saturating_sub(need).min(column.list - list);
            list += back;
            focused -= back;
            Sibling::Collapsed
        } else {
            Sibling::Inside(left)
        };
        (list, focused, sibling)
    };
    let lower = match took {
        Took::Turn { offset } => Lower::TurnFocused {
            offset,
            turn: focused,
            work: Some(sibling),
        },
        Took::Work { file } => Lower::WorkFocused {
            file,
            turn: sibling,
            work: focused,
        },
    };
    Geometry { list, lower }
}

/// A focused lower panel, once "neither" has been answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Took {
    Turn { offset: usize },
    Work { file: usize },
}

/// What the selected agent's panels have to say, given the room under the
/// list. Too short for [3], the one thing worth knowing about the work
/// goes in [2] instead.
fn needs<'m>(
    deck: &Deck<'m>,
    repo: &Repo,
    entry: &Entry,
    card: Shown,
    room: LowerRoom,
) -> Needs<'m> {
    let m = deck.m;
    let mut detail = deck.detail(repo, entry, card);
    let status = m.status(entry);
    let work = match m.facts.get(entry.name()) {
        Some(Ok(facts)) => {
            let head = shows_readiness(entry, &status, facts.readiness())
                .then(|| worktree_row(readiness(facts.readiness()), deck.content));
            (!facts.changed.is_empty() || head.is_some()).then_some(Work::Facts { facts, head })
        }
        Some(Err(e)) => Some(Work::Unreadable(format!("git: {}", first_line(e).0))),
        None => None,
    };
    let work_room = room.work_room();
    if let (WorkRoom::TooShort, Some(Work::Unreadable(text))) = (work_room, &work) {
        let line = Line::styled(truncate_end(text, deck.content), dim());
        detail.items.push(Item::fixed(Order::Work, vec![line]));
    }
    Needs {
        detail,
        work: work.filter(|_| work_room == WorkRoom::Fits),
    }
}

/// [2] and [3] drawn into `area` as `geometry` shares it. Everything
/// about focus and cursors comes from the geometry. Unfocused, [2]'s last
/// row says what it's hiding. Focused, it scrolls.
fn lower_panels(
    deck: &Deck<'_>,
    repo: &Repo,
    entry: &Entry,
    needs: Needs<'_>,
    geometry: Geometry,
    area: Rect,
) -> Below {
    let at = |y: usize, height: usize| {
        Rect::new(
            area.x,
            area.y + u16::try_from(y).unwrap_or(u16::MAX),
            area.width,
            u16::try_from(height).unwrap_or(u16::MAX),
        )
    };
    let Needs { detail, work, .. } = needs;
    let all = fill_all(&detail.items);
    let unfocused = |inside: usize, focus: Focused| {
        let whole = fill(&detail.items, inside);
        match NonZeroUsize::new(all.len().saturating_sub(whole.len())) {
            None => (whole, focus, Turn::Whole),
            // The count takes the last row, so items keep their priority
            // and a fixed item is never shown in part. When the count
            // would cost more than its own row (a fixed item that fit no
            // longer does), the whole items show without one.
            Some(beyond) => {
                let mut shown = fill(&detail.items, inside.saturating_sub(1));
                if whole.len() > shown.len() + 1 {
                    (whole, focus, Turn::Hidden(beyond))
                } else {
                    let hidden = beyond.saturating_add(whole.len().saturating_sub(shown.len()));
                    shown.push(more_row(hidden, deck.content));
                    (shown, focus, Turn::Hidden(hidden))
                }
            }
        }
    };
    let (filled, focus, turn, file) = match geometry.lower {
        Lower::TurnFocused {
            offset,
            turn: inside,
            ..
        } => {
            let offset = offset.min(all.len().saturating_sub(inside));
            let shown: Vec<Line<'static>> = all.iter().skip(offset).take(inside).cloned().collect();
            let turn = if all.len() <= inside {
                Turn::Whole
            } else {
                Turn::Scrolled {
                    top: offset + 1,
                    bottom: offset + shown.len(),
                    total: all.len(),
                }
            };
            (shown, Focused::Agent { offset }, turn, None)
        }
        // Collapsed to its title line, so everything it had is hidden.
        Lower::WorkFocused {
            file,
            turn: Sibling::Collapsed,
            ..
        } => (
            Vec::new(),
            Focused::Changes { file },
            NonZeroUsize::new(all.len()).map_or(Turn::Whole, Turn::Hidden),
            Some(file),
        ),
        Lower::WorkFocused {
            file,
            turn: Sibling::Inside(inside),
            ..
        } => {
            let (filled, focus, turn) = unfocused(inside, Focused::Changes { file });
            (filled, focus, turn, Some(file))
        }
        Lower::Stacked { turn, .. } => {
            let (filled, focus, turn) = unfocused(turn.inside(), Focused::Agents);
            (filled, focus, turn, None)
        }
    };
    let turn_height = geometry.lower.turn_height();
    let work_rows = match geometry.lower {
        Lower::Stacked { work, .. } => work.map(Sibling::Inside),
        Lower::TurnFocused { work, .. } => work,
        Lower::WorkFocused { work, .. } => Some(Sibling::Inside(work)),
    };
    let changes = work.zip(work_rows).map(|(work, rows)| {
        let area = at(turn_height, rows.height());
        match (work, rows) {
            (Work::Facts { facts, head }, Sibling::Inside(inside)) => {
                let shared = deck.overlaps(repo, entry);
                let head_rows = usize::from(head.is_some());
                let file_rows = inside.saturating_sub(head_rows);
                let changes = changes(facts, &shared, deck.content, file_rows, file);
                let lines = head.into_iter().chain(changes.lines.clone()).collect();
                (Panel { area, lines }, changes)
            }
            // Only beside a focused [2], so no cursor to name.
            (Work::Facts { facts, .. }, Sibling::Collapsed) => {
                let shared = deck.overlaps(repo, entry);
                (
                    Panel {
                        area,
                        lines: Vec::new(),
                    },
                    summary(facts, &shared, None),
                )
            }
            (Work::Unreadable(text), rows) => (
                Panel {
                    area,
                    lines: match rows {
                        Sibling::Collapsed => Vec::new(),
                        Sibling::Inside(_) => vec![worktree_row(Line::raw(text), deck.content)],
                    },
                },
                Changes {
                    lines: Vec::new(),
                    diff: DiffStat::default(),
                    shared: 0,
                    with: None,
                    widest_with: 0,
                },
            ),
        }
    });
    Below {
        agent: (pad(at(0, turn_height), filled), detail),
        changes,
        focus,
        turn,
    }
}

/// What [3] has to show: the files and readiness, or why git couldn't be
/// read.
enum Work<'a> {
    Facts {
        facts: &'a WorktreeFacts,
        head: Option<Line<'static>>,
    },
    Unreadable(String),
}

/// [2] and [3], if there's room for it, with the focus clamped to them.
struct Below {
    agent: (Panel, Detail),
    changes: Option<(Panel, Changes)>,
    focus: Focused,
    turn: Turn,
}

/// A panel holding `lines`, each a column off the left border.
fn pad(area: Rect, lines: Vec<Line<'static>>) -> Panel {
    let lines = lines
        .into_iter()
        .map(|mut line| {
            line.spans.insert(0, Span::raw(" "));
            line
        })
        .collect();
    Panel { area, lines }
}

/// The three panels in `area`, the one `j`/`k` act on with `border` (the
/// accent while the sidebar has the keys) and the others DIM.
pub(super) fn draw(frame: &mut Frame<'_>, m: &mut Model, area: Rect, border: Style) {
    let nav = sidebar_focused(m);
    let style_of = |focused: bool| if focused { border } else { dim() };
    let agents = matches!(m.panel, Focused::Agents);
    let agent = matches!(m.panel, Focused::Agent { .. });
    let files = matches!(m.panel, Focused::Changes { .. });
    // Overview's folds and the tier depend on the list's room, so it's
    // measured before anything asks for the rows.
    m.areas.sidebar = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        list_room(area.height),
    );
    let title = sidebar_title(m, usize::from(area.width).saturating_sub(4));
    let corner = sidebar_count(m, style_of(agents));
    let Some(laid) = lay_out(m, area) else {
        m.turn = Turn::Whole;
        m.work_room = WorkRoom::TooShort;
        frame.render_widget(panel(title, corner, style_of(agents), nav && agents), area);
        return;
    };
    // A focused panel that stopped existing hands the focus back, and the
    // list was windowed for it, so the layout is redone for what shows.
    let laid = if std::mem::discriminant(&laid.focus) == std::mem::discriminant(&m.panel) {
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
            .map_or(0, |(p, _)| usize::from(p.area.height)),
        Focused::Changes { .. } => laid
            .changes
            .as_ref()
            .map_or(0, |(p, _)| usize::from(p.area.height)),
    }
    .saturating_sub(2);
    let list = panel(title, corner, style_of(agents), nav && agents);
    frame.render_widget(Paragraph::new(laid.list.lines).block(list), laid.list.area);
    if let Some((Panel { area, lines }, detail)) = laid.agent {
        let corner = match laid.turn {
            Turn::Scrolled { top, bottom, total } => vec![Span::styled(
                format!("{top}-{bottom} of {total}"),
                style_of(agent),
            )],
            Turn::Whole | Turn::Hidden(_) => Vec::new(),
        };
        let wide = line_width(&detail.right);
        let right = if fits_beside(&detail.title, wide, usize::from(area.width)) {
            detail.right
        } else {
            Vec::new()
        };
        let block = panel(detail.title, corner, style_of(agent), nav && agent)
            .title_top(Line::from(pad_title(right)).right_aligned());
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }
    if let Some((Panel { area, lines }, changes)) = laid.changes {
        let (added, removed) = counts(changes.diff.added, changes.diff.removed);
        let mut title = vec![Span::raw("[3] "), Span::styled("Changes", bold())];
        for (count, colour) in [(added, Color::Green), (removed, Color::Red)] {
            if !count.is_empty() {
                title.push(Span::raw(" "));
                title.push(Span::styled(count, Style::new().fg(colour)));
            }
        }
        if changes.shared > 0 {
            title.push(Span::raw("  "));
            title.push(Span::styled(
                format!("∩ {}", changes.shared),
                Style::new().fg(Color::Yellow),
            ));
        }
        let with = changes.with.map_or_else(Vec::new, |with| {
            vec![
                Span::styled("∩ ", Style::new().fg(Color::Yellow)),
                Span::styled(with, dim()),
            ]
        });
        // Who else sits beside the title when the widest it can get fits
        // there whole, else in the corner, which then never shows a
        // position: one kind of thing per corner, decided by the width
        // rather than the cursor, so moving the cursor doesn't flip it.
        let widest = "∩ ".width() + changes.widest_with;
        let beside_title = fits_beside(&title, widest, usize::from(area.width));
        let (top, corner) = match (laid.focus, beside_title) {
            (Focused::Changes { .. }, false) => (
                Vec::new(),
                fit(with, usize::from(area.width).saturating_sub(4)),
            ),
            (Focused::Changes { file }, true) => (
                with,
                vec![Span::styled(
                    format!("{} of {}", file + 1, changes.diff.files),
                    style_of(files),
                )],
            ),
            (Focused::Agents | Focused::Agent { .. }, _) => (Vec::new(), Vec::new()),
        };
        let title = fit(title, usize::from(area.width).saturating_sub(4));
        let block = panel(title, corner, style_of(files), nav && files)
            .title_top(Line::from(pad_title(top)).right_aligned());
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }
    m.panel = laid.focus;
    m.turn = laid.turn;
    m.work_room = laid.room;
    if let Some(scroll) = laid.scroll {
        m.scroll = scroll;
    }
    m.areas.rows = laid.hits;
}

/// Whether text `wide` columns wide fits beside `title` in a border
/// `width` wide: two corners, both titles' padding and one `─` between
/// them, so neither overwrites the other and they never touch.
fn fits_beside(title: &[Span<'static>], wide: usize, width: usize) -> bool {
    line_width(title) + 7 + wide <= width
}

/// `spans` with a space each side, as the border titles have.
fn pad_title(mut spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    if !spans.is_empty() {
        spans.insert(0, Span::raw(" "));
        spans.push(Span::raw(" "));
    }
    spans
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use agent_core::activity::State;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Modifier;

    use super::*;
    use crate::fixtures::{fleet, name, reported};
    use crate::model::{Group, Pane, View};

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
            .map(|(panel, _)| texts(&panel.lines))
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
        assert_eq!(delegating.lead(), None, "the tree says more");
    }

    #[test]
    fn a_finished_turn_reads_its_first_line_and_leads_with_all_of_it() {
        let done = Gist::Done(Some("## **Done**\n\nAdded `retry`.\nTests pass."));
        assert_eq!(done.line(Form::Long, 40), ("Done".to_string(), Shown::Cut));
        assert_eq!(
            done.lead().as_deref(),
            Some("Done Added retry. Tests pass.")
        );
        assert_eq!(
            Gist::Failed(&Failure::RateLimit).line(Form::Short, 24),
            ("rate limited".to_string(), Shown::Whole)
        );
    }

    #[test]
    fn readiness_reads_as_counts() {
        let read = |r| text(&readiness(r));
        assert_eq!(
            read(Readiness::Dirty {
                dirty: n(2),
                ahead: 0
            }),
            "2 uncommitted"
        );
        assert_eq!(
            read(Readiness::Dirty {
                dirty: n(2),
                ahead: 2
            }),
            "2 uncommitted · ↑2"
        );
        assert_eq!(
            read(Readiness::Behind {
                behind: n(2),
                ahead: n(3)
            }),
            "behind 2 · ↑3"
        );
        assert_eq!(read(Readiness::Ready { ahead: n(1) }), "ready ↑1");
        assert_eq!(read(Readiness::NothingAhead), "nothing ahead");
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
        };
        WorktreeFacts {
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
        }
    }

    #[test]
    fn changes_line_up_their_counts_and_say_how_many_more() {
        let facts = facts();
        let none = HashMap::new();
        assert_eq!(
            texts(&changes(&facts, &none, 34, 4, None).lines),
            [
                " src/payroll/calc.ts        +98 −52",
                " src/payroll/calc.test.ts   +24    ",
                " logo.png                   bin    ",
                " notes.md                   new    ",
            ]
        );
        let short = changes(&facts, &none, 34, 3, None);
        assert_eq!(texts(&short.lines)[2], " +2 more");
        let cursor = changes(&facts, &none, 34, 2, Some(3));
        assert_eq!(
            texts(&cursor.lines),
            [
                " logo.png                       bin",
                "▌notes.md                       new",
            ],
            "the rows scroll to keep the cursor in view"
        );
    }

    /// Text that shrinks a row at a time, `cap` rows of `full`.
    fn item(cap: usize, full: usize) -> Item {
        Item {
            cap,
            order: Order::Lead,
            content: Content::Text {
                rows: (0..full).map(|i| format!("row {i}")).collect(),
                style: Style::new(),
                width: 20,
                hang: "",
            },
        }
    }

    #[test]
    fn items_shrink_in_priority_order_then_grow_into_spare_rows() {
        let items = [item(3, 3), item(4, 4), item(1, 1)];
        assert_eq!(
            texts(&fill(&items, 6)),
            ["row 0", "row 1", "row 2", "row 0", "row 1", "row 2 …"],
            "the second item shrinks and the third gets nothing"
        );
        assert_eq!(
            fill(&[item(2, 6), item(1, 1)], 9).len(),
            7,
            "spare rows grow past caps"
        );
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
                        Focused::Agents => None,
                        Focused::Agent { .. } => Some(1),
                        Focused::Changes { .. } => Some(2),
                    };
                    let mut panels = vec![&laid.list];
                    panels.extend(laid.agent.as_ref().map(|(p, _)| p));
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
                m.toggle_view();
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
            "[1] heucris… · Agents · Pinned"
        );
        assert_eq!(
            text(&super::super::sidebar_title(&m, 40)),
            "[1] heucristic0 · Agents · Pinned"
        );
        m.toggle_view();
        assert_eq!(text(&super::super::sidebar_title(&m, 29)), "[1] Agents");
        m.toggle_view();
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
        m.toggle_view();
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
        m.toggle_view();
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
        m.toggle_view();
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
            tree[0].trim_end().ends_with("general-purpose  50s"),
            "a subagent with no description keeps the kind column\n{}",
            tree[0]
        );
    }

    #[test]
    fn the_agent_in_the_pane_says_so() {
        let mut m = fleet(8);
        let agent = name("obs-layout-4");
        let ticket = m.ticket();
        m.pane = Pane::Connecting {
            name: agent.clone(),
            ticket,
        };
        m.set_cursor(Cursor::Entry(agent));
        let laid = laid(&mut m, (200, 50));
        let (_, detail) = laid.agent.expect("succeeds");
        assert_eq!(texts(&[Line::from(detail.right)]), ["in pane"]);
    }

    #[test]
    fn shared_files_are_marked_in_changes_and_named_under_the_cursor() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("rate-limit-1")));
        let first = laid(&mut m, (200, 50));
        let (panel, changes) = first.changes.as_ref().expect("[3] shows");
        assert_eq!(changes.shared, 1);
        assert_eq!(changes.with, None, "nothing under the cursor unfocused");
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

        m.panel = Focused::Changes { file: 0 };
        let first = laid(&mut m, (200, 50));
        let (_, changes) = first.changes.as_ref().expect("[3] shows");
        assert_eq!(changes.with.as_deref(), Some("auth-refresh-2, 2 others"));
    }

    #[test]
    fn readiness_opens_changes_and_shows_without_files() {
        let mut m = fleet(8);
        m.toggle_view();
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
        m.facts.insert(
            done.clone(),
            Ok(WorktreeFacts {
                ahead: 0,
                ..WorktreeFacts::default()
            }),
        );
        let first = laid(&mut m, (200, 50));
        let (panel, changes) = first.changes.as_ref().expect("[3] shows");
        assert_eq!(changes.diff.files, 0);
        assert_eq!(texts(&panel.lines), [" nothing ahead"]);
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
    fn the_turn_reads_prompt_then_message_then_subagents_and_says_whats_hidden() {
        let mut m = fleet(8);
        m.toggle_view();
        m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
        let first = laid(&mut m, (200, 50));
        let lines = agent_lines(&first);
        let prompt = lines
            .iter()
            .position(|l| l.starts_with(" › go"))
            .expect("the prompt");
        let message = lines
            .iter()
            .position(|l| l.starts_with(" Added token refresh"))
            .expect("the message");
        assert!(prompt < message, "{}", lines.join("\n"));
        assert_eq!(first.turn, Turn::Whole);

        assert!(
            lines[0].trim_start().starts_with("✓ done"),
            "with the message below, the state line says the state\n{}",
            lines.join("\n")
        );

        let short = laid(&mut m, (80, 24));
        let lines = agent_lines(&short);
        let Turn::Hidden(hidden) = short.turn else {
            panic!("{:?}", short.turn)
        };
        assert_eq!(
            lines.last().expect("rows").trim(),
            format!("+{hidden} more")
        );
        let all = {
            let area = m.areas.sidebar;
            let deck = Deck::new(
                &m,
                m.snapshot().expect("loaded"),
                usize::from(area.width),
                usize::from(area.height),
            );
            let (repo, entry) = m.selected().expect("an agent is selected");
            fill_all(
                &deck
                    .detail(repo, entry.expect("an agent, not a repo"), Shown::Cut)
                    .items,
            )
            .len()
        };
        assert_eq!(
            lines.len() - 1 + hidden.get(),
            all,
            "the count is exact\n{}",
            lines.join("\n")
        );

        m.panel = Focused::Agents;
        let unfocused = laid(&mut m, (80, 24));
        let list_height = usize::from(unfocused.list.area.height);
        let turn_height = usize::from(unfocused.agent.as_ref().expect("[2] shows").0.area.height);
        let work_height = unfocused.changes.as_ref().expect("[3] shows").0.area.height;
        m.panel = Focused::Agent { offset: 1 };
        let expanded = laid(&mut m, (80, 24));
        assert_eq!(expanded.turn, Turn::Whole, "focused, [2] shows all of it");
        assert_eq!(agent_lines(&expanded).len(), all);
        assert_eq!(
            usize::from(expanded.list.area.height),
            list_height - (all - (turn_height - 2)),
            "the list gives only the deficit, [3] having nothing above its content"
        );
        let (changes, _) = expanded.changes.as_ref().expect("[3] shows");
        assert_eq!(changes.area.height, work_height, "[3] keeps what it has");

        m.panel = Focused::Agent { offset: 1 };
        let scrolled = laid(&mut m, (80, 16));
        let Turn::Scrolled { top, bottom, total } = scrolled.turn else {
            panic!("{:?}", scrolled.turn)
        };
        assert_eq!(
            (top, bottom, total),
            (2, 1 + agent_lines(&scrolled).len(), all)
        );
        assert!(
            !agent_lines(&scrolled).iter().any(|l| l.contains("more")),
            "focused, nothing is hidden"
        );

        m.panel = Focused::Changes { file: 0 };
        let files = laid(&mut m, (80, 24));
        assert_eq!(
            usize::from(files.agent.as_ref().expect("[2] shows").0.area.height),
            turn_height,
            "two files already fit, so focusing [3] moves nothing"
        );
        let (changes, _) = files.changes.as_ref().expect("[3] shows");
        assert_eq!(changes.area.height, work_height);
    }

    #[test]
    fn a_hidden_count_never_costs_a_fixed_item_its_row() {
        let mut m = fleet(20);
        m.toggle_view();
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let laid = laid(&mut m, (80, 24));
        let (panel, _) = &laid.agent.as_ref().expect("[2] shows");
        let lines = texts(&panel.lines);
        assert_eq!(
            panel.lines.len() + 2,
            usize::from(panel.area.height),
            "[2] is sized to what it shows\n{}",
            lines.join("\n")
        );
        let Turn::Hidden(hidden) = laid.turn else {
            panic!("{:?}\n{}", laid.turn, lines.join("\n"))
        };
        assert_eq!(
            hidden.get(),
            1,
            "only the prompt is hidden: the tree that fit stays\n{}",
            lines.join("\n")
        );
        assert_eq!(lines.len(), 4, "{}", lines.join("\n"));
        assert!(lines[0].contains("↳3"));
        assert!(lines[3].trim_start().starts_with('└'));
        assert!(
            !lines.iter().any(|l| l.contains("more")),
            "a count row that costs the tree isn't worth it"
        );
    }

    #[test]
    fn focusing_a_lower_panel_never_moves_the_list() {
        let mut m = fleet(20);
        m.toggle_view();
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
    fn a_focused_changes_panel_that_collapses_the_turn_says_so() {
        let mut m = fleet(20);
        m.toggle_view();
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::Changes { file: 0 };
        let laid = laid(&mut m, (80, 16));
        assert_eq!(laid.agent.as_ref().expect("[2] shows").0.area.height, 1);
        assert!(matches!(laid.turn, Turn::Hidden(_)), "{:?}", laid.turn);
    }

    #[test]
    fn the_readiness_row_never_eats_the_file_rows() {
        for height in 12..=24 {
            let mut m = fleet(8);
            m.toggle_view();
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
            let (turn, _) = laid.agent.as_ref().expect("[2] shows");
            let list_clamped = list_room(height - 1) <= LIST_MIN;
            assert!(
                list_clamped || usize::from(turn.area.height) >= DETAIL_MIN + 2,
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
    fn a_git_error_moves_into_the_turn_when_the_column_is_too_short() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("rate-limit-1")));
        m.facts
            .insert(name("rate-limit-1"), Err("fatal: not a repo".into()));
        let laid = laid(&mut m, (80, 12));
        assert!(laid.changes.is_none(), "no room for [3]");
        assert_eq!(laid.room, WorkRoom::TooShort);
        let lines = agent_lines(&laid);
        assert!(
            lines.iter().any(|l| l == " git: fatal: not a repo"),
            "one gutter, not two\n{}",
            lines.join("\n")
        );
    }

    /// The owner's three states at a 70-row terminal and at 80x24, as the
    /// sizing rule's worked examples give them: heights with borders as
    /// [1] / [2] / [3].
    #[test]
    fn the_split_follows_content_and_focus() {
        let heights = |g: Geometry| {
            (
                g.list + 2,
                g.lower.turn_height(),
                g.lower.work_height().unwrap_or(0),
            )
        };
        let column = |rows: usize, list: usize| Column {
            list,
            lower: LowerRoom::of(rows - list - 2).expect("room for a panel"),
        };
        let shaped = |col: Column, turn: usize, need: Option<usize>| Shape {
            turn,
            work: need.map(|need| {
                match col.lower {
                    LowerRoom::Both { inside } => Some(WorkShape::of(need, need, inside)),
                    LowerRoom::TurnOnly { .. } => None,
                }
                .expect("the column has room for [3]")
            }),
        };
        let turn = |offset| Focus::Turn { offset };
        let work = Focus::Work { file: 0 };
        let tall = column(69, 12);
        let x = shaped(tall, 5, Some(143));
        assert_eq!(heights(split(tall, x, Focus::Neither)), (14, 7, 48));
        assert_eq!(
            heights(split(tall, x, turn(0))),
            (14, 7, 48),
            "fits, nothing moves"
        );
        let long = shaped(tall, 40, Some(143));
        assert_eq!(heights(split(tall, long, Focus::Neither)), (14, 19, 36));
        assert_eq!(
            heights(split(tall, long, turn(0))),
            (14, 42, 13),
            "takes from [3] only"
        );
        let longer = shaped(tall, 70, Some(143));
        assert_eq!(
            heights(split(tall, longer, turn(0))),
            (5, 63, 1),
            "then [1], then collapses [3]"
        );
        assert_eq!(heights(split(tall, x, work)), (5, 1, 63));
        let few = shaped(tall, 5, Some(38));
        assert_eq!(
            heights(split(tall, few, work)),
            (14, 7, 48),
            "37 files already fit"
        );

        let alone = shaped(tall, 30, None);
        assert_eq!(heights(split(tall, alone, Focus::Neither)), (14, 55, 0));
    }

    #[test]
    fn the_split_follows_content_and_focus_at_small_sizes() {
        let heights = |g: Geometry| {
            (
                g.list + 2,
                g.lower.turn_height(),
                g.lower.work_height().unwrap_or(0),
            )
        };
        let column = |rows: usize, list: usize| Column {
            list,
            lower: LowerRoom::of(rows - list - 2).expect("room for a panel"),
        };
        let shaped = |col: Column, turn: usize, need: Option<usize>| Shape {
            turn,
            work: need.map(|need| {
                match col.lower {
                    LowerRoom::Both { inside } => Some(WorkShape::of(need, need, inside)),
                    LowerRoom::TurnOnly { .. } => None,
                }
                .expect("the column has room for [3]")
            }),
        };
        let turn = |offset| Focus::Turn { offset };
        let work = Focus::Work { file: 0 };
        let small = column(23, 9);
        let x = shaped(small, 5, Some(143));
        assert_eq!(heights(split(small, x, Focus::Neither)), (11, 6, 6));
        assert_eq!(
            heights(split(small, x, turn(0))),
            (10, 7, 6),
            "one row from [1]"
        );
        let twelve = shaped(small, 12, Some(143));
        assert_eq!(
            heights(split(small, twelve, turn(0))),
            (8, 14, 1),
            "collapse gives rows back to [1]"
        );
        assert_eq!(heights(split(small, x, work)), (5, 1, 17));
        let five = shaped(small, 5, Some(6));
        assert_eq!(heights(split(small, five, work)), (9, 6, 8));
        let two = shaped(small, 9, Some(2));
        assert_eq!(
            heights(split(small, two, Focus::Neither)),
            (11, 8, 4),
            "[3] keeps only what it has, so [2] gets the rest"
        );
        let empty = shaped(small, 0, Some(143));
        assert_eq!(
            heights(split(small, empty, Focus::Neither)),
            (11, 1, 11),
            "an empty [2] is its title line"
        );
        assert_eq!(
            heights(split(small, empty, work)),
            (5, 1, 17),
            "focused [3] beside it takes from the list only"
        );
        let empty_fits = shaped(small, 0, Some(6));
        assert_eq!(
            heights(split(small, empty_fits, work)),
            heights(split(small, empty_fits, Focus::Neither)),
            "and moves nothing while the files fit"
        );

        let tiny = column(15, 3);
        let x = shaped(tiny, 5, Some(143));
        assert_eq!(heights(split(tiny, x, Focus::Neither)), (5, 4, 6));
        assert_eq!(
            heights(split(tiny, x, turn(0))),
            (5, 9, 1),
            "the list is at its minimum, so the surplus stays with [2]"
        );
        assert_eq!(heights(split(tiny, x, work)), (5, 1, 9));

        let alone = shaped(small, 30, None);
        assert_eq!(
            heights(split(small, alone, turn(0))),
            (5, 18, 0),
            "no [3]: only [1] gives way"
        );
    }

    #[test]
    fn a_focused_changes_panel_takes_only_the_rows_its_files_need() {
        let mut m = fleet(20);
        m.toggle_view();
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let unfocused = laid(&mut m, (80, 24));
        let list_height = unfocused.list.area.height;
        let turn_height = unfocused.agent.as_ref().expect("[2] shows").0.area.height;
        let (work, changes) = unfocused.changes.as_ref().expect("[3] shows");
        let head = usize::from(
            texts(&work.lines)
                .first()
                .is_some_and(|l| l.contains("ready")),
        );
        let hidden = (changes.diff.files + head)
            .checked_sub(usize::from(work.area.height) - 2)
            .filter(|hidden| *hidden > 0)
            .expect("the fixture leaves [3] short of rows");

        m.panel = Focused::Changes { file: 0 };
        let focused = laid(&mut m, (80, 24));
        let (work, _) = focused.changes.as_ref().expect("[3] shows");
        assert_eq!(
            usize::from(work.area.height),
            usize::from(unfocused.changes.as_ref().expect("[3] shows").0.area.height) + hidden,
            "exactly the deficit"
        );
        assert_eq!(
            focused.agent.as_ref().expect("[2] shows").0.area.height,
            turn_height,
            "[2] is at its minimum, so the list pays"
        );
        assert_eq!(
            usize::from(focused.list.area.height),
            usize::from(list_height) - hidden
        );
        assert!(
            !texts(&work.lines).iter().any(|l| l.contains(" more")),
            "everything shows\n{}",
            texts(&work.lines).join("\n")
        );
    }

    #[test]
    fn items_read_in_order_whatever_their_priority() {
        let fixed = |order, n: usize| {
            Item::fixed(
                order,
                (0..n)
                    .map(|i| Line::raw(format!("{order:?} {i}")))
                    .collect(),
            )
        };
        let items = [
            fixed(Order::State, 1),
            fixed(Order::Lead, 2),
            fixed(Order::Tree, 2),
            fixed(Order::Prompt, 1),
        ];
        assert_eq!(
            texts(&fill(&items, 6)),
            [
                "State 0", "Prompt 0", "Lead 0", "Lead 1", "Tree 0", "Tree 1"
            ]
        );
        assert_eq!(
            texts(&fill(&items, 5)),
            ["State 0", "Lead 0", "Lead 1", "Tree 0", "Tree 1"],
            "the prompt is the first to go"
        );
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
        let lines = texts(&changes(&facts, &shared, 34, 4, None).lines);
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
            Focused::Changes { file: 0 },
            "one file changed"
        );
        m.facts.remove(&name("rate-limit-1"));
        assert_eq!(laid(&mut m, (200, 50)).focus, Focused::Agents);
    }

    #[test]
    fn the_agent_panel_repeats_the_card_only_when_the_card_cuts_it() {
        let mut m = fleet(8);
        m.toggle_view();
        m.set_cursor(Cursor::Entry(name("payroll-fix-3")));
        let lines = agent_lines(&laid(&mut m, (200, 50)));
        assert!(
            !lines[0].contains("Edit"),
            "the card shows it whole\n{}",
            lines.join("\n")
        );
        let lines = agent_lines(&laid(&mut m, (80, 24)));
        let state = lines[0].trim();
        let after_glyph = state
            .split_once(' ')
            .map(|(_, rest)| rest)
            .unwrap_or_default();
        assert!(
            after_glyph.starts_with("Edit ") && after_glyph.trim_end().ends_with("38s"),
            "the short card cuts it, so [2] names the tool with the time\n{}",
            lines.join("\n")
        );
        assert!(
            !state.contains('…') && !state.contains("calc.ts"),
            "not a cut copy of the lead\n{state}"
        );
        assert!(
            lines
                .concat()
                .replace(' ', "")
                .contains("src/payroll/rules/overtime/calc.ts"),
            "and the lead says it whole\n{}",
            lines.join("\n")
        );
    }

    #[test]
    fn the_focused_panel_scrolls_and_clamps() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.panel = Focused::Agent { offset: 99 };
        let scrolled = laid(&mut m, (80, 24));
        let Focused::Agent { offset } = scrolled.focus else {
            panic!("{:?}", scrolled.focus)
        };
        assert!(offset < 99, "clamped to what [2] shows");
        m.panel = Focused::Changes { file: 2 };
        let changed = laid(&mut m, (200, 50));
        let lines = texts(&changed.changes.expect("succeeds").0.lines);
        let marked = lines
            .iter()
            .position(|l| l.starts_with('▌'))
            .expect("a marked row");
        assert_eq!(marked, 3, "under the readiness row\n{}", lines.join("\n"));
        assert!(lines[marked].contains("types.ts"), "{}", lines.join("\n"));
    }
}
