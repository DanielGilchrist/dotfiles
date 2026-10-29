//! The sidebar, three panels stacked lazygit-style: [1] the agents as
//! cards, [2] the selected agent, [3] the files it changed.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::Path;
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
    Pulse, Row, Target,
};

/// Rows [2] keeps inside its border however long the list is.
const DETAIL_MIN: usize = 4;
/// Rows [3] keeps inside its border however long the list is. It's kept
/// even for an agent with no changes, so moving the selection never
/// changes how much room the list has.
const CHANGES_MIN: usize = 3;
/// Inner widths below this use the short forms.
const SHORT_BELOW: usize = 36;
const LEAD_LINES: usize = 3;
const PROMPT_LINES: usize = 2;
/// Trailing path components a changed file keeps when they fit.
const PATH_TAIL: usize = 3;
/// Shared files listed one per line, beyond which they're summed up.
const OVERLAP_LINES: usize = 2;

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
    /// By its label, with its repo on the right, as in Needs you and Pinned.
    Grouped(String),
    /// Like [`Title::Grouped`], for an agent from another repo listed in
    /// Focus. It never takes a second line, so it can't change the tier.
    Elsewhere(String),
    /// By its repo, standing in for a repo with no other entry.
    Repo(String),
}

impl Title {
    fn indent(&self) -> &'static str {
        match self {
            Self::Label | Self::Repo(_) => "",
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
            Title::Grouped(name) | Title::Elsewhere(name) => {
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

    /// Files this agent changed that others in its repo changed too: each
    /// file's name with the others that changed it.
    fn overlaps(&self, repo: &Repo, entry: &Entry) -> Vec<(String, Vec<&'m Entry>)> {
        let Some(Ok(mine)) = self.m.facts.get(entry.name()) else {
            return Vec::new();
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
                let file = basename(&change.path.to_string_lossy()).to_string();
                (!others.is_empty()).then_some((file, others))
            })
            .collect()
    }

    /// [2]: what the selected agent is doing, unless its `card` says it
    /// whole, then what the pane can't show, in priority order.
    fn detail(&self, repo: &Repo, entry: &Entry, card: Shown) -> Detail {
        let m = self.m;
        let status = m.status(entry);
        let activity = m.activity(entry.name());
        let width = self.content;
        let in_pane = self.in_pane(entry);
        let heard = activity.map_or(m.now, |a| a.heard);
        let gist = Gist::of(&status, heard, m.now);
        let time = time_in_state(&status, m.now);
        let room = width.saturating_sub(2 + line_width(&time) + 1);
        let (text, shown) = match (&gist, &status) {
            (Some(gist), _) => gist.line(self.form, room),
            (None, AgentStatus::Idle) => ("idle".to_string(), Shown::Whole),
            (None, AgentStatus::Exited) => ("exited".to_string(), Shown::Whole),
            (None, _) => ("no session".to_string(), Shown::Whole),
        };
        let style = gist.as_ref().map_or_else(dim, Gist::style);
        let state = justify(
            vec![glyph(&status), Span::raw(" "), Span::styled(text, style)],
            time,
            width,
        );
        let state = (card != Shown::Whole).then(|| Item::fixed(vec![state]));
        let lead = gist
            .filter(|_| state.is_some() && !in_pane && shown == Shown::Cut)
            .and_then(|gist| {
                let style = gist.style();
                gist.lead().map(|text| Item {
                    cap: LEAD_LINES,
                    content: Content::Text {
                        rows: wrap(&text, width),
                        style,
                        width,
                        hang: "",
                    },
                })
            });
        let tree = activity.filter(|_| m.subagents(entry).is_some()).map(|a| {
            Item::fixed(tree(
                a.subagents.values().collect(),
                self.form,
                width,
                m.now,
            ))
        });
        let overlap = overlap_item(&self.overlaps(repo, entry), width);
        let readiness = match m.facts.get(entry.name()) {
            Some(Ok(facts)) => shows_readiness(entry, &status, facts.readiness())
                .then(|| Item::fixed(vec![readiness(facts.readiness())])),
            Some(Err(e)) => Some(Item::fixed(vec![Line::styled(
                truncate_end(&format!("git: {e}"), width),
                dim(),
            )])),
            None => None,
        };
        let prompt = activity.and_then(|a| a.prompt.as_deref()).map(|prompt| {
            let room = width.saturating_sub(2);
            Item {
                cap: PROMPT_LINES,
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
        } else if repo.name() != entry.label() {
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
            items: [state, lead, tree, overlap, readiness, prompt]
                .into_iter()
                .flatten()
                .collect(),
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

/// `{first} +{k}`: who else changed it, counted rather than listed so the
/// file name is never what gets cut.
fn others(agents: &[&Entry]) -> String {
    match agents {
        [] => String::new(),
        [only] => only.label().to_string(),
        [first, rest @ ..] => format!("{} +{}", first.label(), rest.len()),
    }
}

/// `overlap flow.ts · auth-refresh-2 +2` per shared file (the file name
/// Yellow, the rest DIM), or all of them on one line when they don't fit:
/// `overlap 5 files · docs-sweep-6 +2`.
fn overlap_item(overlaps: &[(String, Vec<&Entry>)], width: usize) -> Option<Item> {
    let yellow = Style::new().fg(Color::Yellow);
    let line = |subject: Span<'static>, agents: &[&Entry]| {
        Line::from(fit(
            vec![
                Span::styled("overlap ", dim()),
                subject,
                Span::styled(format!(" · {}", others(agents)), dim()),
            ],
            width,
        ))
    };
    let mut everyone: Vec<&Entry> = Vec::new();
    for (_, agents) in overlaps {
        for agent in agents {
            if !everyone.iter().any(|e| e.name() == agent.name()) {
                everyone.push(agent);
            }
        }
    }
    let all = match overlaps {
        [] => return None,
        [(file, agents)] => line(Span::styled(file.clone(), yellow), agents),
        _ => line(
            Span::styled(format!("{} files", overlaps.len()), yellow),
            &everyone,
        ),
    };
    let files = overlaps
        .iter()
        .take(OVERLAP_LINES + 1)
        .map(|(file, agents)| line(Span::styled(file.clone(), yellow), agents))
        .collect();
    Some(Item {
        cap: OVERLAP_LINES,
        content: Content::Overlap { files, all },
    })
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
                (Some(description), Form::Short) => (description, format!(" {since}")),
                (None, _) => (subagent.kind.as_str(), format!(" {since}")),
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
/// columns, in `rows` rows, each after a gutter. With a cursor, the rows
/// scroll to keep it in view and its gutter is marked. Without one, the
/// last row says how many more when they don't all fit.
fn changes(facts: &WorktreeFacts, width: usize, rows: usize, cursor: Option<usize>) -> Changes {
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
    let mut lines: Vec<Line<'static>> = shown
        .iter()
        .zip(columns)
        .enumerate()
        .map(|(i, (change, (added, removed)))| {
            let mut right = vec![Span::styled(
                format!("{:>added_width$}", added.content),
                added.style,
            )];
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
        diff: facts.diff(),
    }
}

/// [3]'s rows and the totals for its title.
struct Changes {
    lines: Vec<Line<'static>>,
    diff: DiffStat,
}

/// A detail item: what it shows in as many rows as it's given, and how
/// many it takes when rows aren't spare.
struct Item {
    cap: usize,
    content: Content,
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
    /// A line per shared file, or one line for all of them.
    Overlap {
        files: Vec<Line<'static>>,
        all: Line<'static>,
    },
}

impl Item {
    fn fixed(lines: Vec<Line<'static>>) -> Self {
        Self {
            cap: lines.len(),
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
            Content::Overlap { files, all } => match rows {
                0 => Vec::new(),
                rows if files.len() <= rows.min(self.cap) => files.clone(),
                _ => vec![all.clone()],
            },
        }
    }
}

/// Items at their caps in priority order, each shrunk to the rows left
/// rather than skipped, stopping at the first that can't show at all, so a
/// lower item never takes rows from a higher one. Rows still spare then let
/// them grow past their caps.
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
    let mut out = Vec::new();
    for (item, height) in items.iter().zip(heights) {
        let lines = item.fit(height + left);
        left -= lines.len().saturating_sub(height);
        out.extend(lines);
    }
    out
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
    let kept = u16::try_from(DETAIL_MIN + CHANGES_MIN + 6).unwrap_or(u16::MAX);
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
    /// [2], while an agent is selected.
    agent: Option<(Panel, Detail)>,
    /// The focused panel, its position clamped to what it shows.
    focus: Focused,
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
    let agent_rows = match selection {
        Some(_) if rest >= 3 => rest,
        _ => 0,
    };
    let list_rows = height - agent_rows;
    let panel_at = |y: usize, rows: usize| {
        let y = area.y + u16::try_from(y).unwrap_or(u16::MAX);
        Rect::new(
            area.x,
            y,
            area.width,
            u16::try_from(rows).unwrap_or(u16::MAX),
        )
    };

    let mut hits = Vec::new();
    let mut lines = Vec::new();
    for (i, (line, row)) in list.lines.into_iter().enumerate().skip(offset).take(room) {
        let y = area.y + 1 + u16::try_from(i - offset).unwrap_or(0);
        hits.extend(row.map(|c| (y, c)));
        lines.push(line);
    }
    let anchor = cursor
        .cloned()
        .zip(list.selected.map(|r| r.start.saturating_sub(offset)));

    let (agent, changes, focus) = match selection.filter(|_| agent_rows > 0) {
        None => (None, None, Focused::Agents),
        Some((repo, entry)) => {
            let below = Rect::new(
                area.x,
                area.y + u16::try_from(list_rows).unwrap_or(u16::MAX),
                area.width,
                u16::try_from(agent_rows).unwrap_or(u16::MAX),
            );
            let below = agent_panels(&deck, repo, entry, list.card, below);
            (Some(below.agent), below.changes, below.focus)
        }
    };
    Some(Laid {
        list: Panel {
            area: panel_at(0, list_rows),
            lines,
        },
        hits,
        scroll: ListScroll { offset, anchor },
        agent,
        focus,
        changes,
    })
}

/// [2] and [3] for the selected agent in `area`, and the focus clamped to
/// what they show. [2] gets its capped items and the rows [3] doesn't need
/// for its files, and [3] the rest.
fn agent_panels(deck: &Deck<'_>, repo: &Repo, entry: &Entry, card: Shown, area: Rect) -> Below {
    let m = deck.m;
    let rows = usize::from(area.height);
    let at = |y: usize, height: usize| {
        Rect::new(
            area.x,
            area.y + u16::try_from(y).unwrap_or(u16::MAX),
            area.width,
            u16::try_from(height).unwrap_or(u16::MAX),
        )
    };
    let detail = deck.detail(repo, entry, card);
    let facts = match m.facts.get(entry.name()) {
        Some(Ok(facts)) if !facts.changed.is_empty() => Some(facts),
        _ => None,
    }
    .filter(|_| rows >= 1 + 2 + CHANGES_MIN + 2);
    let inside = match facts {
        Some(facts) => {
            let capped: usize = detail.items.iter().map(|i| i.fit(i.cap).len()).sum();
            let most = rows - 2 - CHANGES_MIN - 2;
            let unwanted = rows.saturating_sub(2 + facts.changed.len().max(CHANGES_MIN) + 2);
            fill(&detail.items, capped.min(most).max(unwanted))
                .len()
                .max(1)
        }
        None => rows - 2,
    };
    let file = match m.panel {
        Focused::Changes { file } => facts.map(|f| file.min(f.changed.len() - 1)),
        Focused::Agents | Focused::Agent { .. } => None,
    };
    let changes = facts.map(|facts| {
        let height = rows - inside - 2;
        let changes = changes(facts, deck.content, height - 2, file);
        let panel = Panel {
            area: at(inside + 2, height),
            lines: changes.lines.clone(),
        };
        (panel, changes)
    });
    let (filled, focus) = match (m.panel, file) {
        (Focused::Agent { offset }, _) => {
            let all = fill(&detail.items, usize::MAX);
            let offset = offset.min(all.len().saturating_sub(inside));
            let shown = all.into_iter().skip(offset).take(inside).collect();
            (shown, Focused::Agent { offset })
        }
        (_, Some(file)) => (fill(&detail.items, inside), Focused::Changes { file }),
        (Focused::Agents | Focused::Changes { .. }, None) => {
            (fill(&detail.items, inside), Focused::Agents)
        }
    };
    Below {
        agent: (pad(at(0, inside + 2), filled), detail),
        changes,
        focus,
    }
}

/// [2] and [3], if there's room for it, with the focus clamped to them.
struct Below {
    agent: (Panel, Detail),
    changes: Option<(Panel, Changes)>,
    focus: Focused,
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
    let title = fit(sidebar_title(m), usize::from(area.width).saturating_sub(4));
    let corner = sidebar_count(m, style_of(agents));
    let Some(laid) = lay_out(m, area) else {
        frame.render_widget(panel(title, corner, style_of(agents), nav && agents), area);
        return;
    };
    let list = panel(title, corner, style_of(agents), nav && agents);
    frame.render_widget(Paragraph::new(laid.list.lines).block(list), laid.list.area);
    if let Some((Panel { area, lines }, detail)) = laid.agent {
        let block = panel(detail.title, Vec::new(), style_of(agent), nav && agent)
            .title_top(Line::from(pad_title(detail.right)).right_aligned());
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
        let corner = match laid.focus {
            Focused::Changes { file } => vec![Span::styled(
                format!("{} of {}", file + 1, changes.diff.files),
                style_of(files),
            )],
            Focused::Agents | Focused::Agent { .. } => Vec::new(),
        };
        frame.render_widget(
            Paragraph::new(lines).block(panel(title, corner, style_of(files), nav && files)),
            area,
        );
    }
    m.panel = laid.focus;
    m.scroll = laid.scroll;
    m.areas.rows = laid.hits;
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

    use super::*;
    use crate::fixtures::{fleet, name, reported};
    use crate::model::{Group, Pane, View};

    const SIZES: [(u16, u16); 3] = [(200, 50), (100, 30), (80, 24)];
    const FLEETS: [usize; 5] = [0, 1, 2, 8, 20];

    fn n(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn texts(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(text).collect()
    }

    /// Draws the whole screen, as the app does.
    fn render(m: &mut Model, (width, height): (u16, u16)) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| super::super::draw(frame, m)).unwrap();
        terminal
    }

    /// The sidebar's column on a screen `size` big, as last drawn: the
    /// left edge above the status bar.
    fn column(m: &Model, (_, height): (u16, u16)) -> Rect {
        Rect::new(0, 0, m.areas.sidebar.width + 2, height - 1)
    }

    fn laid(m: &mut Model, size: (u16, u16)) -> Laid {
        render(m, size);
        lay_out(m, column(m, size)).unwrap()
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
        let snapshot = m.snapshot().unwrap();
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
        assert_eq!(list_room(49), 36);
        assert_eq!(list_room(23), 10);
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
        assert_eq!(
            texts(&changes(&facts, 34, 4, None).lines),
            [
                " src/payroll/calc.ts        +98 −52",
                " src/payroll/calc.test.ts   +24    ",
                " logo.png                   bin    ",
                " notes.md                   new    ",
            ]
        );
        let short = changes(&facts, 34, 3, None);
        assert_eq!(texts(&short.lines)[2], " +2 more");
        let cursor = changes(&facts, 34, 2, Some(3));
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
        for size in SIZES {
            for agents in FLEETS {
                for view in [View::Focus, View::Overview] {
                    let mut m = fleet(agents);
                    if m.view != view {
                        m.toggle_view();
                    }
                    let context = format!("{size:?}, {agents} agents, {view:?}");
                    let laid = laid(&mut m, size);
                    let column = column(&m, size);
                    let mut panels = vec![&laid.list];
                    panels.extend(laid.agent.as_ref().map(|(p, _)| p));
                    panels.extend(laid.changes.as_ref().map(|(p, _)| p));
                    let mut y = column.y;
                    for panel in panels {
                        assert_eq!(panel.area.y, y, "{context}: panels stack");
                        assert!(
                            panel.lines.len() + 2 <= usize::from(panel.area.height),
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
                    m.move_cursor(1);
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
        for (_, height) in SIZES {
            let rows = usize::from(list_room(height - 1));
            let mut m = fleet(20);
            m.toggle_view();
            let before = tier(m.extent(), rows);
            let everyone_done: HashMap<_, _> = m
                .snapshot()
                .unwrap()
                .entries()
                .map(|(_, e)| (e.name().clone(), reported(State::Done { summary: None }, 5)))
                .collect();
            m.activities = Some(everyone_done);
            m.revalidate();
            assert_eq!(tier(m.extent(), rows), before, "{rows} rows");
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
            .unwrap();
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
        let mut activities = m.activities.clone().unwrap();
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
        let mut activities = m.activities.clone().unwrap();
        activities.insert(
            name("payroll-fix-3"),
            reported(State::Working { tools: Vec::new() }, 400),
        );
        m.activities = Some(activities);
        let list = laid(&mut m, (200, 50)).list.lines;
        let failed = texts(&list)
            .iter()
            .position(|l| l.contains("! rate-limit-1"))
            .unwrap();
        assert_eq!(texts(&list)[failed + 1].trim(), "rate limited");
        let stale = list
            .iter()
            .find(|l| text(l).contains("payroll-fix-3"))
            .unwrap();
        let time: Vec<_> = stale.spans.iter().rev().take(2).collect();
        assert_eq!(time[0].content, "?");
        assert_eq!(time[0].style.fg, Some(Color::Yellow));
        assert_eq!(time[1].content, "6m");

        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let lines = agent_lines(&laid(&mut m, (200, 50)));
        assert!(
            lines[0].trim_start().starts_with('├'),
            "the card already says what it's doing\n{}",
            lines.join("\n")
        );
        let tree: Vec<&String> = lines
            .iter()
            .filter(|l| l.trim_start().starts_with(['├', '└']))
            .collect();
        assert_eq!(tree.len(), 3, "{}", lines.join("\n"));
        assert!(tree[2].contains("Adversarial review:…  general-purpose  30s"));
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
        let (_, detail) = laid.agent.unwrap();
        assert_eq!(texts(&[Line::from(detail.right)]), ["in pane"]);
    }

    #[test]
    fn overlaps_keep_the_file_and_count_the_agents() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("rate-limit-1")));
        let laid = laid(&mut m, (200, 50));
        let lines = &laid.agent.as_ref().unwrap().0.lines;
        let overlap = lines.iter().find(|l| text(l).contains("overlap ")).unwrap();
        assert_eq!(text(overlap).trim(), "overlap flow.ts · auth-refresh-2 +2");
        let file = overlap
            .spans
            .iter()
            .find(|s| s.content == "flow.ts")
            .unwrap();
        assert_eq!(file.style.fg, Some(Color::Yellow));
    }

    #[test]
    fn focus_lists_who_needs_you_in_other_repos() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("agent-tui0")));
        let lines = texts(&laid(&mut m, (200, 50)).list.lines);
        let header = lines.iter().position(|l| l.contains("Needs you")).unwrap();
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
        assert!(
            lines[0].contains("Edit") && lines[0].trim_end().ends_with("38s"),
            "the short card cuts it, so [2] says it with the time\n{}",
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
        let lines = texts(&changed.changes.unwrap().0.lines);
        assert!(lines[2].starts_with('▌'), "{}", lines.join("\n"));
    }
}
