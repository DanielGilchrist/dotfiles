//! The sidebar: the agents as cards, then detail blocks under them about
//! the selection and every other agent that isn't settled.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::Path;
use std::time::SystemTime;

use agent_core::activity::Subagent;
use agent_core::model::{Entry, EntryKind, Head, Repo, RepoKey, Snapshot};
use agent_core::names::SessionName;
use agent_core::ports::{Lines, Readiness, WorktreeFacts};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use super::{
    ACCENT, NAME_MIN, ago, bold, dim, fit, glyph, justify, line_width, name_style, scroll,
    sidebar_focused, spinner, truncate_end, truncate_start, wrap,
};
use crate::model::{
    AgentStatus, Cursor, Doing, Extent, Failure, Fold, ListScroll, Model, Place, Pulse, Row,
    Target, View,
};

/// Rows the list leaves for the detail when choosing a tier.
const DETAIL_RESERVE: usize = 6;
/// Inner widths below this use the short forms.
const SHORT_BELOW: usize = 36;
const LEAD_LINES: usize = 3;
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

/// The most second lines that leave the detail [`DETAIL_RESERVE`] of a
/// panel `rows` tall, counting every live card as if it had one.
fn tier(extent: Extent, rows: usize) -> Tier {
    let avail = rows.saturating_sub(DETAIL_RESERVE);
    if extent.rows + extent.live <= avail {
        Tier::Two
    } else if extent.rows < avail {
        Tier::Waiting {
            spare: avail - extent.rows,
        }
    } else {
        Tier::One
    }
}

/// How much of its gist a card's second line showed, which decides
/// whether the agent's block leads with the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Whole,
    Cut,
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

/// How a card names its agent.
enum Title {
    /// By its label, as under its repo and in Focus.
    Label,
    /// By its label, with its repo on the right, as in Needs you and Pinned.
    Grouped(String),
    /// Like [`Title::Grouped`], for an agent from another repo listed in
    /// Focus. It never takes a second line, so it can't change the tier.
    Elsewhere(String),
    /// By its repo, standing in for a repo with no other entry.
    Repo(String),
}

/// One agent in the list: line 1 always, line 2 when the tier gives it one.
struct Card {
    lines: Vec<Line<'static>>,
    shown: Shown,
}

/// What every card and block on screen shares.
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

    fn card(&self, entry: &Entry, title: Title, selected: bool) -> Card {
        let m = self.m;
        let status = m.status(entry);
        let heard = m.activity(entry.name()).map_or(m.now, |a| a.heard);
        let gist = Gist::of(&status, heard, m.now)
            .filter(|_| !matches!(title, Title::Elsewhere(_)) && self.has_line2(entry.name()));
        let subagents = match gist {
            Some(Gist::Delegating { .. }) => None,
            _ => m.subagents(entry),
        };
        let line1 = self.line1(entry, &status, title, subagents, selected);
        match gist {
            Some(gist) => {
                let (text, shown) = gist.line(self.form, self.content.saturating_sub(2));
                let line2 = Line::from(vec![Span::raw("  "), Span::styled(text, gist.style())]);
                Card {
                    lines: vec![line1, line2],
                    shown,
                }
            }
            None => Card {
                lines: vec![line1],
                shown: Shown::Hidden,
            },
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
        let mut left = vec![glyph(status), Span::raw(" ")];
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
            Title::Label => left.push(Span::styled(entry.label().to_string(), style)),
        }
        left.extend(subagents.map(|n| Span::styled(format!(" ↳{n}"), dim())));
        let mut time = time_in_state(status, m.now);
        let mut markers = Vec::new();
        if let Some(pin) = entry.pin() {
            markers.push(Span::styled(
                pin.slot()
                    .map_or_else(|| "*".to_string(), |s| s.get().to_string()),
                dim(),
            ));
        }
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

    /// The agent's block: its header rule, then its items in display order,
    /// which is also the order they survive in.
    fn block(&self, repo: &Repo, entry: &Entry, shown: Shown, role: Role) -> Block {
        let m = self.m;
        let status = m.status(entry);
        let activity = m.activity(entry.name());
        let width = self.content;
        let heard = activity.map_or(m.now, |a| a.heard);
        let lead = Gist::of(&status, heard, m.now)
            .filter(|_| !self.in_pane(entry) && matches!(shown, Shown::Cut | Shown::Hidden))
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
        let overlap = overlap_item(&self.overlaps(repo, entry), width);
        let tree = activity.filter(|_| m.subagents(entry).is_some()).map(|a| {
            Item::fixed(tree(
                a.subagents.values().collect(),
                self.form,
                width,
                m.now,
            ))
        });
        let (readiness, files) = match m.facts.get(entry.name()) {
            Some(Ok(facts)) => (
                shows_readiness(entry, &status, facts.readiness())
                    .then(|| Item::fixed(vec![readiness(facts.readiness())])),
                files(facts, self.form, width, role.files()),
            ),
            Some(Err(e)) => (
                Some(Item::fixed(vec![Line::styled(
                    truncate_end(&format!("git: {e}"), width),
                    dim(),
                )])),
                None,
            ),
            None => (None, None),
        };
        let prompt = activity.and_then(|a| a.prompt.as_deref()).map(|prompt| {
            let room = width.saturating_sub(2);
            Item {
                cap: role.prompt_lines(),
                content: Content::Text {
                    rows: wrap(&format!("› {prompt}"), room),
                    style: dim(),
                    width: room,
                    hang: "  ",
                },
            }
        });
        Block {
            header: self.header(repo, entry, &status, role),
            items: [lead, overlap, tree, readiness, files, prompt]
                .into_iter()
                .flatten()
                .collect(),
        }
    }

    /// `{glyph} {name} ─── {right}`. The right side says `in pane`, or the
    /// repo (in Overview, unless the name says it) and the time in state
    /// (unless the selection's card shows it).
    fn header(
        &self,
        repo: &Repo,
        entry: &Entry,
        status: &AgentStatus,
        role: Role,
    ) -> Line<'static> {
        let right: Vec<Span<'static>> = if self.in_pane(entry) {
            vec![Span::styled("in pane", dim())]
        } else {
            let mut right = Vec::new();
            if self.form == Form::Long
                && self.m.view == View::Overview
                && repo.name() != entry.label()
            {
                right.push(Span::styled(repo.name().into_owned(), dim()));
            }
            let time = match role {
                Role::Selected => Vec::new(),
                Role::Scrolled | Role::Other => time_in_state(status, self.m.now),
            };
            if !right.is_empty() && !time.is_empty() {
                right.push(Span::raw("  "));
            }
            right.extend(time.into_iter().map(|t| t.patch_style(dim())));
            right
        };
        let left = vec![
            glyph(status),
            Span::raw(" "),
            Span::styled(entry.label().to_string(), bold()),
        ];
        rule(left, right, self.content)
    }
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

/// `left`, a DIM rule, then `right`, in `width` columns.
fn rule(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let right_width = match line_width(&right) {
        0 => 0,
        w => w + 1,
    };
    let mut spans = fit(left, width.saturating_sub(right_width + 2));
    spans.push(Span::raw(" "));
    let fill = width.saturating_sub(line_width(&spans) + right_width);
    spans.push(Span::styled("─".repeat(fill), dim()));
    if right_width > 0 {
        spans.push(Span::raw(" "));
        spans.extend(right);
    }
    Line::from(spans)
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

/// `changed n files  +a −r`, then every file in `diff --numstat` order
/// with its counts in columns. It shows `cap` files unless rows are spare.
fn files(facts: &WorktreeFacts, form: Form, width: usize, cap: usize) -> Option<Item> {
    let changed = &facts.changed;
    if changed.is_empty() {
        return None;
    }
    let diff = facts.diff();
    let noun = if diff.files == 1 { "file" } else { "files" };
    let green = Style::new().fg(Color::Green);
    let red = Style::new().fg(Color::Red);
    let (added, removed) = counts(diff.added, diff.removed);
    let mut header = vec![Span::styled(
        format!("changed {} {noun}  ", diff.files),
        dim(),
    )];
    header.push(Span::styled(added.clone(), green));
    if !added.is_empty() && !removed.is_empty() {
        header.push(Span::raw(" "));
    }
    header.push(Span::styled(removed, red));

    let columns: Vec<(Span<'static>, String)> = changed
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
    let rows = changed
        .iter()
        .zip(columns)
        .map(|(change, (added, removed))| {
            let mut right = vec![Span::styled(
                format!("{:>added_width$}", added.content),
                added.style,
            )];
            if removed_width > 0 {
                right.push(Span::styled(format!(" {removed:<removed_width$}"), red));
            }
            let path = change.path.to_string_lossy();
            let path = match form {
                Form::Long => path.as_ref(),
                Form::Short => basename(&path),
            };
            let room = width.saturating_sub(2 + line_width(&right) + 1);
            justify(
                vec![Span::raw("  "), Span::raw(truncate_start(path, room))],
                right,
                width,
            )
        })
        .collect::<Vec<_>>();
    let more = usize::from(rows.len() > cap);
    Some(Item {
        cap: 1 + rows.len().min(cap) + more,
        content: Content::Files {
            header: Line::from(header),
            rows,
        },
    })
}

/// Whether a block is the selection's, which gets more of each item, and
/// whether its card is on screen to show its time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Selected,
    /// The selection, with its card scrolled out of view.
    Scrolled,
    Other,
}

impl Role {
    fn files(self) -> usize {
        match self {
            Self::Selected | Self::Scrolled => 5,
            Self::Other => 3,
        }
    }

    fn prompt_lines(self) -> usize {
        match self {
            Self::Selected | Self::Scrolled => 2,
            Self::Other => 1,
        }
    }
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
    /// A header and a row per file, the rows left out counted as `+k more`.
    Files {
        header: Line<'static>,
        rows: Vec<Line<'static>>,
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

    /// Rows its tallest form takes.
    fn full(&self) -> usize {
        match &self.content {
            Content::Fixed(lines) => lines.len(),
            Content::Text { rows, .. } => rows.len(),
            Content::Files { rows, .. } => 1 + rows.len(),
            Content::Overlap { files, .. } if files.len() <= self.cap => files.len(),
            Content::Overlap { .. } => 1,
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
            Content::Files { header, rows: all } => {
                let listed = match rows {
                    0 => return Vec::new(),
                    rows if rows > all.len() => all.len(),
                    1 | 2 => rows - 1,
                    rows => rows - 2,
                };
                let mut lines = vec![header.clone()];
                lines.extend(all.iter().take(listed).cloned());
                if listed < all.len() && rows > 2 {
                    lines.push(Line::styled(
                        format!("  +{} more", all.len() - listed),
                        dim(),
                    ));
                }
                lines
            }
            Content::Overlap { files, all } => match rows {
                0 => Vec::new(),
                rows if files.len() <= rows.min(self.cap) => files.clone(),
                _ => vec![all.clone()],
            },
        }
    }
}

/// A detail block: a header rule, then its items in priority order.
struct Block {
    header: Line<'static>,
    items: Vec<Item>,
}

/// The selection's block first, each item at its cap or shrunk to the rows
/// left, stopping at the first that can't show at all, so a lower item
/// never takes rows from a higher one. Only once the selection shows
/// everything up to its caps do the other blocks follow, in list order,
/// each starting only if its header and first item fit and the first item
/// that has to shrink ending the detail. Rows still spare then let the
/// selection's items grow past their caps.
fn stack(
    selected: Option<Block>,
    others: impl IntoIterator<Item = Block>,
    rows: usize,
) -> Vec<Line<'static>> {
    let mut heights = Vec::new();
    let mut whole = true;
    if let Some(block) = &selected {
        let mut left = rows.saturating_sub(1);
        for item in &block.items {
            let height = item.fit(item.cap.min(left)).len();
            if height < item.cap.min(item.full()) {
                whole = false;
            }
            if height == 0 {
                break;
            }
            left -= height;
            heights.push(height);
        }
    }
    let mut used = match heights.iter().sum::<usize>() {
        0 => 0,
        n => n + 1,
    };
    let mut rest = Vec::new();
    if whole {
        'blocks: for Block { header, items } in others {
            let Some(first) = items.first() else {
                continue;
            };
            if used + 1 + first.fit(first.cap).len().max(1) > rows {
                break;
            }
            rest.push(header);
            used += 1;
            for item in &items {
                let lines = item.fit(item.cap.min(rows - used));
                let shrunk = lines.len() < item.cap.min(item.full());
                used += lines.len();
                rest.extend(lines);
                if shrunk {
                    break 'blocks;
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Some(Block { header, items }) = selected
        && !heights.is_empty()
    {
        let mut spare = rows - used;
        out.push(header);
        for (item, height) in items.iter().zip(heights) {
            let lines = item.fit(height + spare);
            spare -= lines.len().saturating_sub(height);
            out.extend(lines);
        }
    }
    out.extend(rest);
    out
}

/// The list's lines with the row each belongs to, the selected card's
/// lines, and every agent listed with how its card showed its gist.
struct List {
    lines: Vec<(Line<'static>, Option<Cursor>)>,
    selected: Option<Range<usize>>,
    agents: Vec<(SessionName, Shown)>,
}

fn list(deck: &Deck<'_>, snapshot: &Snapshot, rows: &[Row], cursor: Option<&Cursor>) -> List {
    let focused = sidebar_focused(deck.m);
    let mut list = List {
        lines: Vec::new(),
        selected: None,
        agents: Vec::new(),
    };
    for row in rows {
        let row_cursor = row.cursor();
        let selected = row_cursor.is_some() && row_cursor.as_ref() == cursor;
        let (lines, agent) = match row {
            Row::Header(group) => (
                vec![justify(
                    vec![
                        Span::styled("▾ ", dim()),
                        Span::styled(group.title(), dim()),
                    ],
                    Vec::new(),
                    deck.content,
                )],
                None,
            ),
            Row::Repo { key, fold } => match snapshot.repo(key) {
                Some(repo) => (vec![deck.repo_line(repo, *fold, selected)], None),
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
                    _ => Title::Label,
                };
                let card = deck.card(entry, title, selected);
                (card.lines, Some((name.clone(), card.shown)))
            }
        };
        if let Some((name, shown)) = agent
            && !list.agents.iter().any(|(n, _)| *n == name)
        {
            list.agents.push((name, shown));
        }
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

/// The sidebar as laid out for one frame: its lines, which row each list
/// line selects, and the scroll to remember.
struct Laid {
    lines: Vec<Line<'static>>,
    hits: Vec<(u16, Cursor)>,
    scroll: ListScroll,
}

/// The list, scrolled to keep the selection in view (and on the same
/// screen row while it moves between groups, when scrolling allows),
/// then the detail in the rows under it.
fn lay_out(m: &Model, area: Rect) -> Option<Laid> {
    let snapshot = m.snapshot()?;
    let height = usize::from(area.height);
    let deck = Deck::new(m, snapshot, usize::from(area.width), height);
    let cursor = m.cursor();
    let list = list(&deck, snapshot, &m.rows(), cursor);
    let len = list.lines.len();
    let selected_line = list.selected.as_ref().map_or(0, |r| r.start);
    let preferred = match (&m.scroll.anchor, cursor) {
        (Some((was, at)), Some(now)) if was.follows(now) => selected_line.saturating_sub(*at),
        _ => m.scroll.offset,
    };
    let offset = scroll(preferred, selected_line, height, len);
    let visible = len.saturating_sub(offset).min(height);
    let on_screen = offset..offset + visible;

    let block_of = |name: &SessionName, shown: Shown, role: Role| {
        snapshot
            .entry(name)
            .map(|(repo, entry)| deck.block(repo, entry, shown, role))
    };
    let shown_of = |name: &SessionName| {
        list.agents
            .iter()
            .find(|(n, _)| n == name)
            .map_or(Shown::Hidden, |(_, shown)| *shown)
    };
    let role = match &list.selected {
        Some(card) if on_screen.contains(&card.start) => Role::Selected,
        _ => Role::Scrolled,
    };
    let selected_name = cursor.and_then(Cursor::entry_name);
    let selected = selected_name.and_then(|name| block_of(name, shown_of(name), role));
    let others = list
        .agents
        .iter()
        .filter(|(name, _)| Some(name) != selected_name)
        .filter(|(name, _)| {
            snapshot.entry(name).is_some_and(|(_, e)| {
                let status = m.status(e);
                Gist::of(&status, m.now, m.now).is_some()
            })
        })
        .filter_map(|(name, shown)| block_of(name, *shown, Role::Other));
    let detail = stack(selected, others, height - visible);

    let mut hits = Vec::new();
    let mut lines = Vec::new();
    for (i, (line, row)) in list.lines.into_iter().enumerate().skip(offset).take(height) {
        let y = area.y + u16::try_from(i - offset).unwrap_or(0);
        hits.extend(row.map(|c| (y, c)));
        lines.push(line);
    }
    lines.extend(detail.into_iter().map(|mut line| {
        line.spans.insert(0, Span::raw(" "));
        line
    }));
    let anchor = cursor
        .cloned()
        .zip(list.selected.map(|r| r.start.saturating_sub(offset)));
    Some(Laid {
        lines,
        hits,
        scroll: ListScroll { offset, anchor },
    })
}

pub(super) fn draw(frame: &mut Frame<'_>, m: &mut Model, area: Rect) {
    let Some(laid) = lay_out(m, area) else {
        return;
    };
    frame.render_widget(Paragraph::new(laid.lines), area);
    m.scroll = laid.scroll;
    m.areas.rows = laid.hits;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use agent_core::activity::State;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::fixtures::{asking, fleet, name, reported};
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

    /// Draws the whole screen, as the app does, and returns the sidebar's
    /// inner area.
    fn render(m: &mut Model, (width, height): (u16, u16)) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| super::super::draw(frame, m)).unwrap();
        terminal
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
    fn the_tier_gives_second_lines_while_the_detail_keeps_its_reserve() {
        assert_eq!(tier(Extent { rows: 19, live: 7 }, 47), Tier::Two);
        assert_eq!(tier(Extent { rows: 31, live: 10 }, 47), Tier::Two);
        assert_eq!(
            tier(Extent { rows: 31, live: 18 }, 47),
            Tier::Waiting { spare: 10 }
        );
        assert_eq!(tier(Extent { rows: 41, live: 3 }, 47), Tier::One);
        assert_eq!(tier(Extent { rows: 0, live: 0 }, 4), Tier::Two);
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
            ("Edit calc.ts".to_string(), Shown::Cut),
            "the basename leaves the rest to the lead"
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
        assert_eq!(delegating.line(Form::Short, 40).0, "↳1 · quiet 22s");
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
            Gist::Done(None).line(Form::Long, 40),
            ("done".to_string(), Shown::Whole)
        );
        assert_eq!(
            Gist::Failed(&Failure::RateLimit).line(Form::Short, 24),
            ("rate limited".to_string(), Shown::Whole)
        );
        let other = Failure::Other("server exploded\ntrace".into());
        assert_eq!(
            Gist::Failed(&other).line(Form::Long, 40),
            ("server exploded".to_string(), Shown::Cut)
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
    fn changed_files_line_up_their_counts_and_say_how_many_more() {
        let change = |path: &str, lines| agent_core::ports::Change {
            path: path.into(),
            lines,
        };
        let facts = WorktreeFacts {
            changed: vec![
                change(
                    "src/payroll/calc.ts",
                    Lines::Counted {
                        added: 98,
                        removed: 52,
                    },
                ),
                change(
                    "src/payroll/calc.test.ts",
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
        let item = |form, width, cap| files(&facts, form, width, cap).unwrap();
        assert_eq!(
            texts(&item(Form::Long, 34, 3).fit(5)),
            [
                "changed 4 files  +122 −52",
                "  src/payroll/calc.ts      +98 −52",
                "  src/payroll/calc.test.ts +24    ",
                "  logo.png                 bin    ",
                "  notes.md                 new    ",
            ],
            "one more file fits where `+1 more` would go"
        );
        assert_eq!(item(Form::Long, 34, 3).cap, 5);
        assert_eq!(item(Form::Long, 34, 3).fit(9).len(), 5, "every file");
        assert_eq!(
            texts(&item(Form::Long, 34, 3).fit(3))[1..],
            ["  src/payroll/calc.ts      +98 −52", "  +3 more"],
            "shrinks before it goes"
        );
        assert_eq!(
            texts(&item(Form::Long, 34, 3).fit(1)),
            ["changed 4 files  +122 −52"]
        );
        assert_eq!(
            texts(&item(Form::Short, 26, 1).fit(3))[1],
            "  calc.ts          +98 −52"
        );
        assert!(files(&WorktreeFacts::default(), Form::Long, 34, 3).is_none());
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

    fn block(name: &str, items: &[(usize, usize)]) -> Block {
        Block {
            header: Line::raw(format!("{name} ───")),
            items: items.iter().map(|(cap, full)| item(*cap, *full)).collect(),
        }
    }

    fn heads(lines: &[Line<'static>]) -> Vec<String> {
        texts(lines)
            .into_iter()
            .filter(|l| l.ends_with('─'))
            .collect()
    }

    #[test]
    fn the_selection_shrinks_items_in_priority_order_before_others_start() {
        let selected = || Some(block("sel", &[(3, 3), (4, 4), (1, 1)]));
        let others = || [block("a", &[(2, 2), (2, 2)]), block("b", &[(1, 1)])];
        assert_eq!(
            texts(&stack(selected(), others(), 7)),
            [
                "sel ───",
                "row 0",
                "row 1",
                "row 2",
                "row 0",
                "row 1",
                "row 2 …"
            ],
            "the second item shrinks and the third gets nothing, nor do others"
        );
        assert_eq!(
            heads(&stack(selected(), others(), 20)),
            ["sel ───", "a ───", "b ───"]
        );
        assert_eq!(
            heads(&stack(selected(), others(), 13)),
            ["sel ───", "a ───"],
            "a's second item shrinks, which ends the detail"
        );
        assert_eq!(
            heads(&stack(
                selected(),
                [block("a", &[(5, 5)]), block("b", &[(1, 1)])],
                12
            )),
            ["sel ───"],
            "b never jumps ahead of a"
        );
        assert_eq!(
            heads(&stack(None, [block("a", &[]), block("b", &[(1, 1)])], 4)),
            ["b ───"],
            "a block with nothing to say has no header"
        );
        assert_eq!(
            stack(
                Some(block("sel", &[(2, 6), (1, 1)])),
                [block("a", &[(1, 1)])],
                9
            )
            .len(),
            9,
            "spare rows grow the selection past its caps"
        );
    }

    fn agent_lines(m: &mut Model, agent: &str, size: (u16, u16)) -> Vec<Line<'static>> {
        render(m, size);
        let laid = lay_out(m, m.areas.sidebar).unwrap();
        laid.lines
            .into_iter()
            .filter(|l| text(l).contains(agent))
            .collect()
    }

    #[test]
    fn cards_show_failures_staleness_and_subagents() {
        let mut m = fleet(8);
        m.toggle_view();
        let mut activities = m.activities.clone().unwrap();
        activities.insert(
            name("payroll-fix-3"),
            reported(State::Working { tools: Vec::new() }, 400),
        );
        m.activities = Some(activities);
        let lines = texts(&laid(&mut m, (200, 50)));
        let failed = lines
            .iter()
            .position(|l| l.contains("! rate-limit-1"))
            .unwrap();
        assert_eq!(lines[failed + 1].trim(), "rate limited");
        let stale = agent_lines(&mut m, "payroll-fix-3", (200, 50));
        let time: Vec<_> = stale[0].spans.iter().rev().take(2).collect();
        assert_eq!(time[0].content, "?", "{:?}", stale[0]);
        assert_eq!(time[0].style.fg, Some(Color::Yellow));
        assert_eq!(time[1].content, "6m");
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let lines = texts(&laid(&mut m, (200, 50)));
        let tree: Vec<&String> = lines
            .iter()
            .filter(|l| l.trim_start().starts_with(['├', '└']))
            .collect();
        assert_eq!(tree.len(), 3, "{}", lines.join("\n"));
        assert!(
            tree[0].contains("general-purpose") && tree[0].matches("general-purpose").count() == 1,
            "the oldest first, and an empty description shows the kind once: {}",
            tree[0]
        );
        assert!(tree[2].trim_start().starts_with("└ "));
        assert!(tree[2].contains("Adversarial review:…  general-purpose  30s"));
    }

    fn laid(m: &mut Model, size: (u16, u16)) -> Vec<Line<'static>> {
        render(m, size);
        lay_out(m, m.areas.sidebar).unwrap().lines
    }

    #[test]
    fn the_agent_in_the_pane_says_so_instead_of_its_time() {
        let mut m = fleet(8);
        let agent = name("obs-layout-4");
        let ticket = m.ticket();
        m.pane = Pane::Connecting {
            name: agent.clone(),
            ticket,
        };
        m.set_cursor(Cursor::Entry(name("rate-limit-1")));
        let lines = texts(&laid(&mut m, (200, 50)));
        let header = lines.iter().find(|l| l.contains("obs-layout-4 ─")).unwrap();
        assert!(header.trim_end().ends_with("─ in pane"), "{header}");
        let selected = lines.iter().find(|l| l.contains("rate-limit-1 ─")).unwrap();
        assert!(
            selected.trim_end().ends_with('─'),
            "the selection's card shows its time, and Focus its repo: {selected}"
        );
    }

    #[test]
    fn readiness_shows_only_what_needs_doing() {
        let mut m = fleet(8);
        let lines = texts(&laid(&mut m, (200, 50)));
        assert!(
            !lines.iter().any(|l| l.contains("nothing ahead")),
            "{}",
            lines.join("\n")
        );
    }

    #[test]
    fn overlaps_keep_the_file_and_count_the_agents() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("rate-limit-1")));
        let lines = laid(&mut m, (200, 50));
        let overlap = lines.iter().find(|l| text(l).contains("overlap ")).unwrap();
        assert_eq!(text(overlap).trim(), "overlap flow.ts · auth-refresh-2 +2");
        let file = overlap
            .spans
            .iter()
            .find(|s| s.content == "flow.ts")
            .unwrap();
        assert_eq!(file.style.fg, Some(Color::Yellow));
        let lines = texts(&laid(&mut m, (80, 24)));
        assert!(
            lines
                .iter()
                .any(|l| l.trim() == "overlap flow.ts · auth-re…"),
            "{}",
            lines.join("\n")
        );
    }

    #[test]
    fn focus_lists_who_needs_you_in_other_repos() {
        let mut m = fleet(8);
        m.set_cursor(Cursor::Entry(name("agent-tui0")));
        let lines = texts(&laid(&mut m, (200, 50)));
        let header = lines.iter().position(|l| l.contains("Needs you")).unwrap();
        assert!(
            lines[header + 1].contains("◆ heucristic0"),
            "{}",
            lines.join("\n")
        );
        assert!(
            lines[header + 2].contains("rate-limit-1") && lines[header + 2].contains("heucristic0")
        );
        assert!(!lines[header + 3].starts_with("    "), "one line each");
        let items = m.items();
        assert!(
            !items
                .iter()
                .any(|c| c.entry_name() == Some(&name("rate-limit-1"))),
            "they can't be selected in Focus"
        );
    }

    #[test]
    fn every_size_and_fleet_fits_its_panel() {
        for size in SIZES {
            for agents in FLEETS {
                for view in [View::Focus, View::Overview] {
                    let mut m = fleet(agents);
                    if m.view != view {
                        m.toggle_view();
                    }
                    let terminal = render(&mut m, size);
                    let area = m.areas.sidebar;
                    let laid = lay_out(&m, area);
                    let context = format!("{size:?}, {agents} agents, {view:?}");
                    let Some(laid) = laid else {
                        panic!("{context}: nothing laid out");
                    };
                    assert!(
                        laid.lines.len() <= usize::from(area.height),
                        "{context}: {} lines in {} rows",
                        laid.lines.len(),
                        area.height
                    );
                    for line in &laid.lines {
                        assert!(
                            line.width() < usize::from(area.width),
                            "{context}: {:?} is wider than {}",
                            text(line),
                            area.width - 1
                        );
                    }
                    let buffer = terminal.backend().buffer();
                    let border = area.x + area.width;
                    for y in area.y..area.y + area.height {
                        assert_eq!(buffer[(border, y)].symbol(), "│", "{context}: row {y}");
                    }
                    let lines = texts(&laid.lines);
                    if agents == 0 {
                        assert!(lines.is_empty(), "{context}");
                    } else {
                        assert!(
                            lines.iter().any(|l| l.starts_with('▌')),
                            "{context}: the selection shows\n{}",
                            lines.join("\n")
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
                    let after = heights(&m);
                    for (cursor, height) in &after {
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
            let rows = usize::from(height) - 3;
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

    #[test]
    fn the_tier_follows_the_agents_and_the_view() {
        let rows = 47;
        let mut m = fleet(8);
        let focus = tier(m.extent(), rows);
        m.toggle_view();
        assert_eq!(m.extent(), Extent { rows: 11, live: 7 });
        assert_eq!(focus, Tier::Two);
        assert_eq!(
            fleet(21).extent(),
            Extent { rows: 22, live: 3 },
            "Focus: its 4 entries, the Needs you header, and the 17 elsewhere"
        );
        let mut many = fleet(30);
        many.toggle_view();
        assert_eq!(tier(many.extent(), rows), Tier::Waiting { spare: 4 });
        let mut more = fleet(40);
        more.toggle_view();
        assert_eq!(tier(more.extent(), rows), Tier::One);
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
    fn the_selected_block_follows_the_list_and_others_follow_in_order() {
        let mut m = fleet(8);
        m.toggle_view();
        render(&mut m, (200, 50));
        let laid = lay_out(&m, m.areas.sidebar).unwrap();
        let lines = texts(&laid.lines);
        let list_end = lines
            .iter()
            .position(|l| l.contains('─'))
            .expect("a detail block");
        assert!(
            lines[list_end].contains(m.cursor().and_then(Cursor::entry_name).unwrap().as_str()),
            "the selection's block comes first, right under the list\n{}",
            lines.join("\n")
        );
        assert!(
            lines.iter().any(|l| l.contains("› go")),
            "{}",
            lines.join("\n")
        );
    }

    #[test]
    fn an_agent_starting_to_ask_opens_and_closes_no_repos() {
        let mut m = fleet(40);
        m.view = View::Overview;
        render(&mut m, (100, 30));
        let folds = |m: &Model| -> Vec<String> {
            m.rows()
                .iter()
                .filter_map(|row| match row {
                    Row::Repo { key, fold } => Some(format!("{key:?} {fold:?}")),
                    _ => None,
                })
                .collect()
        };
        let quiet = m
            .snapshot()
            .unwrap()
            .entries()
            .map(|(_, e)| e.clone())
            .filter(|e| m.status(e) == AgentStatus::Idle)
            .last()
            .unwrap();
        m.set_cursor(Cursor::Entry(quiet.name().clone()));
        render(&mut m, (100, 30));
        let before = folds(&m);
        let mut activities = m.activities.clone().unwrap_or_default();
        activities.insert(quiet.name().clone(), asking("rm?", 0));
        crate::update::update(&mut m, crate::worker::Msg::Activities(activities));
        render(&mut m, (100, 30));
        assert_eq!(folds(&m), before);
    }

    #[test]
    fn focus_keeps_the_detail_reserve_whatever_waits_elsewhere() {
        let mut m = fleet(40);
        m.view = View::Focus;
        let extent = m.extent();
        assert!(
            extent.rows
                >= 1 + m.snapshot().unwrap().entries().count()
                    - m.selected().unwrap().0.entries().len(),
            "every other repo's agent may wait: {extent:?}"
        );
    }
}
