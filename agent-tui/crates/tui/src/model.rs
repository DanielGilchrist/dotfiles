use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use agent_core::actions::Actions;
use agent_core::activity::{Activity, AgentId, State as ActivityState, Subagent};
use agent_core::config::Keys;
use agent_core::model::{
    Entry, EntryKind, EntryState, Removable, Repo, RepoKey, Revival, Running, Snapshot, Startable,
};
use agent_core::names::{AgentName, BranchName, Checkout, Prompt, SessionName};
use agent_core::ops::{Approved, Hazardous, RemovalPlan};
use agent_core::paths::{RepoRoot, Typed, typed_path};
use agent_core::ports::{Change, FileState, HarnessId, Readiness, Uncommitted, WorktreeFacts};
use agent_term::{Emulator, Terminal};
use ratatui::layout::Rect;

use crate::document::{BranchKey, CommitKey, DiffKey, Documents, FolderKey, Hover, Key};
use crate::palette::Palette;
use crate::track::{CachedTree, LiveSince};
pub use crate::track::{Outbox, Tracked, Watched};
use crate::tree::{self, ChangesScope, ChangesView, Node, TreeRow};

pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(150);
pub const INFO_TTL: Duration = Duration::from_secs(4);

/// How long the prompt must be still before the model is asked.
pub const NAMING_DEBOUNCE: Duration = Duration::from_millis(900);
/// Prompts this short are named from their words; asking isn't worth it.
const NAMING_MIN_WORDS: usize = 3;

/// The sidebar panel `j`/`k` act on in NAV, and how far it has moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Panel {
    /// [1]: they move the selection.
    Agents,
    /// [2]: they scroll the selected agent's details down `offset` rows
    /// and move the cursor over the turn and its subagents.
    Agent { offset: usize, row: TurnRow },
    /// [3]: they move the cursor over the selected agent's changed files.
    Changes { file: usize },
    /// The keys are in the document [0] shows for the list panel it came
    /// from, whose cursor still says which document.
    Main(Origin),
}

/// The list panel a focused document in [0] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Turn { offset: usize, row: TurnRow },
    Changes { file: usize },
}

/// The row [2]'s cursor is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnRow {
    Turn,
    /// A subagent of the agent `by`.
    Subagent {
        by: SessionName,
        id: AgentId,
    },
}

/// The list panel the keys act on, whether they're in it or in the document
/// [0] shows for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listed<'a> {
    Agents,
    Agent { offset: usize, row: &'a TurnRow },
    Changes { file: usize },
}

impl Origin {
    fn into_panel(self) -> Panel {
        match self {
            Self::Turn { offset, row } => Panel::Agent { offset, row },
            Self::Changes { file } => Panel::Changes { file },
        }
    }
}

impl Panel {
    /// [2] with the cursor on the turn, scrolled to the top.
    pub const fn turn() -> Self {
        Self::Agent {
            offset: 0,
            row: TurnRow::Turn,
        }
    }

    pub fn list(&self) -> Listed<'_> {
        match self {
            Self::Agents => Listed::Agents,
            Self::Agent { offset, row } | Self::Main(Origin::Turn { offset, row }) => {
                Listed::Agent {
                    offset: *offset,
                    row,
                }
            }
            Self::Changes { file } | Self::Main(Origin::Changes { file }) => {
                Listed::Changes { file: *file }
            }
        }
    }

    /// Takes the focus from the document, leaving the list panel it came
    /// from.
    pub fn leave_document(&mut self) {
        if let Self::Main(_) = self
            && let Self::Main(from) = std::mem::replace(self, Self::Agents)
        {
            *self = from.into_panel();
        }
    }

    fn origin(self) -> Option<Origin> {
        match self {
            Self::Agent { offset, row } | Self::Main(Origin::Turn { offset, row }) => {
                Some(Origin::Turn { offset, row })
            }
            Self::Changes { file } | Self::Main(Origin::Changes { file }) => {
                Some(Origin::Changes { file })
            }
            Self::Agents => None,
        }
    }

    /// This panel after a layout settled `list` as its list panel: the
    /// focus on its document survives when the list panel does.
    pub fn with_list(self, list: Self) -> Self {
        match (self, list.origin()) {
            (Self::Main(_), Some(from)) => Self::Main(from),
            (_, from) => from.map_or(Self::Agents, Origin::into_panel),
        }
    }

    /// Puts the keys in the panel's document. [1] has none.
    pub fn focus_document(&mut self) {
        let from = match std::mem::replace(self, Self::Agents) {
            Self::Agent { offset, row } => Origin::Turn { offset, row },
            Self::Changes { file } => Origin::Changes { file },
            panel => {
                *self = panel;
                return;
            }
        };
        *self = Self::Main(from);
    }

    pub fn in_main(&self) -> bool {
        matches!(self, Self::Main(_))
    }

    /// The row of [2]'s cursor, while [2] or its document has the keys.
    pub fn turn_row(&self) -> Option<&TurnRow> {
        match self.list() {
            Listed::Agent { row, .. } => Some(row),
            Listed::Agents | Listed::Changes { .. } => None,
        }
    }

    fn turn_row_mut(&mut self) -> Option<&mut TurnRow> {
        match self {
            Self::Agent { row, .. } | Self::Main(Origin::Turn { row, .. }) => Some(row),
            Self::Agents | Self::Changes { .. } | Self::Main(Origin::Changes { .. }) => None,
        }
    }
}

/// Whether the column is tall enough for [3] at all. Until a draw says
/// otherwise there's room, so keys aren't refused on a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkRoom {
    Fits,
    TooShort,
}

/// How the screen is shared between the sidebar and the main view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    /// The sidebar beside [0].
    #[default]
    Split,
    /// [0] takes the width. Keys still go to the focused panel.
    Full,
}

/// Only an agent worktree's commits are its own work, so any other
/// checkout shows readiness only while it has uncommitted files. `nothing
/// ahead` is news only once the agent says it's done.
pub fn shows_readiness(entry: &Entry, status: &AgentStatus, readiness: Readiness) -> bool {
    match (readiness, entry.kind()) {
        (Readiness::NothingAhead, EntryKind::Agent { .. }) => {
            matches!(status, AgentStatus::Done { .. })
        }
        (Readiness::Dirty { .. }, _)
        | (Readiness::Behind { .. } | Readiness::Ready { .. }, EntryKind::Agent { .. }) => true,
        (_, EntryKind::Main { .. } | EntryKind::Adopted { .. } | EntryKind::Orphan { .. }) => false,
    }
}

/// What a move past the end of a list does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    /// Continues from the other end.
    Around,
    /// Stops at the end.
    Clamp,
}

/// Which way a cycle goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Every repo.
    Overview,
    /// One repo.
    Focus,
    /// The pinned agents.
    Pinned,
}

impl View {
    pub const ALL: [Self; 3] = [Self::Overview, Self::Focus, Self::Pinned];

    fn index(self) -> usize {
        Self::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }

    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// What [3]'s cursor is on, by identity so a refresh can't move it to
/// another row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowId {
    Branch,
    Path(PathBuf),
}

/// What the sidebar selection points at, by identity so it survives refreshes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cursor {
    /// A repo row; only exists in Overview.
    Repo(RepoKey),
    /// An entry where its repo lists it, or a pin in the Pinned tab.
    Entry(SessionName),
    /// The copy of an entry in a group: Overview's and Pinned's Needs you.
    Grouped(Group, SessionName),
}

/// A group of agents listed above or below the repos. Selecting a copy
/// there doesn't expand its repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Agents asking something, failed, or done and not yet seen.
    NeedsYou,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
        }
    }
}

impl Cursor {
    /// Whether `other` selects the same thing, counting an agent moving
    /// into or out of Needs you as staying put.
    pub fn follows(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Grouped(Group::NeedsYou, agent), elsewhere)
            | (elsewhere, Self::Grouped(Group::NeedsYou, agent)) => {
                elsewhere.entry_name() == Some(agent)
            }
            _ => self == other,
        }
    }

    pub fn entry_name(&self) -> Option<&SessionName> {
        match self {
            Self::Entry(name) | Self::Grouped(_, name) => Some(name),
            Self::Repo(_) => None,
        }
    }

    pub fn repo<'a>(&'a self, snapshot: &'a Snapshot) -> Option<&'a Repo> {
        match self {
            Self::Repo(key) => snapshot.repo(key),
            Self::Entry(name) | Self::Grouped(_, name) => {
                snapshot.entry(name).map(|(repo, _)| repo)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Header(Group),
    /// A repo with more than one entry. Its entries follow when it's open.
    Repo {
        key: RepoKey,
        fold: Fold,
    },
    /// An agent in a group: Overview's or Pinned's Needs you.
    Grouped(Group, SessionName),
    /// An agent in another repo that needs you, listed under Focus's repo
    /// so it isn't missed. Selecting it would change the repo in focus, so
    /// it can't be selected: `a` goes there.
    Elsewhere(SessionName),
    Entry {
        name: SessionName,
        place: Place,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Open,
    Closed,
}

/// How an entry row sits in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Focus: the whole list is its repo.
    Alone,
    /// Under its open repo row.
    Nested,
    /// Its repo's only entry, standing in for the repo row.
    Solo,
    /// In the Pinned tab, with its repo on the right.
    Pinned,
}

impl Row {
    /// What selecting this row selects; group headers select nothing.
    pub fn cursor(&self) -> Option<Cursor> {
        match self {
            Self::Header(_) | Self::Elsewhere(_) => None,
            Self::Repo { key, .. } => Some(Cursor::Repo(key.clone())),
            Self::Grouped(group, name) => Some(Cursor::Grouped(*group, name.clone())),
            Self::Entry { name, .. } => Some(Cursor::Entry(name.clone())),
        }
    }
}

/// A repo's only entry: such a repo is listed as that entry, with no repo
/// row.
fn sole(repo: &Repo) -> Option<&Entry> {
    match repo.entries() {
        [only] => Some(only),
        _ => None,
    }
}

/// Which repos stay open when they don't all fit: those nearest the
/// selection, while their entries fit in `spare` rows. `sizes` are the
/// rows each repo's entries take, `origin` is where the selection is (0
/// above the repos, `i + 1` in repo `i`), and the repo holding it always
/// opens.
fn folds(sizes: &[usize], origin: usize, spare: usize) -> Vec<Fold> {
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by_key(|&i| (i + 1).abs_diff(origin));
    let mut folds = vec![Fold::Closed; sizes.len()];
    let mut left = spare;
    for i in order {
        let holds_selection = i + 1 == origin;
        if sizes[i] > left && !holds_selection {
            break;
        }
        left = left.saturating_sub(sizes[i]);
        folds[i] = Fold::Open;
    }
    folds
}

/// Where keystrokes go while connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Agent,
    Nav,
}

/// Whether the escape chord was the last key, so pressing it again jumps to
/// the previous agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chord {
    Idle,
    Armed,
}

/// Identifies one request to connect, so a late reply to an abandoned
/// request can't replace the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Show it in the pane; keys stay in the sidebar.
    Preview,
    /// Show it and type into it.
    Activate,
}

pub struct Connection {
    pub name: SessionName,
    pub term: Terminal<Box<dyn Emulator>>,
    /// Other clients attached to the same session.
    pub others: Option<NonZeroUsize>,
    pub mode: Mode,
    pub opened: Instant,
}

impl Connection {
    /// Other clients attached, if this connection is to `name`.
    pub fn sharing(&self, name: &SessionName) -> Option<NonZeroUsize> {
        if &self.name == name {
            self.others
        } else {
            None
        }
    }
}

/// The right-hand pane. Only a live connection takes keyboard input, so
/// AGENT mode can't exist without one.
pub enum Pane {
    Empty,
    Connecting { name: SessionName, ticket: Ticket },
    Connected(Connection),
}

impl Pane {
    pub fn connection(&self) -> Option<&Connection> {
        match self {
            Self::Connected(c) => Some(c),
            _ => None,
        }
    }

    pub fn connection_mut(&mut self) -> Option<&mut Connection> {
        match self {
            Self::Connected(c) => Some(c),
            _ => None,
        }
    }

    pub fn session(&self) -> Option<&SessionName> {
        match self {
            Self::Empty => None,
            Self::Connecting { name, .. } => Some(name),
            Self::Connected(c) => Some(&c.name),
        }
    }

    /// The session the pane is still waiting to connect to for `ticket`.
    pub fn awaiting(&self, ticket: Ticket) -> Option<&SessionName> {
        match self {
            Self::Connecting {
                name,
                ticket: pending,
            } if *pending == ticket => Some(name),
            _ => None,
        }
    }

    /// Stops waiting for `ticket`'s connection, if the pane still is.
    pub fn abandon(&mut self, ticket: Ticket) {
        if let Self::Connecting {
            ticket: pending, ..
        } = self
            && *pending == ticket
        {
            *self = Self::Empty;
        }
    }
}

/// Where keys go. Overlays take them first, then an agent in AGENT mode.
pub enum Focus<'a> {
    Overlay(&'a Overlay),
    Agent(&'a Connection),
    Nav(Option<&'a Connection>),
}

/// What a row says about an agent: its session and, while that's live,
/// what its harness reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    NoSession,
    Exited,
    /// Live, with nothing to report: waiting for a prompt, seen, or a
    /// harness without hooks.
    Idle,
    Working {
        doing: Doing,
        since: SystemTime,
        pulse: Pulse,
    },
    Asking {
        question: String,
        since: SystemTime,
    },
    Failed {
        failure: Failure,
        since: SystemTime,
    },
    /// Finished a turn the user hasn't looked at.
    Done {
        summary: Option<String>,
        since: SystemTime,
    },
}

/// When an agent's git facts were asked for, and how many events it had
/// reported then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Inspection {
    pub events: Option<u64>,
    pub at: Instant,
}

/// Why a turn failed, as far as the harness's reason says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    RateLimit,
    Overloaded,
    Billing,
    Other(String),
}

impl Failure {
    fn of(reason: &str) -> Self {
        let lower = reason.to_lowercase();
        if lower.contains("rate limit") {
            Self::RateLimit
        } else if lower.contains("overload") {
            Self::Overloaded
        } else if lower.contains("billing") {
            Self::Billing
        } else {
            Self::Other(reason.to_string())
        }
    }
}

/// What a tool acts on. A path keeps its end when cut, anything else its
/// start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Path(String),
    Text(String),
}

impl Target {
    fn of(target: &str) -> Self {
        if target.contains('/') && !target.contains(char::is_whitespace) {
            Self::Path(target.to_string())
        } else {
            Self::Text(target.to_string())
        }
    }
}

/// What a working agent is busy with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Doing {
    Thinking,
    Tool {
        name: String,
        target: Option<Target>,
    },
    /// Its turn ended and it waits on this many background subagents.
    Delegating(NonZeroUsize),
    /// Its subagents finished and it's about to take their results.
    HandingBack,
}

/// Why an agent needs the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    Asking,
    Failed,
    Done,
}

impl Waiting {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Asking => "◆",
            Self::Failed => "!",
            Self::Done => "✓",
        }
    }
}

/// How many agents need the user, by kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attention {
    pub asking: usize,
    pub failed: usize,
    pub done: usize,
}

impl Attention {
    /// The non-zero counts, most urgent first.
    pub fn parts(self) -> Vec<(Waiting, usize)> {
        [
            (Waiting::Asking, self.asking),
            (Waiting::Failed, self.failed),
            (Waiting::Done, self.done),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .collect()
    }

    /// The terminal title: counts first, so a narrow tab still shows them.
    pub fn title(self) -> String {
        let counts: Vec<String> = self
            .parts()
            .iter()
            .map(|(kind, n)| format!("{}{n}", kind.glyph()))
            .collect();
        match counts.as_slice() {
            [] => "agent".into(),
            _ => format!("{} · agent", counts.join(" ")),
        }
    }
}

/// Whether the terminal the TUI runs in has focus. Terminals that never
/// report it count as focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalFocus {
    Focused,
    Unfocused,
}

/// Whether a working agent is still being heard from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pulse {
    Steady,
    /// Nothing heard for a while with no tool running: interrupted or stuck.
    Stale,
}

/// How urgently an agent needs the user, most urgent first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Urgency {
    Blocked,
    Unseen,
}

impl AgentStatus {
    /// Whether, and how urgently, it needs the user, and since when.
    pub fn urgency(&self) -> Option<(Urgency, SystemTime)> {
        match self {
            Self::Asking { since, .. } | Self::Failed { since, .. } => {
                Some((Urgency::Blocked, *since))
            }
            Self::Done { since, .. } => Some((Urgency::Unseen, *since)),
            Self::NoSession | Self::Exited | Self::Idle | Self::Working { .. } => None,
        }
    }
}

/// What this TUI can do with an entry, worked out in one place so the keys
/// and the hints agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// The session this TUI runs in: nothing here can act on it.
    Own,
    Other {
        enter: Enter,
        kill: Option<Running>,
        remove: Option<Removable>,
    },
}

/// What ⏎ does to an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enter {
    Attach,
    /// Start an exited agent again through `agent`, resuming its
    /// conversation.
    Restart(Startable),
    /// Let zellij replay a session we don't own the checkout of.
    Resurrect,
    Start(Startable),
}

impl Enter {
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Attach => "attach",
            Self::Restart(_) | Self::Resurrect => "resurrect",
            Self::Start(_) => "start",
        }
    }
}

/// A filter and a selection within its matches. `offset` is view state:
/// the first visible row, kept stable while the selection moves.
#[derive(Debug, Default, Clone)]
pub struct Query {
    pub text: String,
    index: usize,
    pub offset: std::cell::Cell<usize>,
}

impl Query {
    pub fn index(&self, len: usize) -> usize {
        self.index.min(len.saturating_sub(1))
    }

    pub fn pick<'a, T>(&self, items: &'a [T]) -> Option<&'a T> {
        items.get(self.index(items.len()))
    }

    pub fn step(&mut self, delta: isize, len: usize) {
        let current = self.index(len);
        self.index = current
            .saturating_add_signed(delta)
            .min(len.saturating_sub(1));
    }

    pub fn push(&mut self, c: char) {
        self.text.push(c);
        self.index = 0;
    }

    pub fn pop(&mut self) {
        self.text.pop();
        self.index = 0;
    }

    pub fn set_text(&mut self, text: String) {
        self.text = text;
        self.index = 0;
    }
}

pub enum Confirm {
    Kill(Running),
    /// Quitting would lose this many queued messages.
    Quit(usize),
    Remove(Proposal),
}

/// A removal waiting for the user's answer.
pub enum Proposal {
    /// Nothing would be lost.
    Clean(Approved),
    /// Something would be lost, so only discarding it goes ahead.
    Hazardous(Hazardous),
}

/// What an answer to a [`Proposal`] does.
pub enum Answer {
    Remove(Approved),
    /// Stop the session and keep everything else.
    KillOnly(Running),
}

/// A key a [`Proposal`] offers.
pub struct Offer {
    pub key: char,
    pub label: &'static str,
    pub effect: OfferEffect,
}

/// Stops the session and keeps everything else, when it's running.
const KILL_ONLY: char = 'd';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferEffect {
    Destroys,
    Keeps,
}

impl From<RemovalPlan> for Proposal {
    fn from(plan: RemovalPlan) -> Self {
        match plan.approve() {
            Ok(approved) => Self::Clean(approved),
            Err(hazardous) => Self::Hazardous(hazardous),
        }
    }
}

impl Proposal {
    pub fn plan(&self) -> &RemovalPlan {
        match self {
            Self::Clean(approved) => approved.plan(),
            Self::Hazardous(hazardous) => hazardous.plan(),
        }
    }

    /// The key that goes ahead with removing, and what it's called.
    fn primary(&self) -> (char, &'static str) {
        match self {
            Self::Clean(_) => ('y', "remove"),
            Self::Hazardous(_) => ('Y', "discard and remove"),
        }
    }

    /// The keys on offer, in display order. [`Proposal::answer`] accepts
    /// exactly these.
    pub fn offers(&self) -> Vec<Offer> {
        let (key, label) = self.primary();
        let remove = Offer {
            key,
            label,
            effect: OfferEffect::Destroys,
        };
        let kill = self.plan().running().map(|_| Offer {
            key: KILL_ONLY,
            label: "kill only",
            effect: OfferEffect::Keeps,
        });
        std::iter::once(remove).chain(kill).collect()
    }

    /// What `key` does, or the proposal back if it isn't on offer.
    #[allow(clippy::result_large_err)]
    pub fn answer(self, key: char) -> Result<Answer, Self> {
        if key == self.primary().0 {
            return Ok(Answer::Remove(match self {
                Self::Clean(approved) => approved,
                Self::Hazardous(hazardous) => hazardous.force(),
            }));
        }
        match (key, self.plan().running().cloned()) {
            (KILL_ONLY, Some(running)) => Ok(Answer::KillOnly(running)),
            _ => Err(self),
        }
    }
}

/// Why `t` can't message an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreachable {
    NotRunning,
    Own,
    /// No hook activity: when it's at rest can't be known.
    Unreported,
}

/// The keybindings list: what's typed to narrow it, the row under the
/// cursor and, as view state, the first visible line.
#[derive(Debug, Default, Clone)]
pub struct HelpState {
    pub filter: String,
    /// `j` and `k` move until typing has started, then they are letters.
    pub typing: bool,
    pub selected: usize,
    pub offset: std::cell::Cell<usize>,
}

pub enum Overlay {
    None,
    New(Box<NewAgent>),
    Switcher(Query),
    Picker(Query),
    Confirm(Confirm),
    Help(HelpState),
    /// The configured actions, to run on this agent.
    Actions(SessionName),
    /// A message being typed for an agent.
    Talk {
        to: SessionName,
        text: String,
    },
    /// A comment being typed on lines of a diff, for an agent's draft review.
    Comment {
        to: SessionName,
        lines: Vec<crate::review::LineInfo>,
        text: String,
        /// The commit the lines are from, when they aren't the checkout's.
        rev: Option<String>,
    },
    /// Working out what removing `name` would lose; the confirmation
    /// replaces this when the answer arrives.
    Planning {
        ticket: Ticket,
        name: SessionName,
    },
}

/// Why the model wants an agent selected when it next appears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Follow {
    /// What the form launched: its launch connects the pane itself.
    Submitted { name: SessionName, ticket: Ticket },
    /// A launch sent to the background with Esc: preview it once it shows.
    Backgrounded { name: SessionName, ticket: Ticket },
    /// The selection from the last run, previewed like any selection. Only
    /// the first snapshot can honour it, and starting inside an agent's
    /// checkout overrides it.
    Restored(SessionName),
    /// A repo just listed from the picker: focus it once it shows. A
    /// refresh was already in flight, so it gets two snapshots to show in
    /// before the follow is dropped (the listing may be hidden by a name
    /// clash).
    Repo { root: RepoRoot, until: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Error,
}

pub struct Status {
    pub text: String,
    pub kind: StatusKind,
    pub at: Instant,
}

/// Screen regions from the last draw, for mouse hit-testing, pty sizing
/// and how many repos Overview can keep open.
#[derive(Default)]
pub struct Areas {
    pub terminal: Rect,
    /// Inside the sidebar's border, empty while it's hidden.
    pub sidebar: Rect,
    pub rows: Vec<(u16, Cursor)>,
    /// [2]'s and [3]'s panels as drawn, and the screen row of each of their
    /// list rows with its index, so a click lands on what it points at.
    pub agent_panel: Rect,
    pub agent_rows: Vec<(u16, usize)>,
    pub changes_panel: Rect,
    pub change_rows: Vec<(u16, usize)>,
    /// Rows inside the panel the keys go to, as last drawn, so a half-page
    /// move knows how far that is.
    pub page: usize,
}

/// Where the sidebar list was scrolled to, and where the selection sat on
/// screen, as last drawn.
#[derive(Debug, Default)]
pub struct ListScroll {
    pub offset: usize,
    /// The selection and its line on screen.
    pub anchor: Option<(Cursor, usize)>,
}

/// The sidebar list at its tallest for the current agents and view: every
/// card one line, every repo open, and every place an agent could be listed
/// counted, so agents changing state never change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Extent {
    pub rows: usize,
    /// The cards whose agent has a session, which may take a second line.
    pub live: usize,
}

/// Which checkouts [1]'s repo and agents tabs list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Those with a live session, and the selected one.
    #[default]
    Active,
    All,
}

impl Visibility {
    /// Whether `entry` is listed on its own account.
    pub fn lists(self, entry: &Entry) -> bool {
        match self {
            Self::All => true,
            Self::Active => matches!(entry.state(), EntryState::Live),
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Active => Self::All,
            Self::All => Self::Active,
        }
    }
}

/// The entry selecting `repo` lands on: its first listed one, else its main
/// checkout.
fn lead(visibility: Visibility, repo: &Repo) -> &Entry {
    repo.entries()
        .iter()
        .find(|e| visibility.lists(e))
        .unwrap_or_else(|| repo.first())
}

/// When a subagent's transcript was last asked for, and how many events its
/// agent had reported then.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentRead {
    pub events: u64,
    pub at: Instant,
}

/// What [0] opened from the branch view: a commit's diff, or the diff of an
/// uncommitted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    Commit { sha: String, subject: String },
    File { path: PathBuf, state: FileState },
}

/// A subagent by its agent and its id.
pub type SubagentKey = (SessionName, AgentId);

pub enum Load {
    Pending,
    Ready {
        snapshot: Snapshot,
        cursor: Option<Cursor>,
    },
}

pub struct Model {
    pub load: Load,
    pub view: View,
    pub pane: Pane,
    pub overlay: Overlay,
    /// Most recently used first; no duplicates.
    mru: Vec<SessionName>,
    pub status: Option<Status>,
    pub preview_at: Option<Instant>,
    pub areas: Areas,
    pub start_dir: PathBuf,
    pub own_session: Option<SessionName>,
    pub harness: HarnessId,
    pub harness_label: String,
    pub last_warnings: Vec<String>,
    pub chord: Chord,
    pub scroll: ListScroll,
    pub panel: Panel,
    /// Whether the column had room for [3] when last drawn, so `3` can
    /// say why it did nothing.
    pub work_room: WorkRoom,
    /// An agent to select once a refresh shows it.
    pub follow: Option<Follow>,
    /// Agents the form is launching, by the launch's ticket. A new
    /// worktree shows up before its session has started, and a second
    /// start from `⏎` would race the first and skip its prompt.
    pub launching: Vec<(Ticket, SessionName)>,
    /// What was selected before a tab that doesn't list it was cycled
    /// to, restored by the next tab that does unless you moved meanwhile.
    displaced: Option<Cursor>,
    next_ticket: u64,
    pub actions: Actions,
    pub keys: Keys,
    pub palette: Palette,
    /// Repos found under `[repos] dirs`, by name, for the pickers to offer
    /// beyond the snapshot.
    pub catalogue: Vec<RepoRoot>,
    /// What couldn't be read under `[repos] dirs`, as last reported.
    pub catalogue_warnings: Vec<String>,
    /// How many snapshots have loaded, to give a follow a deadline.
    pub snapshots: u64,
    /// What each agent's harness last reported, once the first report of
    /// them has arrived.
    pub activities: Option<HashMap<SessionName, Activity>>,
    /// The clock for this frame, so every row, count and title agrees.
    pub now: SystemTime,
    /// How [3] lists the changed files, for the whole session.
    pub changes_view: ChangesView,
    pub changes_scope: ChangesScope,
    /// What is known about each agent, dropped when it leaves the snapshot.
    pub agents: HashMap<SessionName, Tracked>,
    /// The finished turn last marked seen, so it's marked once.
    pub acknowledged: Option<(SessionName, u64)>,
    pub last_bell: Option<Instant>,
    /// A bell held back by the rate limit, to ring when it allows.
    pub bell_due: Option<Instant>,
    pub terminal: TerminalFocus,
    /// Which checkouts [1] lists.
    pub visibility: Visibility,
    /// How many times any agent's git facts have been read, so a read's
    /// generation never repeats across agents that come and go.
    facts_reads: u64,
    /// [0]'s subagent log: layout flags.
    pub log_view: crate::log::LogView,
    /// [0]'s diff: the cursor over its lines.
    pub diff_view: crate::review::DiffView,
    /// What the branch view opened in [0], until `Esc` goes back to it.
    pub opened: Option<Opened>,
    /// The agent whose terminal the peek float shows.
    pub peek: Option<SessionName>,
    /// What the main view [0] reads when it isn't the terminal.
    pub docs: Documents,
    pub screen: Screen,
    /// Whether the sidebar was drawn last frame: not in the full screen
    /// mode, nor below the width it needs.
    pub sidebar_shown: bool,
}

impl Model {
    pub fn new(
        start_dir: PathBuf,
        own_session: Option<SessionName>,
        harness: HarnessId,
        harness_label: String,
    ) -> Self {
        Self {
            load: Load::Pending,
            view: View::Overview,
            pane: Pane::Empty,
            overlay: Overlay::None,
            mru: Vec::new(),
            status: None,
            preview_at: None,
            areas: Areas::default(),
            start_dir,
            own_session,
            harness,
            harness_label,
            last_warnings: Vec::new(),
            chord: Chord::Idle,
            scroll: ListScroll::default(),
            panel: Panel::Agents,
            work_room: WorkRoom::Fits,
            follow: None,
            launching: Vec::new(),
            displaced: None,
            next_ticket: 0,
            actions: Actions::default(),
            keys: Keys::default(),
            palette: Palette::DEFAULT,
            catalogue: Vec::new(),
            catalogue_warnings: Vec::new(),
            snapshots: 0,
            activities: None,
            now: SystemTime::now(),
            agents: HashMap::new(),
            changes_view: ChangesView::default(),
            changes_scope: ChangesScope::default(),
            acknowledged: None,
            last_bell: None,
            bell_due: None,
            terminal: TerminalFocus::Focused,
            visibility: Visibility::Active,
            facts_reads: 0,
            log_view: crate::log::LogView::default(),
            diff_view: crate::review::DiffView::default(),
            opened: None,
            peek: None,
            docs: Documents::default(),
            screen: Screen::Split,
            sidebar_shown: true,
        }
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        match &self.load {
            Load::Ready { snapshot, .. } => Some(snapshot),
            Load::Pending => None,
        }
    }

    pub fn cursor(&self) -> Option<&Cursor> {
        match &self.load {
            Load::Ready { cursor, .. } => cursor.as_ref(),
            Load::Pending => None,
        }
    }

    pub fn ticket(&mut self) -> Ticket {
        self.next_ticket += 1;
        Ticket(self.next_ticket)
    }

    pub fn mru(&self) -> &[SessionName] {
        &self.mru
    }

    pub fn touch_mru(&mut self, name: &SessionName) {
        self.mru.retain(|n| n != name);
        self.mru.insert(0, name.clone());
    }

    pub fn selected(&self) -> Option<(&Repo, Option<&Entry>)> {
        let snapshot = self.snapshot()?;
        let cursor = self.cursor()?;
        let repo = cursor.repo(snapshot)?;
        Some((repo, cursor.entry_name().and_then(|n| repo.entry(n))))
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.selected().and_then(|(_, e)| e)
    }

    /// The selected agent's files in the scope [3] shows, as last read.
    /// The changed files under `folder` in the order [3]'s tree lists them:
    /// folders first, then files by name.
    pub fn folder_files(&self, folder: &Path) -> Vec<Change> {
        let under: Vec<Change> = self
            .changes()
            .iter()
            .filter(|c| c.path.starts_with(folder))
            .cloned()
            .collect();
        crate::tree::build(&under, crate::tree::ChangesView::Tree, &HashSet::new())
            .into_iter()
            .filter_map(|row| match row.node {
                crate::tree::Node::File(i) => under.get(i).cloned(),
                crate::tree::Node::Folder { .. } => None,
            })
            .collect()
    }

    pub fn changes(&self) -> &[Change] {
        match self.selected_entry().and_then(|e| self.facts(e.name())) {
            Some(Ok(facts)) => self.changes_scope.of(facts),
            Some(Err(_)) | None => &[],
        }
    }

    /// Moves within the focused panel: the selection in [1], the scroll in
    /// [2], the file cursor in [3]. [2] is clamped when it's drawn, since
    /// only the view knows how long it is.
    /// One row at a time wraps around the list. A page at a time stops at
    /// the ends, since it's for reading through, not cycling.
    pub fn move_in_panel(&mut self, delta: isize) {
        self.move_in_panel_by(delta, Wrap::Around);
    }

    pub fn page_in_panel(&mut self, delta: isize) {
        self.move_in_panel_by(delta, Wrap::Clamp);
    }

    fn move_in_panel_by(&mut self, delta: isize, wrap: Wrap) {
        match &self.panel {
            Panel::Main(_) => {}
            Panel::Agents => self.move_cursor_by(delta, wrap),
            Panel::Agent { .. } => {
                self.point_at_row(|at, last| at.saturating_add_signed(delta).min(last));
            }
            Panel::Changes { file } => {
                self.panel = Panel::Changes {
                    file: file
                        .saturating_add_signed(delta)
                        .min(self.change_rows().saturating_sub(1)),
                };
            }
        }
    }

    /// Half the focused panel's rows, at least one.
    pub fn half_page(&self) -> isize {
        isize::try_from(self.areas.page / 2)
            .unwrap_or(isize::MAX)
            .max(1)
    }

    pub fn panel_to_edge(&mut self, edge: Edge) {
        match &self.panel {
            Panel::Main(_) => {}
            Panel::Agents => self.move_to_edge(edge),
            Panel::Agent { .. } => {
                self.point_at_row(|_, last| match edge {
                    Edge::First => 0,
                    Edge::Last => last,
                });
            }
            Panel::Changes { .. } => {
                self.panel = Panel::Changes {
                    file: match edge {
                        Edge::First => 0,
                        Edge::Last => self.change_rows().saturating_sub(1),
                    },
                };
            }
        }
    }

    /// Whether [3] leads with the branch row, which is a row the cursor can be
    /// on. Only the second tab has it.
    pub fn branch_row(&self) -> bool {
        let Some(entry) = self.selected_entry() else {
            return false;
        };
        let Some(Ok(facts)) = self.facts(entry.name()) else {
            return false;
        };
        self.changes_scope == ChangesScope::Since
            && shows_readiness(entry, &self.status(entry), facts.readiness())
    }

    /// The rows [3]'s cursor moves over: the branch, then the files.
    pub fn change_rows(&self) -> usize {
        usize::from(self.branch_row()) + self.change_list().len()
    }

    /// The folders and files [3] lists under the readiness row.
    pub fn change_list(&self) -> std::sync::Arc<Vec<TreeRow>> {
        let Some(tracked) = self
            .selected_entry()
            .and_then(|e| self.agents.get(e.name()))
        else {
            return std::sync::Arc::new(Vec::new());
        };
        let mut cache = tracked.tree.borrow_mut();
        if let Some(hit) = cache.as_ref().filter(|c| {
            c.generation == tracked.generation
                && c.view == self.changes_view
                && c.scope == self.changes_scope
                && c.collapsed == tracked.collapsed
        }) {
            return hit.rows.clone();
        }
        let rows = std::sync::Arc::new(tree::build(
            self.changes(),
            self.changes_view,
            &tracked.collapsed,
        ));
        *cache = Some(CachedTree {
            generation: tracked.generation,
            view: self.changes_view,
            scope: self.changes_scope,
            collapsed: tracked.collapsed.clone(),
            rows: rows.clone(),
        });
        rows
    }

    /// What is known about `name`, if anything is.
    pub fn tracked(&self, name: &SessionName) -> Option<&Tracked> {
        self.agents.get(name)
    }

    /// `name`'s record, started if there is none.
    pub fn track(&mut self, name: &SessionName) -> &mut Tracked {
        self.agents.entry(name.clone()).or_default()
    }

    /// What is known about `name`'s subagent `id`, if it is being watched.
    pub fn watched(&self, (name, id): &SubagentKey) -> Option<&Watched> {
        self.agents.get(name)?.subagents.get(id)
    }

    /// `name`'s git facts, or why they couldn't be read, as last read.
    pub fn facts(&self, name: &SessionName) -> Option<&Result<WorktreeFacts, String>> {
        self.agents.get(name)?.facts.as_ref()
    }

    /// The comments drafted on diffs for `name`, until sent.
    pub fn draft(&self, name: &SessionName) -> &[crate::review::Comment] {
        self.agents.get(name).map_or(&[], |t| t.draft.as_slice())
    }

    /// How many comments are drafted across every agent.
    pub fn drafted(&self) -> usize {
        self.agents.values().map(|t| t.draft.len()).sum()
    }

    /// How many messages are queued across every agent.
    pub fn queued(&self) -> usize {
        self.agents
            .values()
            .filter_map(|t| t.outbox.as_ref())
            .map(Outbox::len)
            .sum()
    }

    /// Records `name`'s freshly read facts: the generation moves on, the
    /// closed folders that no longer exist are forgotten, and [3]'s cursor
    /// stays on the row it was on.
    pub fn set_facts(&mut self, name: &SessionName, facts: Result<WorktreeFacts, String>) {
        let selected = self.selected_entry().is_some_and(|e| e.name() == name);
        let anchor = selected.then(|| self.changes_anchor()).flatten();
        self.facts_reads += 1;
        let generation = self.facts_reads;
        let tracked = self.track(name);
        tracked.generation = generation;
        match &facts {
            Ok(facts) => {
                let mut existing = tree::folders(&facts.changed);
                existing.extend(tree::folders(&facts.pending));
                tracked.collapsed.retain(|p| existing.contains(p));
            }
            Err(_) => tracked.collapsed.clear(),
        }
        tracked.facts = Some(facts);
        if let Some(anchor) = anchor {
            self.restore_changes_anchor(&anchor);
        }
    }

    /// What [3]'s row `row` is, none for the branch row.
    pub fn change_row(&self, row: usize) -> Option<TreeRow> {
        let at = row.checked_sub(usize::from(self.branch_row()))?;
        self.change_list().get(at).cloned()
    }

    /// The file on [3]'s row `row`, none for the branch row and folders.
    pub fn change_at(&self, row: usize) -> Option<&Change> {
        match self.change_row(row)?.node {
            Node::File(i) => self.changes().get(i),
            Node::Folder { .. } => None,
        }
    }

    /// The folder under [3]'s cursor.
    pub fn folder_under_cursor(&self) -> Option<TreeRow> {
        let Listed::Changes { file } = self.panel.list() else {
            return None;
        };
        let row = file.min(self.change_rows().checked_sub(1)?);
        self.change_row(row)
            .filter(|r| matches!(r.node, Node::Folder { .. }))
    }

    /// Moves [3]'s cursor to the next or previous file row, over folders and
    /// the branch row, while [0] keeps the keys. `false` when there is none.
    pub fn step_file(&mut self, forward: bool) -> bool {
        let Listed::Changes { file } = self.panel.list() else {
            return false;
        };
        let rows = self.change_rows();
        let mut at = file.min(rows.saturating_sub(1));
        loop {
            at = if forward {
                at + 1
            } else {
                match at.checked_sub(1) {
                    Some(at) => at,
                    None => return false,
                }
            };
            if at >= rows {
                return false;
            }
            if self.change_at(at).is_some() {
                self.set_changes_file(at);
                return true;
            }
        }
    }

    fn set_changes_file(&mut self, to: usize) {
        match &mut self.panel {
            Panel::Changes { file } | Panel::Main(Origin::Changes { file }) => *file = to,
            _ => {}
        }
    }

    /// Moves [3]'s cursor to the row for `path`, or the nearest row that
    /// stands for it: the first under it, or the folder that holds it.
    /// Whether there was one.
    fn point_changes_at(&mut self, path: &Path) -> bool {
        let skip = usize::from(self.branch_row());
        let list = self.change_list();
        let at = list
            .iter()
            .position(|r| r.path == path)
            .or_else(|| list.iter().position(|r| r.path.starts_with(path)))
            .or_else(|| {
                list.iter().position(|r| {
                    path.starts_with(&r.path) && matches!(r.node, Node::Folder { .. })
                })
            });
        if let Some(at) = at {
            self.set_changes_file(at + skip);
        }
        at.is_some()
    }

    /// What [3]'s cursor is on, if [3] has it.
    pub fn changes_anchor(&self) -> Option<RowId> {
        let (Panel::Changes { file } | Panel::Main(Origin::Changes { file })) = self.panel else {
            return None;
        };
        let row = file.min(self.change_rows().checked_sub(1)?);
        if self.branch_row() && row == 0 {
            return Some(RowId::Branch);
        }
        self.change_row(row).map(|r| RowId::Path(r.path))
    }

    /// Puts [3]'s cursor back on `anchor` after the list changed under it,
    /// or on the nearest row when it's gone.
    pub fn restore_changes_anchor(&mut self, anchor: &RowId) {
        let (Panel::Changes { file } | Panel::Main(Origin::Changes { file })) = self.panel else {
            return;
        };
        let found = match anchor {
            RowId::Branch if self.branch_row() => {
                self.set_changes_file(0);
                true
            }
            RowId::Branch => false,
            RowId::Path(path) => self.point_changes_at(path),
        };
        if !found {
            let last = self.change_rows().saturating_sub(1);
            self.set_changes_file(file.min(last));
        }
    }

    /// The path of the row [3]'s cursor is on, if it's a file or folder.
    fn changes_cursor_path(&self) -> Option<PathBuf> {
        let Listed::Changes { file } = self.panel.list() else {
            return None;
        };
        let row = file.min(self.change_rows().checked_sub(1)?);
        self.change_row(row).map(|r| r.path)
    }

    /// Switches [3] between the tree and the flat list, keeping the cursor
    /// on the same file.
    pub fn toggle_changes_view(&mut self) {
        let was = self.changes_cursor_path();
        self.changes_view = self.changes_view.toggled();
        if let Some(path) = was {
            self.point_changes_at(&path);
        }
    }

    /// Switches [3] between the uncommitted files and everything since the
    /// fork. The cursor stays on the same row when the other view has it,
    /// else on the branch row.
    pub fn toggle_changes_scope(&mut self) {
        let was = self.changes_cursor_path();
        self.changes_scope = self.changes_scope.toggled();
        if !was.is_some_and(|path| self.point_changes_at(&path)) {
            self.set_changes_file(0);
        }
    }

    /// Opens or closes the folder under [3]'s cursor. A chain row closes
    /// at its deepest folder and opens every folder it stands for.
    pub fn toggle_folder(&mut self) {
        let Some(folder) = self.folder_under_cursor() else {
            return;
        };
        let Some(name) = self.selected_entry().map(|e| e.name().clone()) else {
            return;
        };
        let closed = &mut self.track(&name).collapsed;
        if matches!(folder.node, Node::Folder { open: false }) {
            for path in &folder.chain {
                closed.remove(path);
            }
        } else {
            closed.insert(folder.path);
        }
    }

    /// Closes every folder of the selected agent, or opens them all, and
    /// keeps the cursor on the row it was on, or the folder that holds it.
    pub fn fold_all(&mut self, close: bool) {
        let Some(name) = self.selected_entry().map(|e| e.name().clone()) else {
            return;
        };
        let was = self.changes_cursor_path();
        let all = if close {
            tree::folders(self.changes())
        } else {
            HashSet::new()
        };
        self.track(&name).collapsed = all;
        if let Some(path) = was {
            self.point_changes_at(&path);
        }
    }

    /// What [3]'s cursor is on and the checkout it's in, unless the keys are
    /// going to the agent.
    fn changes_hover(&self) -> Option<(Hover, PathBuf)> {
        let Listed::Changes { file } = self.panel.list() else {
            return None;
        };
        if matches!(self.focus(), Focus::Agent(_)) {
            return None;
        }
        let entry = self.selected_entry()?;
        let row = file.min(self.change_rows().checked_sub(1)?);
        let root = entry.path()?.to_path_buf();
        let name = entry.name().clone();
        let generation = self.tracked(&name).map_or(0, |t| t.generation);
        if self.panel.in_main()
            && let Some(opened) = &self.opened
        {
            let hover = match opened {
                Opened::Commit { sha, subject } => Hover::Commit(CommitKey {
                    name,
                    sha: sha.clone(),
                    subject: subject.clone(),
                }),
                Opened::File { path, state } => Hover::Diff(DiffKey {
                    name,
                    change: Change::listed(&Uncommitted {
                        path: path.clone(),
                        state: *state,
                    }),
                    base: agent_core::ports::DiffBase::Head,
                    generation,
                }),
            };
            return Some((hover, root));
        }
        let folder = self.folder_under_cursor().map(|r| r.path);
        let hover = match (row, self.change_at(row), self.facts(&name)) {
            (_, None, _) if folder.is_none() && !self.branch_row() => return None,
            (_, None, _) if folder.is_some() => {
                let path = folder.unwrap_or_default();
                Hover::Folder(FolderKey {
                    files: self.folder_files(&path),
                    base: self.changes_scope.diff_base(),
                    name,
                    path,
                    generation,
                })
            }
            (_, Some(change), _) => Hover::Diff(DiffKey {
                name,
                change: change.clone(),
                base: self.changes_scope.diff_base(),
                generation,
            }),
            (_, None, Some(Ok(facts))) => Hover::Branch(BranchKey {
                name,
                ahead: facts.ahead,
                behind: facts.behind,
                dirty: facts.dirty(),
                compared: facts.compared.clone(),
                generation,
            }),
            (_, None, _) => return None,
        };
        Some((hover, root))
    }

    /// The document a read has to produce for what the cursor is on, and
    /// the checkout to read it in.
    pub fn hovered_document(&self) -> Option<(Key, PathBuf)> {
        let (hover, root) = self.changes_hover()?;
        let key = match hover {
            Hover::Diff(key) => Key::Diff(key),
            Hover::Branch(key) => Key::Branch(key),
            Hover::Folder(key) => Key::Folder(key),
            Hover::Commit(key) => Key::Commit(key),
            Hover::Turn(_) | Hover::Subagent(..) | Hover::Repo(_) => return None,
        };
        Some((key, root))
    }

    /// Which item of its panel the cursor is on, of how many, for [0]'s
    /// title while the sidebar is out of sight.
    pub fn position(&self) -> Option<(usize, usize)> {
        match self.panel.list() {
            Listed::Agents => None,
            Listed::Changes { file } => {
                let rows = self.change_rows();
                Some((file.min(rows.checked_sub(1)?) + 1, rows))
            }
            Listed::Agent { .. } => Some((self.row_at() + 1, self.subagent_rows().len() + 1)),
        }
    }

    /// Whether the keys go to the document in [0].
    pub fn main_focused(&self) -> bool {
        self.panel.in_main()
    }

    /// Whether the peek float is showing an agent's terminal. The keys
    /// never reach it.
    pub fn peeking(&self) -> bool {
        self.peek.is_some() && !matches!(self.focus(), Focus::Agent(_))
    }

    /// Whether [0] shows a document that scrolls, as opposed to the
    /// terminal or a repo's summary.
    pub fn shows_document(&self) -> bool {
        !matches!(self.hover(), None | Some(Hover::Repo(_)))
    }

    /// Whether the terminal is being looked at: [0] shows it, or the peek
    /// float does.
    pub fn terminal_visible(&self) -> bool {
        self.peeking() || self.hover().is_none()
    }

    /// The agent whose terminal [0] shows, if it shows one.
    pub fn visible_session(&self) -> Option<&SessionName> {
        self.pane.session().filter(|_| self.terminal_visible())
    }

    /// Points the main view at what the cursor is on.
    pub fn settle_main(&mut self) {
        let hover = self.hover();
        if hover.is_none() {
            self.panel.leave_document();
        }
        self.docs.settle(hover.as_ref(), Instant::now());
    }

    /// The selected agent's checkout and the file under [3]'s cursor in it.
    pub fn file_under_cursor(&self) -> Option<(PathBuf, PathBuf)> {
        let Listed::Changes { file } = self.panel.list() else {
            return None;
        };
        let root = self.selected_entry()?.path()?;
        let row = file.min(self.change_rows().checked_sub(1)?);
        self.change_at(row)
            .map(|c| (root.to_path_buf(), c.path.clone()))
    }

    pub fn activity(&self, name: &SessionName) -> Option<&Activity> {
        self.activities.as_ref()?.get(name)
    }

    pub fn status(&self, entry: &Entry) -> AgentStatus {
        match entry.state() {
            EntryState::Idle(_) => return AgentStatus::NoSession,
            EntryState::Exited(_) => return AgentStatus::Exited,
            EntryState::Live => {}
        }
        let Some(activity) = self.activity(entry.name()) else {
            return AgentStatus::Idle;
        };
        let since = activity.since;
        if let Some(ask) = activity.waiting() {
            return AgentStatus::Asking {
                question: ask.question.clone(),
                since: ask.since,
            };
        }
        match &activity.state {
            ActivityState::Idle | ActivityState::Exited => AgentStatus::Idle,
            ActivityState::Working { .. } | ActivityState::Delegating => {
                let pulse = if activity.stale(self.now) {
                    Pulse::Stale
                } else {
                    Pulse::Steady
                };
                let doing = match (&activity.state, activity.current_tool()) {
                    (ActivityState::Delegating, _) => NonZeroUsize::new(activity.running())
                        .map_or(Doing::HandingBack, Doing::Delegating),
                    (_, Some(tool)) => Doing::Tool {
                        name: tool.name.clone(),
                        target: tool.target.as_deref().map(Target::of),
                    },
                    (_, None) => Doing::Thinking,
                };
                AgentStatus::Working {
                    doing,
                    since,
                    pulse,
                }
            }
            ActivityState::Failed { reason } => AgentStatus::Failed {
                failure: Failure::of(reason),
                since,
            },
            ActivityState::Done { summary } => AgentStatus::Done {
                summary: summary.clone(),
                since,
            },
        }
    }

    /// Whether `t` may message the agent: it's live, isn't the one this TUI
    /// runs in, and reports through hooks, which is the only way to know
    /// when typing is safe.
    pub fn talkable<'a>(&self, entry: &'a Entry) -> Result<&'a SessionName, Unreachable> {
        let EntryState::Live = entry.state() else {
            return Err(Unreachable::NotRunning);
        };
        if self.own_session.as_ref() == Some(entry.name()) {
            return Err(Unreachable::Own);
        }
        if self.activity(entry.name()).is_none() {
            return Err(Unreachable::Unreported);
        }
        Ok(entry.name())
    }

    /// The moment a hook report has to postdate before typing into `name`
    /// is safe: its session's latest start as far as this TUI knows. A
    /// record that has itself launched since doesn't need it.
    pub fn not_before(&self, name: &SessionName) -> Option<SystemTime> {
        let tracked = self.agents.get(name)?;
        let seen = match tracked.live_since {
            Some(LiveSince::Since(at)) => Some(at),
            Some(LiveSince::Unknown) | None => None,
        };
        seen.max(tracked.seen.launched_at())
    }

    /// `name` is at rest but hasn't reported since its session started and
    /// never recorded a launch, so nothing can tell a restart from a record
    /// left by an earlier run.
    pub fn awaiting_report(&self, name: &SessionName) -> bool {
        self.activity(name)
            .is_some_and(|a| a.at_rest() && !a.bound_met(self.not_before(name)))
    }

    /// Whether text typed now into `name` can't answer a prompt as far as
    /// the record shows.
    pub fn ready_for_text(&self, name: &SessionName) -> bool {
        self.activity(name)
            .is_some_and(|a| a.ready_for_text(self.not_before(name), SystemTime::now()))
    }

    /// Whether the user's keys are going to `name`'s terminal.
    pub fn typing_into(&self, name: &SessionName) -> bool {
        self.pane
            .connection()
            .is_some_and(|c| &c.name == name && c.mode == Mode::Agent)
    }

    /// The selected agent's subagents, oldest first, as [2] lists them.
    pub fn subagent_rows(&self) -> Vec<(&AgentId, &Subagent)> {
        let Some(entry) = self.selected_entry() else {
            return Vec::new();
        };
        let Some(activity) = self
            .activity(entry.name())
            .filter(|_| matches!(entry.state(), EntryState::Live))
        else {
            return Vec::new();
        };
        let mut rows: Vec<_> = activity.subagents.iter().collect();
        if let Some(TurnRow::Subagent { by, id }) = self.panel.turn_row()
            && by == entry.name()
            && !activity.subagents.contains_key(id)
            && let Some(held) = self
                .agents
                .get(by)
                .and_then(|t| t.subagents.get(id))
                .and_then(|w| w.held.as_ref())
        {
            rows.push((id, held));
        }
        rows.sort_by_key(|(_, s)| s.since);
        rows
    }

    /// Keeps the subagent under [2]'s cursor as its agent last listed it,
    /// marks it finished once the activity drops it, and forgets it when the
    /// cursor has left its agent or its live session.
    pub fn hold_turn_row(&mut self) {
        let Some(TurnRow::Subagent { by, id }) = self.panel.turn_row() else {
            return;
        };
        let live = self
            .selected_entry()
            .is_some_and(|e| e.name() == by && matches!(e.state(), EntryState::Live));
        let listed = live
            .then(|| self.activity(by).and_then(|a| a.subagents.get(id)).cloned())
            .flatten();
        let now = self.now;
        let Some(watched) = self
            .agents
            .get_mut(by)
            .and_then(|t| t.subagents.get_mut(id))
        else {
            return;
        };
        match (live, listed) {
            (false, _) => watched.held = None,
            (true, Some(subagent)) => watched.held = Some(subagent),
            (true, None) => {
                if let Some(held) = &mut watched.held {
                    held.finish(now);
                }
            }
        }
    }

    /// Where [2]'s cursor is among its rows: 0 is the turn, then the
    /// subagents. The turn while it points at a subagent that's gone.
    pub fn row_at(&self) -> usize {
        let Some(TurnRow::Subagent { id, .. }) = self.panel.turn_row() else {
            return 0;
        };
        self.subagent_rows()
            .iter()
            .position(|(row, _)| *row == id)
            .map_or(0, |at| at + 1)
    }

    /// Moves [2]'s cursor to the row `to` picks from where it is and the
    /// last row's index.
    pub fn point_at_row(&mut self, to: impl FnOnce(usize, usize) -> usize) {
        let rows = self.subagent_rows();
        let row = to(self.row_at(), rows.len()).min(rows.len());
        let pointed = match (row.checked_sub(1), self.selected_entry()) {
            (Some(at), Some(entry)) => Some((entry.name().clone(), rows[at].0.clone())),
            _ => None,
        };
        let Some((by, id)) = pointed else {
            if let Some(row) = self.panel.turn_row_mut() {
                *row = TurnRow::Turn;
            }
            return;
        };
        self.track(&by).subagents.entry(id.clone()).or_default();
        if let Some(row) = self.panel.turn_row_mut() {
            *row = TurnRow::Subagent { by, id };
        }
        self.hold_turn_row();
    }

    /// The subagent [2]'s cursor is on, while [2] is focused.
    pub fn subagent_selected(&self) -> Option<(SessionName, AgentId)> {
        self.panel.turn_row()?;
        let name = self.selected_entry()?.name().clone();
        let rows = self.subagent_rows();
        let (id, _) = rows.get(self.row_at().checked_sub(1)?)?;
        Some((name, (*id).clone()))
    }

    /// What the main view reads: the file under [3]'s cursor or the row
    /// under [2]'s, unless the keys are going to the agent.
    pub fn hover(&self) -> Option<Hover> {
        if matches!(self.focus(), Focus::Agent(_)) {
            return None;
        }
        match self.panel.list() {
            Listed::Agents => match self.cursor() {
                Some(Cursor::Repo(key)) => Some(Hover::Repo(key.clone())),
                _ => None,
            },
            Listed::Changes { .. } => self.changes_hover().map(|(hover, _)| hover),
            Listed::Agent { .. } => {
                let entry = self.selected_entry()?;
                self.activity(entry.name())?;
                let name = entry.name().clone();
                Some(match self.subagent_selected() {
                    Some((_, id)) => Hover::Subagent(name, id),
                    None => Hover::Turn(name),
                })
            }
        }
    }

    /// How many subagents a live agent has running, if any.
    pub fn subagents(&self, entry: &Entry) -> Option<NonZeroUsize> {
        let EntryState::Live = entry.state() else {
            return None;
        };
        NonZeroUsize::new(self.activity(entry.name())?.running())
    }

    /// Agents that need the user, in the order `a` visits them: blocked ones
    /// (asking or failed) before unseen ones, longest waiting first.
    pub fn needs_you(&self) -> Vec<SessionName> {
        let Some(snapshot) = self.snapshot() else {
            return Vec::new();
        };
        let mut waiting: Vec<_> = snapshot
            .entries()
            .filter(|(_, e)| self.own_session.as_ref() != Some(e.name()))
            .filter_map(|(_, e)| {
                self.status(e)
                    .urgency()
                    .map(|(urgency, since)| (urgency, since, e.name().clone()))
            })
            .collect();
        waiting.sort();
        waiting.into_iter().map(|(_, _, name)| name).collect()
    }

    /// The agents `a` would visit, by kind: needing the user, and not the
    /// one being typed into.
    pub fn attention(&self) -> Attention {
        let typing_into = match self.focus() {
            Focus::Agent(c) => Some(&c.name),
            Focus::Overlay(_) | Focus::Nav(_) => None,
        };
        let Some(snapshot) = self.snapshot() else {
            return Attention::default();
        };
        self.needs_you()
            .iter()
            .filter(|name| Some(*name) != typing_into)
            .fold(Attention::default(), |mut counts, name| {
                match snapshot.entry(name).map(|(_, e)| self.status(e)) {
                    Some(AgentStatus::Asking { .. }) => counts.asking += 1,
                    Some(AgentStatus::Failed { .. }) => counts.failed += 1,
                    Some(AgentStatus::Done { .. }) => counts.done += 1,
                    _ => {}
                }
                counts
            })
    }

    /// Re-checks the cursor after what it points at may have gone, such as
    /// a Needs you row once its agent is seen.
    pub fn revalidate(&mut self) {
        let repo = self.selected().map(|(r, _)| r.key().clone());
        self.reconcile(repo.as_ref());
    }

    pub fn focus(&self) -> Focus<'_> {
        match (&self.overlay, self.pane.connection()) {
            (Overlay::None, Some(c)) if c.mode == Mode::Agent => Focus::Agent(c),
            (Overlay::None, connection) => Focus::Nav(connection),
            (overlay, _) => Focus::Overlay(overlay),
        }
    }

    pub fn reach(&self, entry: &Entry) -> Reach {
        if self.own_session.as_ref() == Some(entry.name()) {
            return Reach::Own;
        }
        let enter = match entry.state() {
            EntryState::Live => Enter::Attach,
            EntryState::Exited(Revival::Restart(at)) => Enter::Restart(at),
            EntryState::Exited(Revival::Resurrect) => Enter::Resurrect,
            EntryState::Idle(at) => Enter::Start(at),
        };
        Reach::Other {
            enter,
            kill: entry.running(self.own_session.as_ref()),
            remove: entry.removable(),
        }
    }

    /// Whether [1] lists `entry` on its own account.
    pub fn active(&self, entry: &Entry) -> bool {
        self.visibility.lists(entry)
    }

    /// Whether [1] lists `entry`: it's active, or it's what's selected, so
    /// the cursor never vanishes.
    pub fn shown(&self, entry: &Entry) -> bool {
        self.active(entry) || self.cursor().and_then(Cursor::entry_name) == Some(entry.name())
    }

    /// How many entries the filter leaves out of the current tab: what `.`
    /// would add to it.
    pub fn hidden(&self) -> usize {
        let Some(snapshot) = self.snapshot() else {
            return 0;
        };
        let hidden = |entries: &[Entry]| entries.iter().filter(|e| !self.shown(e)).count();
        match self.view {
            View::Pinned => 0,
            View::Focus => self
                .selected()
                .map_or(0, |(repo, _)| hidden(repo.entries())),
            View::Overview => snapshot.repos().iter().map(|r| hidden(r.entries())).sum(),
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        let Some(snapshot) = self.snapshot() else {
            return Vec::new();
        };
        let current = self.cursor().and_then(|c| c.repo(snapshot)).map(Repo::key);
        match self.view {
            View::Focus => current
                .and_then(|k| snapshot.repo(k))
                .map(|repo| self.focus_rows(repo))
                .unwrap_or_default(),
            View::Pinned => {
                let mut rows: Vec<Row> = snapshot
                    .pinned()
                    .into_iter()
                    .map(|(_, e)| Row::Entry {
                        name: e.name().clone(),
                        place: Place::Pinned,
                    })
                    .collect();
                let waiting: Vec<SessionName> = self
                    .needs_you()
                    .into_iter()
                    .filter(|name| !self.pinned(name))
                    .collect();
                rows.extend(Self::needs_you_rows(&waiting));
                rows
            }
            View::Overview => {
                let waiting = self.needs_you();
                let mut rows = Self::needs_you_rows(&waiting);
                let repos = snapshot.repos();
                let listed = |e: &&Entry| !waiting.contains(e.name()) && self.shown(e);
                // Folds are worked out from the extent's worst case, so
                // agents changing state never open or close a repo.
                let sizes: Vec<usize> = repos
                    .iter()
                    .map(|r| match sole(r) {
                        Some(_) => 0,
                        None => r.entries().iter().filter(|e| self.shown(e)).count(),
                    })
                    .collect();
                let room = usize::from(self.areas.sidebar.height);
                let tallest = self.extent().rows;
                let folds = if tallest <= room {
                    vec![Fold::Open; repos.len()]
                } else {
                    let origin = current
                        .and_then(|k| repos.iter().position(|r| r.key() == k))
                        .map_or(0, |i| i + 1);
                    let shut = tallest - sizes.iter().sum::<usize>();
                    folds(&sizes, origin, room.saturating_sub(shut))
                };
                for (repo, fold) in repos.iter().zip(folds) {
                    if let Some(only) = sole(repo) {
                        if listed(&only) {
                            rows.push(Row::Entry {
                                name: only.name().clone(),
                                place: Place::Solo,
                            });
                        }
                        continue;
                    }
                    let selected = self.cursor() == Some(&Cursor::Repo(repo.key().clone()));
                    if !selected && !repo.entries().iter().any(|e| listed(&e)) {
                        continue;
                    }
                    rows.push(Row::Repo {
                        key: repo.key().clone(),
                        fold,
                    });
                    if let Fold::Open = fold {
                        rows.extend(repo.entries().iter().filter(listed).map(|e| Row::Entry {
                            name: e.name().clone(),
                            place: Place::Nested,
                        }));
                    }
                }
                rows
            }
        }
    }

    /// The Needs you group, when anyone is in it.
    fn needs_you_rows(waiting: &[SessionName]) -> Vec<Row> {
        if waiting.is_empty() {
            return Vec::new();
        }
        std::iter::once(Row::Header(Group::NeedsYou))
            .chain(
                waiting
                    .iter()
                    .map(|name| Row::Grouped(Group::NeedsYou, name.clone())),
            )
            .collect()
    }

    /// Focus: the repo's entries, then agents from other repos that need
    /// you.
    fn focus_rows(&self, repo: &Repo) -> Vec<Row> {
        let mut rows: Vec<Row> = repo
            .entries()
            .iter()
            .filter(|e| self.shown(e))
            .map(|e| Row::Entry {
                name: e.name().clone(),
                place: Place::Alone,
            })
            .collect();
        let elsewhere: Vec<Row> = self
            .needs_you()
            .into_iter()
            .filter(|name| repo.entry(name).is_none())
            .map(Row::Elsewhere)
            .collect();
        if !elsewhere.is_empty() {
            rows.push(Row::Header(Group::NeedsYou));
            rows.extend(elsewhere);
        }
        rows
    }

    /// The sidebar list at its tallest: every tab reserves the Needs you
    /// header even while it's hidden, Focus counts every other repo's entry
    /// as a one-line row in it, and Pinned counts every entry that isn't
    /// pinned as waiting under the pins.
    pub fn extent(&self) -> Extent {
        let Some(snapshot) = self.snapshot() else {
            return Extent::default();
        };
        let (headers, cards): (usize, Vec<&Entry>) = match self.view {
            View::Focus => {
                let Some((repo, _)) = self.selected() else {
                    return Extent::default();
                };
                let here = repo.entries().iter().filter(|e| self.active(e)).count();
                let elsewhere = snapshot.entries().filter(|(_, e)| self.active(e)).count() - here;
                return Extent {
                    rows: 1 + elsewhere + here,
                    live: repo
                        .entries()
                        .iter()
                        .filter(|e| matches!(e.state(), EntryState::Live))
                        .count(),
                };
            }
            View::Overview => {
                let repos = snapshot
                    .repos()
                    .iter()
                    .filter(|r| sole(r).is_none())
                    .filter(|r| r.entries().iter().any(|e| self.active(e)));
                (
                    1 + repos.count(),
                    snapshot
                        .entries()
                        .map(|(_, e)| e)
                        .filter(|e| self.active(e))
                        .collect(),
                )
            }
            View::Pinned => (1, snapshot.entries().map(|(_, e)| e).collect()),
        };
        Extent {
            rows: headers + cards.len(),
            live: cards
                .iter()
                .filter(|e| matches!(e.state(), EntryState::Live))
                .count(),
        }
    }

    pub fn items(&self) -> Vec<Cursor> {
        self.rows().iter().filter_map(Row::cursor).collect()
    }

    /// Replaces the snapshot, keeping the cursor on the same thing if it
    /// still exists and somewhere sensible if not.
    pub fn load(&mut self, snapshot: Snapshot) {
        let first = matches!(self.load, Load::Pending);
        self.snapshots += 1;
        let previous = self.cursor().cloned();
        let previous_repo = previous.as_ref().and_then(|c| {
            self.snapshot()
                .and_then(|s| c.repo(s))
                .map(|r| r.key().clone())
        });
        self.load = Load::Ready {
            snapshot,
            cursor: previous,
        };
        self.reconcile(previous_repo.as_ref());
        match self.follow.take() {
            Some(Follow::Submitted { name, ticket }) => match self.entry_cursor(&name) {
                Some(cursor) => {
                    self.set_cursor(cursor);
                    self.preview_at = None;
                }
                None => self.follow = Some(Follow::Submitted { name, ticket }),
            },
            Some(Follow::Backgrounded { name, ticket }) => match self.entry_cursor(&name) {
                Some(cursor) => {
                    self.set_cursor(cursor);
                    self.schedule_preview();
                }
                None => self.follow = Some(Follow::Backgrounded { name, ticket }),
            },
            Some(Follow::Restored(name)) if first => {
                let here = self
                    .snapshot()
                    .and_then(|s| s.containing(&self.start_dir))
                    .map(|(_, e)| e.name().clone());
                let cursor = match here {
                    Some(here) => self.entry_cursor(&here),
                    None => self.entry_cursor(&name),
                };
                if let Some(cursor) = cursor {
                    self.set_cursor(cursor);
                    self.schedule_preview();
                }
            }
            Some(Follow::Repo { root, until }) => {
                let key = RepoKey::Root(root.clone());
                if self.snapshot().is_some_and(|s| s.repo(&key).is_some()) {
                    self.select_repo(&key);
                    self.schedule_preview();
                } else if self.snapshots < until {
                    self.follow = Some(Follow::Repo { root, until });
                }
            }
            Some(Follow::Restored(_)) | None => {}
        }
    }

    /// A cursor on `name` if the snapshot shows it.
    fn entry_cursor(&self, name: &SessionName) -> Option<Cursor> {
        self.snapshot()?
            .entry(name)
            .map(|_| Cursor::Entry(name.clone()))
    }

    fn reconcile(&mut self, previous_repo: Option<&RepoKey>) {
        let view = self.view;
        let start_dir = self.start_dir.clone();
        let waiting = self.needs_you();
        let visibility = self.visibility;
        let Load::Ready { snapshot, cursor } = &mut self.load else {
            return;
        };
        let in_needs_you = |e: &Entry| match view {
            View::Focus => false,
            View::Overview => waiting.contains(e.name()),
            View::Pinned => waiting.contains(e.name()) && e.pin().is_none(),
        };
        let listed = |e: &Entry| match view {
            View::Focus | View::Overview => !in_needs_you(e),
            View::Pinned => e.pin().is_some(),
        };
        let valid = match cursor.as_ref() {
            None => false,
            Some(Cursor::Repo(key)) => {
                view == View::Overview
                    && snapshot
                        .repo(key)
                        .is_some_and(|r| r.entries().iter().any(&listed))
            }
            Some(Cursor::Grouped(Group::NeedsYou, name)) => {
                snapshot.entry(name).is_some_and(|(_, e)| in_needs_you(e))
            }
            Some(Cursor::Entry(name)) => snapshot.entry(name).is_some_and(|(_, e)| listed(e)),
        };
        if valid {
            return;
        }
        let entry_of = |e: &Entry| {
            if in_needs_you(e) {
                Some(Cursor::Grouped(Group::NeedsYou, e.name().clone()))
            } else if listed(e) {
                Some(Cursor::Entry(e.name().clone()))
            } else {
                None
            }
        };
        let fallback = match cursor.as_ref() {
            Some(Cursor::Grouped(_, name) | Cursor::Entry(name)) => {
                snapshot.entry(name).and_then(|(_, e)| entry_of(e))
            }
            Some(Cursor::Repo(key)) => snapshot
                .repo(key)
                .and_then(|r| entry_of(lead(visibility, r))),
            None => None,
        };
        let fallback = fallback.or_else(|| match view {
            View::Pinned => snapshot
                .pinned()
                .first()
                .map(|(_, e)| *e)
                .or_else(|| snapshot.entries().map(|(_, e)| e).find(|e| in_needs_you(e)))
                .and_then(entry_of),
            View::Focus | View::Overview => previous_repo
                .and_then(|k| snapshot.repo(k))
                .and_then(|r| entry_of(lead(visibility, r)))
                .or_else(|| {
                    snapshot
                        .containing(&start_dir)
                        .or_else(|| snapshot.entries().next())
                        .and_then(|(_, e)| entry_of(e))
                }),
        });
        if cursor.as_ref().and_then(Cursor::entry_name)
            != fallback.as_ref().and_then(Cursor::entry_name)
        {
            self.panel = Panel::Agents;
        }
        *cursor = fallback;
        self.schedule_preview();
    }

    /// What `form` means against the current snapshot.
    pub fn derive(&self, form: &NewAgent) -> Derived {
        form.derive(
            self.snapshot(),
            &self.catalogue,
            self.own_session.as_ref(),
            &self.start_dir,
        )
    }

    /// Whether `name` is a pinned agent in the snapshot.
    fn pinned(&self, name: &SessionName) -> bool {
        self.snapshot()
            .and_then(|s| s.entry(name))
            .is_some_and(|(_, e)| e.pin().is_some())
    }

    /// Moves the cursor; repo rows only exist in Overview, so in Focus
    /// those become the entry they stand for. In Overview an agent that
    /// needs you is listed only under Needs you, and a repo whose agents
    /// all need you has no row of its own. Pinned lists pins and the
    /// other waiting agents, so anything else moves the tab to Overview.
    pub fn set_cursor(&mut self, next: Cursor) {
        self.displaced = None;
        if self.view == View::Pinned {
            let listed = match &next {
                Cursor::Repo(_) => false,
                Cursor::Entry(name) | Cursor::Grouped(_, name) => {
                    self.pinned(name) || self.needs_you().contains(name)
                }
            };
            if !listed {
                self.view = View::Overview;
            }
        }
        let next = match (self.view, next) {
            (View::Focus, Cursor::Grouped(_, name)) => Cursor::Entry(name),
            (View::Focus, Cursor::Repo(key)) => match self.snapshot().and_then(|s| s.repo(&key)) {
                Some(repo) => Cursor::Entry(lead(self.visibility, repo).name().clone()),
                None => return,
            },
            (View::Pinned, Cursor::Entry(name) | Cursor::Grouped(_, name)) => {
                if self.pinned(&name) {
                    Cursor::Entry(name)
                } else {
                    Cursor::Grouped(Group::NeedsYou, name)
                }
            }
            (View::Overview, Cursor::Entry(name)) if self.needs_you().contains(&name) => {
                Cursor::Grouped(Group::NeedsYou, name)
            }
            (View::Overview, Cursor::Repo(key)) => {
                let waiting = self.needs_you();
                match self.snapshot().and_then(|s| s.repo(&key)) {
                    Some(repo) if repo.entries().iter().all(|e| waiting.contains(e.name())) => {
                        Cursor::Grouped(Group::NeedsYou, repo.first().name().clone())
                    }
                    _ => Cursor::Repo(key),
                }
            }
            (_, next) => next,
        };
        let Load::Ready { cursor, .. } = &mut self.load else {
            return;
        };
        if cursor.as_ref() != Some(&next) {
            if cursor.as_ref().and_then(Cursor::entry_name) != next.entry_name() {
                self.panel = Panel::Agents;
            }
            *cursor = Some(next);
            self.schedule_preview();
        }
    }

    pub fn schedule_preview(&mut self) {
        self.preview_at = Some(Instant::now() + PREVIEW_DEBOUNCE);
    }

    /// Moves the selection `delta` rows, past the ends as `wrap` says.
    pub fn move_cursor_by(&mut self, delta: isize, wrap: Wrap) {
        let items = self.items();
        if items.is_empty() {
            return;
        }
        let current = self
            .cursor()
            .and_then(|c| items.iter().position(|i| i == c))
            .unwrap_or(0);
        let last = items.len() - 1;
        let next = match wrap {
            Wrap::Around => current
                .checked_add_signed(delta)
                .map_or(last, |n| n % items.len()),
            Wrap::Clamp => current.saturating_add_signed(delta).min(last),
        };
        self.set_cursor(items[next].clone());
    }

    pub fn move_to_edge(&mut self, edge: Edge) {
        let items = self.items();
        let target = match edge {
            Edge::First => items.first(),
            Edge::Last => items.last(),
        };
        if let Some(target) = target.cloned() {
            self.set_cursor(target);
        }
    }

    pub fn jump_repo(&mut self, delta: isize) {
        let Some(snapshot) = self.snapshot() else {
            return;
        };
        let here = self.cursor().and_then(|c| c.repo(snapshot)).map(Repo::key);
        let repos: Vec<&Repo> = snapshot
            .repos()
            .iter()
            .filter(|r| Some(r.key()) == here || r.entries().iter().any(|e| self.active(e)))
            .collect();
        if repos.is_empty() {
            return;
        }
        let current = here
            .and_then(|key| repos.iter().position(|x| x.key() == key))
            .unwrap_or(0);
        let next = current
            .checked_add_signed(delta)
            .map_or(repos.len() - 1, |n| n % repos.len());
        let key = repos[next].key().clone();
        self.select_repo(&key);
    }

    /// Selects a repo, in the repo tab when the Pinned tab is showing.
    pub fn select_repo(&mut self, key: &RepoKey) {
        let Some(repo) = self.snapshot().and_then(|s| s.repo(key)) else {
            return;
        };
        let cursor = match (self.view, sole(repo)) {
            (View::Overview, None) => Cursor::Repo(key.clone()),
            (View::Overview, Some(_)) | (View::Focus | View::Pinned, _) => {
                Cursor::Entry(lead(self.visibility, repo).name().clone())
            }
        };
        if self.view == View::Pinned {
            self.view = View::Focus;
        }
        self.set_cursor(cursor);
    }

    /// The next tab.
    #[cfg(test)]
    pub fn toggle_view(&mut self) {
        self.cycle_view(Direction::Forward);
    }

    #[cfg(test)]
    pub fn show_view(&mut self, view: View) {
        while self.view != view {
            self.cycle_view(Direction::Forward);
        }
    }

    /// Moves to the next or previous tab, keeping the selected agent when
    /// the tab lists it. One that doesn't is remembered, so a full cycle
    /// comes back to it.
    pub fn cycle_view(&mut self, direction: Direction) {
        let remembered = self
            .displaced
            .take()
            .filter(|c| self.snapshot().and_then(|s| c.repo(s)).is_some());
        let wanted = remembered.or_else(|| self.cursor().cloned());
        let repo = self.selected().map(|(r, _)| r.key().clone());
        self.view = match direction {
            Direction::Forward => self.view.next(),
            Direction::Back => self.view.previous(),
        };
        if let Load::Ready { cursor, .. } = &mut self.load {
            cursor.clone_from(&wanted);
        }
        self.reconcile(repo.as_ref());
        self.schedule_preview();
        if self.cursor().and_then(Cursor::entry_name)
            != wanted.as_ref().and_then(Cursor::entry_name)
        {
            self.displaced = wanted;
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            kind: StatusKind::Info,
            at: Instant::now(),
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            kind: StatusKind::Error,
            at: Instant::now(),
        });
    }

    /// Switcher order: the previous agent first (alt-tab), then pins, then
    /// the rest by recency, then everything else, live first.
    pub fn switcher_items(&self) -> Vec<(&Repo, &Entry)> {
        let Some(snapshot) = self.snapshot() else {
            return Vec::new();
        };
        let current = self.pane.session();
        let recent: Vec<_> = self
            .mru
            .iter()
            .filter(|n| Some(*n) != current)
            .filter_map(|n| snapshot.entry(n))
            .collect();
        let mut rest: Vec<_> = snapshot
            .entries()
            .filter(|(_, e)| self.reach(e) != Reach::Own)
            .collect();
        rest.sort_by_cached_key(|(_, e)| {
            (e.presence().is_none(), e.name().as_str().to_lowercase())
        });
        let mut ordered: Vec<(&Repo, &Entry)> = Vec::new();
        let candidates = recent
            .first()
            .copied()
            .into_iter()
            .chain(snapshot.pinned())
            .chain(recent)
            .chain(rest);
        for item in candidates.filter(|(_, e)| self.reach(e) != Reach::Own) {
            if !ordered.iter().any(|(_, e)| e.name() == item.1.name()) {
                ordered.push(item);
            }
        }
        ordered
    }
}

/// Case-insensitive subsequence match; returns matched byte offsets.
pub fn fuzzy_match(query: &str, text: &str) -> Option<Vec<usize>> {
    let mut indices = Vec::new();
    let mut chars = text
        .char_indices()
        .map(|(i, c)| (i, c.to_ascii_lowercase()));
    for q in query.chars().map(|c| c.to_ascii_lowercase()) {
        let (i, _) = chars.by_ref().find(|(_, c)| *c == q)?;
        indices.push(i);
    }
    Some(indices)
}

/// A field of the new-agent form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Task,
    Name,
    Repo,
}

impl Field {
    pub fn next(self) -> Self {
        match self {
            Self::Task => Self::Name,
            Self::Name => Self::Repo,
            Self::Repo => Self::Task,
        }
    }

    pub fn previous(self) -> Self {
        self.next().next()
    }
}

/// Where a new agent's worktree comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoChoice {
    Known(RepoRoot),
    /// A typed path, resolved to its repo by the worker.
    Path(PathBuf),
}

/// What the repo field currently resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoInput {
    /// Nothing typed: the repo selected when the form opened.
    Default(Option<RepoRoot>),
    /// A typed path (`/`, `~/`, `./`, `../`) to a directory, absolute.
    Path(PathBuf),
    /// A typed path to nothing.
    Missing(PathBuf),
    /// A search; the chosen match, if any.
    Search(Option<RepoRoot>),
    /// `~user/...`, which isn't supported.
    Unsupported,
}

impl RepoInput {
    fn path(path: PathBuf) -> Self {
        if path.is_dir() {
            Self::Path(path)
        } else {
            Self::Missing(path)
        }
    }

    pub fn choice(&self) -> Option<RepoChoice> {
        match self {
            Self::Default(root) | Self::Search(root) => root.clone().map(RepoChoice::Known),
            Self::Path(path) => Some(RepoChoice::Path(path.clone())),
            Self::Missing(_) | Self::Unsupported => None,
        }
    }
}

/// What ⏎ would do with the typed name, decided once per edit or refresh so
/// what the form shows is what submitting does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameCheck {
    Empty,
    TooLong {
        bytes: usize,
    },
    Invalid(String),
    /// The session the TUI runs in.
    Own,
    /// A repo's main checkout without a session; start it from the list.
    MainCheckout {
        repo: String,
    },
    /// Running: ⏎ attaches and types the task in.
    Live {
        name: AgentName,
        repo: String,
    },
    /// Exited: ⏎ resurrects it, which can't carry a task.
    Exited {
        name: AgentName,
        repo: String,
    },
    /// An agent worktree with no session: ⏎ starts it with the task.
    Idle {
        name: AgentName,
        repo: String,
        at: Startable,
    },
    Free(AgentName),
}

impl NameCheck {
    fn of(text: &str, snapshot: Option<&Snapshot>, own: Option<&SessionName>) -> Self {
        use agent_core::model::EntryKind;
        use agent_core::names::NameError;
        if text.is_empty() {
            return Self::Empty;
        }
        let name = match text.parse::<AgentName>() {
            Ok(name) => name,
            Err(NameError::TooLong { len, .. }) => return Self::TooLong { bytes: len },
            Err(NameError::Reserved(_)) => return Self::Invalid(format!("'{text}' is reserved")),
            Err(_) => {
                let bad = invalid_chars(text)
                    .into_iter()
                    .next()
                    .map_or(' ', |(_, c)| c);
                return Self::Invalid(format!("'{bad}' not allowed: letters, digits, . - _"));
            }
        };
        let session = name.session();
        if own == Some(&session) {
            return Self::Own;
        }
        let Some((repo, entry)) = snapshot.and_then(|s| s.entry(&session)) else {
            return Self::Free(name);
        };
        let repo = repo.name().into_owned();
        match (entry.kind(), entry.state()) {
            (_, EntryState::Live) => Self::Live { name, repo },
            (_, EntryState::Exited(Revival::Resurrect)) => Self::Exited { name, repo },
            (
                EntryKind::Agent { .. },
                EntryState::Idle(at) | EntryState::Exited(Revival::Restart(at)),
            ) => Self::Idle { name, repo, at },
            (_, EntryState::Exited(Revival::Restart(_)) | EntryState::Idle(_)) => {
                Self::MainCheckout { repo }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    First,
    Last,
}

/// Where the new-agent form is with its submission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Editing,
    /// Waiting for `launch`'s result, which activates the agent. The form
    /// shows `launch` as it was submitted, whatever refreshes say since.
    Submitting {
        ticket: Ticket,
        launch: Launchable,
    },
}

/// What ⏎ in the form does, decided in `refresh` from the name check, the
/// prompt and the repo, so the form shows exactly what submitting does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    Ready(Launchable),
    Blocked(Blocked),
}

/// Something the form can launch, carried out as decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launchable {
    Create {
        name: AgentName,
        repo: RepoChoice,
        task: Option<Prompt>,
    },
    /// Running: attach, typing the prompt in if there is one.
    Attach {
        name: AgentName,
        task: Option<Prompt>,
    },
    /// Exited: resurrect it. A prompt can't be typed into a dead session.
    Resurrect(AgentName),
    /// An idle worktree: start it with the prompt.
    Start { at: Startable, task: Option<Prompt> },
    /// A branch or pull request to review: its worktree is created or
    /// found by the branch, so the agent reached may be named otherwise.
    Checkout {
        name: AgentName,
        repo: RepoChoice,
        target: Checkout,
    },
}

impl Launchable {
    pub fn session(&self) -> SessionName {
        match self {
            Self::Create { name, .. }
            | Self::Attach { name, .. }
            | Self::Resurrect(name)
            | Self::Checkout { name, .. } => name.session(),
            Self::Start { at, .. } => at.name(),
        }
    }

    /// What ⏎ does, as a verb.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Create { .. } => "create",
            Self::Attach { task: Some(_), .. } => "attach + send",
            Self::Attach { task: None, .. } => "attach",
            Self::Resurrect(_) => "resurrect",
            Self::Start { .. } => "start",
            Self::Checkout { .. } => "check out",
        }
    }

    /// What's happening while it runs.
    pub fn progress(&self) -> &'static str {
        match self {
            Self::Create { .. } => "creating",
            Self::Attach { .. } => "attaching to",
            Self::Resurrect(_) => "resurrecting",
            Self::Start { .. } => "starting",
            Self::Checkout { .. } => "checking out",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// The name check says why.
    Name,
    Prompt(String),
    /// Not a branch name or a pull request link.
    Target(String),
    /// The name is an agent on another branch (`None`: a detached HEAD),
    /// so the checkout can't attach to it.
    OnAnotherBranch(Option<BranchName>),
    /// The name is an agent in another repo, whose branches are its own.
    InAnotherRepo(String),
    NoRepo,
    /// Exited, with a prompt that couldn't be delivered.
    ExitedWithPrompt,
}

impl Blocked {
    /// Why ⏎ did nothing, unless the form already shows it.
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Name | Self::OnAnotherBranch(_) | Self::InAnotherRepo(_) => None,
            Self::Prompt(why) => Some(format!("prompt: {why}")),
            Self::Target(why) => Some(why.clone()),
            Self::NoRepo => Some("pick a repo: type its name, or a path".into()),
            Self::ExitedWithPrompt => Some(
                "it exited, so the prompt can't be typed in. Clear it to resurrect, then send it"
                    .into(),
            ),
        }
    }
}

impl Submission {
    /// A checkout needs a target and a repo. A name that already exists
    /// is still a checkout when its agent is on the branch (the worker
    /// attaches), and refused here when the snapshot shows it isn't. For a
    /// pull request the branch is only known once the forge answers.
    fn checkout(
        check: &NameCheck,
        target: &str,
        repo: &RepoInput,
        snapshot: Option<&Snapshot>,
    ) -> Self {
        let target = match target.trim() {
            "" => {
                return Self::Blocked(Blocked::Target("type a branch, or a GitHub PR link".into()));
            }
            text => match text.parse::<Checkout>() {
                Ok(target) => target,
                Err(why) => return Self::Blocked(Blocked::Target(why.to_string())),
            },
        };
        let name = match check {
            NameCheck::Free(name) => name,
            NameCheck::Live { name, .. }
            | NameCheck::Exited { name, .. }
            | NameCheck::Idle { name, .. } => {
                let existing = snapshot.and_then(|s| s.entry(&name.session()));
                let chosen = match repo.choice() {
                    Some(RepoChoice::Known(root)) => Some(root),
                    Some(RepoChoice::Path(_)) | None => None,
                };
                if let (Some((in_repo, _)), Some(chosen)) = (existing, chosen)
                    && in_repo.key().root() != Some(&chosen)
                {
                    return Self::Blocked(Blocked::InAnotherRepo(in_repo.name().into_owned()));
                }
                let head = existing.and_then(|(_, e)| e.head());
                if let (Checkout::Branch(branch), Some(head)) = (&target, head)
                    && head.branch() != Some(branch)
                {
                    return Self::Blocked(Blocked::OnAnotherBranch(head.branch().cloned()));
                }
                name
            }
            NameCheck::Empty
            | NameCheck::TooLong { .. }
            | NameCheck::Invalid(_)
            | NameCheck::Own
            | NameCheck::MainCheckout { .. } => return Self::Blocked(Blocked::Name),
        };
        match repo.choice() {
            Some(repo) => Self::Ready(Launchable::Checkout {
                name: name.clone(),
                repo,
                target,
            }),
            None => Self::Blocked(Blocked::NoRepo),
        }
    }

    fn of(check: &NameCheck, task: &str, repo: &RepoInput) -> Self {
        let task = match task.trim() {
            "" => Ok(None),
            _ => task.parse::<Prompt>().map(Some).map_err(|e| e.to_string()),
        };
        match (check, task) {
            (_, Err(why)) => Self::Blocked(Blocked::Prompt(why)),
            (NameCheck::Free(name), Ok(task)) => match repo.choice() {
                Some(repo) => Self::Ready(Launchable::Create {
                    name: name.clone(),
                    repo,
                    task,
                }),
                None => Self::Blocked(Blocked::NoRepo),
            },
            (NameCheck::Live { name, .. }, Ok(task)) => Self::Ready(Launchable::Attach {
                name: name.clone(),
                task,
            }),
            (NameCheck::Exited { .. }, Ok(Some(_))) => Self::Blocked(Blocked::ExitedWithPrompt),
            (NameCheck::Exited { name, .. }, Ok(None)) => {
                Self::Ready(Launchable::Resurrect(name.clone()))
            }
            (NameCheck::Idle { at, .. }, Ok(task)) => Self::Ready(Launchable::Start {
                at: at.clone(),
                task,
            }),
            (
                NameCheck::Empty
                | NameCheck::TooLong { .. }
                | NameCheck::Invalid(_)
                | NameCheck::Own
                | NameCheck::MainCheckout { .. },
                Ok(_),
            ) => Self::Blocked(Blocked::Name),
        }
    }
}

/// Where the suggested name came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameSource {
    Link,
    Words,
    Model,
    Branch,
}

/// The name used when none is typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameSuggestion {
    pub name: String,
    pub source: NameSource,
}

/// Asking the harness's model for a name, debounced while the prompt is
/// being typed. `generation` identifies the prompt edit a reply belongs to.
#[derive(Debug, Clone, Default)]
pub struct Naming {
    pub generation: u64,
    pub state: NamingState,
}

#[derive(Debug, Clone, Default)]
pub enum NamingState {
    /// Nothing to ask: the prompt is short, linked, or not asked about yet.
    #[default]
    Idle,
    /// Ask once typing has paused until then.
    Due(Instant),
    Asking,
    /// The model's name for `prompt`.
    Answered {
        prompt: String,
        name: AgentName,
    },
}

/// What the form's first field is: a first prompt for a new agent, or
/// what to check out for review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormKind {
    New,
    Checkout,
}

/// The new-agent form. `name` is what the user typed; when it's empty the
/// suggestion is used. `suggestion`, `check` and `repo_input` are derived
/// from the fields and the snapshot, and refreshed whenever either changes.
/// For a checkout, `task` holds the branch or pull request link.
#[derive(Debug, Clone)]
pub struct NewAgent {
    pub kind: FormKind,
    pub task: String,
    pub name: String,
    pub repo: Query,
    pub default_repo: Option<RepoRoot>,
    pub focus: Field,
    pub error: Option<String>,
    pub naming: Naming,
    pub phase: Phase,
}

/// What the form's fields mean against the current snapshot, worked out
/// whenever it's needed so it can't go stale. A suggestion never points at
/// an existing agent: it gets `-2`, `-3`… instead, so only a name the user
/// typed can attach to something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub suggestion: Option<NameSuggestion>,
    /// What ⏎ would name the agent: the typed name, or the suggestion.
    pub name: String,
    pub check: NameCheck,
    pub repo_input: RepoInput,
    /// The rest of the matched repo's name, when the typed text is a proper
    /// prefix of it, ignoring case.
    pub completion: Option<String>,
    pub submission: Submission,
}

impl NewAgent {
    pub fn new(kind: FormKind, default_repo: Option<RepoRoot>) -> Self {
        Self {
            kind,
            task: String::new(),
            name: String::new(),
            repo: Query::default(),
            default_repo,
            focus: Field::Task,
            error: None,
            naming: Naming::default(),
            phase: Phase::Editing,
        }
    }

    /// The prompt changed: drop any model answer for the old one and, if
    /// the prompt is prose worth naming, ask again once typing pauses.
    pub fn prompt_changed(&mut self) {
        self.naming.generation += 1;
        let source = agent_core::naming::suggest(&self.task).map(|s| s.source);
        let words = self.task.split_whitespace().count();
        self.naming.state = match (self.kind, source) {
            (FormKind::Checkout, _) | (_, Some(agent_core::naming::Source::Link)) => {
                NamingState::Idle
            }
            _ if words < NAMING_MIN_WORDS => NamingState::Idle,
            _ => NamingState::Due(Instant::now() + NAMING_DEBOUNCE),
        };
    }

    /// The model replied for `generation`; kept only if the prompt hasn't
    /// changed since.
    pub fn model_named(&mut self, generation: u64, name: Option<AgentName>) {
        if generation != self.naming.generation {
            return;
        }
        self.naming.state = match name {
            Some(name) => NamingState::Answered {
                prompt: self.task.clone(),
                name,
            },
            None => NamingState::Idle,
        };
    }

    pub fn derive(
        &self,
        snapshot: Option<&Snapshot>,
        catalogue: &[RepoRoot],
        own: Option<&SessionName>,
        start_dir: &Path,
    ) -> Derived {
        use agent_core::naming::{Source, for_checkout, suggest};
        let suggestion = match self.kind {
            FormKind::Checkout => self
                .task
                .trim()
                .parse::<Checkout>()
                .ok()
                .and_then(|target| {
                    let source = match target {
                        Checkout::Branch(_) => NameSource::Branch,
                        Checkout::PullRequest(_) => NameSource::Link,
                    };
                    for_checkout(&target).map(|name| NameSuggestion {
                        name: name.to_string(),
                        source,
                    })
                }),
            FormKind::New => {
                let local = suggest(&self.task);
                let answer = match &self.naming.state {
                    NamingState::Answered { prompt, name } if *prompt == self.task => Some(name),
                    _ => None,
                };
                match (local, answer) {
                    (Some(link), _) if link.source == Source::Link => {
                        Some((link.name, NameSource::Link))
                    }
                    (_, Some(name)) => Some((name.clone(), NameSource::Model)),
                    (Some(words), None) => Some((words.name, NameSource::Words)),
                    (None, None) => None,
                }
                .map(|(name, source)| NameSuggestion {
                    name: free_name(&name, snapshot, own),
                    source,
                })
            }
        };
        let name = match (self.name.as_str(), &suggestion) {
            ("", Some(suggestion)) => suggestion.name.clone(),
            (typed, _) => typed.to_string(),
        };
        let check = NameCheck::of(&name, snapshot, own);
        let mut repo_input = self.parse_repo(snapshot, catalogue, start_dir);
        // A pull request link names its repo, which wins over the selected
        // one while nothing is typed and exactly one repo has that name.
        if let (FormKind::Checkout, RepoInput::Default(_)) = (self.kind, &repo_input)
            && let Ok(Checkout::PullRequest(pr)) = self.task.trim().parse::<Checkout>()
            && let Some(root) = named_repo(snapshot, catalogue, &pr.repo)
        {
            repo_input = RepoInput::Default(Some(root));
        }
        let completion = match &repo_input {
            RepoInput::Search(Some(root)) => completes(&root.name(), self.repo.text.trim()),
            _ => None,
        };
        let submission = match self.kind {
            FormKind::New => Submission::of(&check, &self.task, &repo_input),
            FormKind::Checkout => Submission::checkout(&check, &self.task, &repo_input, snapshot),
        };
        Derived {
            suggestion,
            name,
            check,
            repo_input,
            completion,
            submission,
        }
    }

    fn parse_repo(
        &self,
        snapshot: Option<&Snapshot>,
        catalogue: &[RepoRoot],
        start_dir: &Path,
    ) -> RepoInput {
        let text = self.repo.text.trim();
        if text.is_empty() {
            return RepoInput::Default(self.default_repo.clone());
        }
        match typed_path(text, start_dir) {
            Some(Typed::Path(path)) => RepoInput::path(path),
            Some(Typed::Unsupported | Typed::NoHome) => RepoInput::Unsupported,
            None => {
                let matches = repo_matches(snapshot, catalogue, text);
                RepoInput::Search(self.repo.pick(&matches).cloned())
            }
        }
    }
}

/// What's left of `name` after `typed`, compared a character at a time
/// ignoring case, if `typed` is a proper prefix of it.
pub fn completes(name: &str, typed: &str) -> Option<String> {
    let mut chars = name.char_indices();
    let mut end = 0;
    for typed in typed.chars() {
        let (i, c) = chars.next()?;
        if !c.to_lowercase().eq(typed.to_lowercase()) {
            return None;
        }
        end = i + c.len_utf8();
    }
    let rest = &name[end..];
    (!rest.is_empty()).then(|| rest.to_string())
}

/// `base`, or `base-2`, `base-3`… (trimmed to fit) if `base` is taken.
fn free_name(base: &AgentName, snapshot: Option<&Snapshot>, own: Option<&SessionName>) -> String {
    let taken = |name: &str| {
        name.parse::<SessionName>()
            .is_ok_and(|s| own == Some(&s) || snapshot.is_some_and(|snap| snap.entry(&s).is_some()))
    };
    if !taken(base.as_str()) {
        return base.to_string();
    }
    (2..100)
        .map(|n| {
            let suffix = format!("-{n}");
            let stem = &base.as_str()[..base
                .as_str()
                .len()
                .min(agent_core::names::MAX_SESSION_LEN - suffix.len())];
            format!("{}{suffix}", stem.trim_end_matches(['-', '.', '_']))
        })
        .find(|candidate| !taken(candidate))
        .unwrap_or_else(|| base.to_string())
}

/// The one repo, listed or in the catalogue, called exactly `name`.
fn named_repo(snapshot: Option<&Snapshot>, catalogue: &[RepoRoot], name: &str) -> Option<RepoRoot> {
    let listed = snapshot
        .into_iter()
        .flat_map(|s| s.repos().iter().filter_map(|r| r.key().root()));
    let mut named = listed
        .chain(unlisted(snapshot, catalogue))
        .filter(|root| root.name() == name);
    match (named.next(), named.next()) {
        (Some(root), None) => Some(root.clone()),
        _ => None,
    }
}

/// Repos matching `query` by name: the snapshot's, then the catalogue's
/// that aren't in it, prefix matches first.
pub fn repo_matches(
    snapshot: Option<&Snapshot>,
    catalogue: &[RepoRoot],
    query: &str,
) -> Vec<RepoRoot> {
    let query_lower = query.to_lowercase();
    let mut matches: Vec<RepoRoot> = snapshot
        .map(|s| {
            s.repos()
                .iter()
                .filter_map(|r| r.key().root())
                .filter(|root| fuzzy_match(query, &root.name()).is_some())
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    matches.extend(
        unlisted(snapshot, catalogue)
            .filter(|root| fuzzy_match(query, &root.name()).is_some())
            .cloned(),
    );
    matches.sort_by_key(|root| match_rank(&root.name(), &query_lower));
    matches
}

/// How well a name answers a lowercased query: an exact match, then a
/// prefix, then any other match. Stable sorts keep ties in list order.
pub fn match_rank(name: &str, query_lower: &str) -> u8 {
    let name = name.to_lowercase();
    if name == query_lower {
        0
    } else if name.starts_with(query_lower) {
        1
    } else {
        2
    }
}

/// The catalogue's repos the snapshot doesn't have.
pub fn unlisted<'a>(
    snapshot: Option<&'a Snapshot>,
    catalogue: &'a [RepoRoot],
) -> impl Iterator<Item = &'a RepoRoot> + 'a {
    catalogue.iter().filter(move |root| {
        snapshot.is_none_or(|s| s.repo(&RepoKey::Root((*root).clone())).is_none())
    })
}

/// Byte offsets and characters that make `name` invalid, for highlighting.
pub fn invalid_chars(name: &str) -> Vec<(usize, char)> {
    name.char_indices()
        .filter(|&(i, c)| {
            let allowed = if i == 0 {
                c.is_ascii_alphanumeric()
            } else {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')
            };
            !allowed
        })
        .collect()
}

#[cfg(test)]
impl Model {
    /// [2]'s cursor on the selected agent's subagent `id`.
    pub fn point_at_subagent(&mut self, id: &str) {
        let by = self
            .selected_entry()
            .expect("an agent is selected")
            .name()
            .clone();
        let offset = match self.panel.list() {
            Listed::Agent { offset, .. } => offset,
            _ => 0,
        };
        self.panel = Panel::Agent {
            offset,
            row: TurnRow::Subagent {
                by,
                id: AgentId::new(id),
            },
        };
    }

    /// `key`'s transcript as if it had been read.
    pub fn set_log(
        &mut self,
        key: SubagentKey,
        log: Result<Option<agent_core::ports::SubagentDetail>, String>,
    ) {
        self.track(&key.0).subagents.entry(key.1).or_default().log = Some(log);
    }

    /// How many subagents are being watched across every agent.
    pub fn watching(&self) -> usize {
        self.agents.values().map(|t| t.subagents.len()).sum()
    }

    pub fn outbox(&self, name: &SessionName) -> Option<&Outbox> {
        self.agents.get(name)?.outbox.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos_nearest_the_selection_stay_open_while_they_fit() {
        use Fold::{Closed, Open};
        assert_eq!(
            folds(&[3, 0, 4, 2], 1, 5),
            [Open, Open, Closed, Closed],
            "the repo after next doesn't fit, so nothing further opens"
        );
        assert_eq!(
            folds(&[3, 0, 4, 2], 0, 3),
            [Open, Open, Closed, Closed],
            "a selection in Needs you counts from the top"
        );
        assert_eq!(
            folds(&[3, 4], 2, 0),
            [Closed, Open],
            "the selection's repo opens without room"
        );
        assert_eq!(folds(&[2, 2, 2], 2, 6), [Open; 3]);
    }

    #[test]
    fn failures_and_tool_targets_are_read_once() {
        assert_eq!(Failure::of("Rate limit"), Failure::RateLimit);
        assert_eq!(Failure::of("server overloaded"), Failure::Overloaded);
        assert_eq!(Failure::of("billing error"), Failure::Billing);
        assert_eq!(Failure::of("boom"), Failure::Other("boom".into()));
        assert_eq!(Target::of("src/a.rs"), Target::Path("src/a.rs".into()));
        assert_eq!(Target::of("ls src/"), Target::Text("ls src/".into()));
        assert_eq!(Target::of("cargo"), Target::Text("cargo".into()));
    }

    #[test]
    fn needs_you_counts_as_the_same_place_as_any_other_row_for_its_agent() {
        let fix: SessionName = "fix".parse().expect("parses");
        let needs_you = Cursor::Grouped(Group::NeedsYou, fix.clone());
        assert!(needs_you.follows(&Cursor::Entry(fix.clone())));
        assert!(Cursor::Entry(fix.clone()).follows(&needs_you));
        let web: SessionName = "web".parse().expect("parses");
        assert!(!Cursor::Entry(web).follows(&needs_you));
    }

    #[test]
    fn the_tabs_cycle_in_order_and_back() {
        for (i, view) in View::ALL.into_iter().enumerate() {
            assert_eq!(view.next(), View::ALL[(i + 1) % View::ALL.len()]);
            assert_eq!(view.next().previous(), view);
            assert_eq!(view.previous().next(), view);
        }
    }
}
