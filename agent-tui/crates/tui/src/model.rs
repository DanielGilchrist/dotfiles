use std::collections::{HashMap, HashSet};
use std::num::{NonZeroU16, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use agent_core::actions::Actions;
use agent_core::activity::{
    Activity, AgentId, Answer as AskedBy, Hold, Origin as Asker, State, Subagent, ToolId,
};
use agent_core::config::Keys;
use agent_core::model::{
    Entry, EntryKind, EntryState, Removable, Repo, RepoKey, Revival, Running, Snapshot, Startable,
};
use agent_core::names::{
    AgentName, BranchName, Checkout, NameError, NameLimit, Prompt, SessionName,
};
use agent_core::ops::{Approved, Hazardous, RemovalPlan};
use agent_core::paths::{RepoRoot, Typed, typed_path};
use agent_core::ports::{
    ActivityDetail, CallKind, Change, ClientId, FileState, HarnessId, Observation, Place, Progress,
    Readiness, Uncommitted, WorktreeFacts,
};
use agent_term::{Emulator, Terminal};
use ratatui::layout::Rect;

use crate::cause::Cause;
use crate::document::{BranchKey, CommitKey, DiffKey, Documents, FolderKey, Hover, Key};
use crate::log::Nav;
use crate::palette::Palette;
use crate::touched::{self, Roots, TouchedFile};
use crate::track::{CachedTree, FactsEpoch, FactsRead, LiveSince};
pub use crate::track::{Inspected, Outbox, Tracked, Watched};
use crate::tree::{self, ChangesScope, ChangesView, Node, TreeRow};

pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(150);
pub const INFO_TTL: Duration = Duration::from_secs(4);

/// How long the prompt must be still before the model is asked.
pub const NAMING_DEBOUNCE: Duration = Duration::from_millis(900);
/// Prompts this short are named from their words. Asking isn't worth it.
const NAMING_MIN_WORDS: usize = 3;

/// The sidebar panel `j`/`k` act on in NAV, and how far it has moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Panel {
    /// [1]: they move the selection.
    Agents,
    /// [2]: they scroll the selected agent's details down `offset` rows
    /// and move the cursor over the activity and its subagents.
    Agent { offset: usize, row: ActivityRow },
    /// [3]: they move the cursor over the selected agent's changed files.
    Changes { file: usize },
    /// The keys are in the document [0] shows for the list panel it came
    /// from, whose cursor still says which document. The activity it was sent
    /// from by one of its entries, if it was, is where `Esc` returns to.
    Main(Origin, Option<ActivitySpot>),
}

/// Where the activity in [0] was when one of its entries took [0] elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySpot {
    pub agent: SessionName,
    pub offset: usize,
    pub row: ActivityRow,
    /// The tab [3] showed, which opening a diff may have changed.
    pub scope: ChangesScope,
}

impl ActivitySpot {
    /// The panel with the keys back on the activity.
    pub fn panel(self) -> Panel {
        Panel::Main(
            Origin::Activity {
                offset: self.offset,
                row: self.row,
            },
            None,
        )
    }
}

/// The list panel a focused document in [0] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Activity { offset: usize, row: ActivityRow },
    Changes { file: usize },
}

/// The row [2]'s cursor is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityRow {
    Main,
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
    Agent { offset: usize, row: &'a ActivityRow },
    Changes { file: usize },
}

impl Origin {
    fn into_panel(self) -> Panel {
        match self {
            Self::Activity { offset, row } => Panel::Agent { offset, row },
            Self::Changes { file } => Panel::Changes { file },
        }
    }
}

impl Panel {
    /// [2] with the cursor on the activity, scrolled to the top.
    pub const fn activity() -> Self {
        Self::Agent {
            offset: 0,
            row: ActivityRow::Main,
        }
    }

    pub fn list(&self) -> Listed<'_> {
        match self {
            Self::Agents => Listed::Agents,
            Self::Agent { offset, row } | Self::Main(Origin::Activity { offset, row }, _) => {
                Listed::Agent {
                    offset: *offset,
                    row,
                }
            }
            Self::Changes { file } | Self::Main(Origin::Changes { file }, _) => {
                Listed::Changes { file: *file }
            }
        }
    }

    /// Takes the focus from the document, leaving the list panel it came
    /// from.
    pub fn leave_document(&mut self) {
        if let Self::Main(..) = self
            && let Self::Main(from, _) = std::mem::replace(self, Self::Agents)
        {
            *self = from.into_panel();
        }
    }

    fn origin(self) -> Option<Origin> {
        match self {
            Self::Agent { offset, row } | Self::Main(Origin::Activity { offset, row }, _) => {
                Some(Origin::Activity { offset, row })
            }
            Self::Changes { file } | Self::Main(Origin::Changes { file }, _) => {
                Some(Origin::Changes { file })
            }
            Self::Agents => None,
        }
    }

    /// This panel after a layout settled `list` as its list panel: the
    /// focus on its document survives when the list panel does.
    pub fn with_list(self, list: Self) -> Self {
        match (self, list.origin()) {
            (Self::Main(_, back), Some(from)) => Self::Main(from, back),
            (_, from) => from.map_or(Self::Agents, Origin::into_panel),
        }
    }

    /// The activity the document in [0] was opened from, if one of its entries
    /// sent it here.
    pub fn back(&self) -> Option<&ActivitySpot> {
        match self {
            Self::Main(_, back) => back.as_ref(),
            Self::Agents | Self::Agent { .. } | Self::Changes { .. } => None,
        }
    }

    /// Forgets the way back to the activity.
    pub fn forget_back(&mut self) {
        if let Self::Main(_, back) = self {
            *back = None;
        }
    }

    /// Puts the keys in the panel's document. [1] has none.
    fn focus_document(&mut self) {
        let from = match std::mem::replace(self, Self::Agents) {
            Self::Agent { offset, row } => Origin::Activity { offset, row },
            Self::Changes { file } => Origin::Changes { file },
            panel => {
                *self = panel;
                return;
            }
        };
        *self = Self::Main(from, None);
    }

    pub fn in_main(&self) -> bool {
        matches!(self, Self::Main(..))
    }

    /// The row of [2]'s cursor, while [2] or its document has the keys.
    pub fn activity_row(&self) -> Option<&ActivityRow> {
        match self.list() {
            Listed::Agent { row, .. } => Some(row),
            Listed::Agents | Listed::Changes { .. } => None,
        }
    }

    fn activity_row_mut(&mut self) -> Option<&mut ActivityRow> {
        match self {
            Self::Agent { row, .. } | Self::Main(Origin::Activity { row, .. }, _) => Some(row),
            Self::Agents | Self::Changes { .. } | Self::Main(Origin::Changes { .. }, _) => None,
        }
    }
}

/// Why typing into an agent waits: what its activity shows, or what was
/// typed into it that it hasn't reported on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    Activity(Hold),
    /// A message was typed a moment ago and the agent hasn't reported since.
    JustSent,
    /// The last message was a command, which reports nothing and may open
    /// a picker.
    Command,
}

/// What a hold says wherever one is shown: `held: asking`.
pub fn held(hold: Held) -> String {
    let why = match hold {
        Held::Activity(Hold::Asking) => "asking",
        Held::Activity(Hold::Booting) => "booting",
        Held::Activity(Hold::Unreported) => "no report yet",
        Held::Activity(Hold::Ended) => "ended",
        Held::Activity(Hold::ToolStarting) => "tool starting",
        Held::JustSent => "just sent",
        Held::Command => "command sent",
    };
    format!("held: {why}")
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
    /// A repo row. Only exists in Overview.
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
        place: Placement,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Open,
    Closed,
}

/// How an entry row sits in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
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
    /// What selecting this row selects. Group headers select nothing.
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
    /// Show it in the pane. Keys stay in the sidebar.
    Preview,
    /// Show it and type into it, leaving any other window open.
    Activate,
    /// Show it and type into it, closing the other windows once connected.
    MoveHere,
}

/// What `⏎` says on an agent another window shows, and in the peek of one.
pub const MOVE_HERE: &str = "move here";
pub const LEAVE_PAUSED: &str = "leave paused";

/// Whether a session is also shown in windows that are neither this TUI's
/// nor another TUI's: a terminal tab, an editor, an SSH shell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Company {
    #[default]
    Alone,
    Elsewhere(Windows),
}

/// The windows besides this TUI's showing a session: at least one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Windows {
    first: Place,
    rest: Vec<Place>,
}

impl Company {
    /// The company of a session whose windows are in `places`, one each.
    pub fn of(places: Vec<Place>) -> Self {
        let mut places = places.into_iter();
        match places.next() {
            Some(first) => Self::Elsewhere(Windows {
                first,
                rest: places.collect(),
            }),
            None => Self::Alone,
        }
    }

    pub fn windows(&self) -> Option<&Windows> {
        match self {
            Self::Alone => None,
            Self::Elsewhere(windows) => Some(windows),
        }
    }

    pub fn is_elsewhere(&self) -> bool {
        self.windows().is_some()
    }
}

impl Windows {
    /// Windows nothing is known of, for a banner with no look behind it.
    pub fn unknown() -> Self {
        Self {
            first: Place::Unknown,
            rest: Vec::new(),
        }
    }

    /// Where the windows are, in words: `in nvim`, `over SSH`, or `in 3
    /// windows` when they aren't all in one place.
    fn phrase(&self) -> String {
        if self.rest.iter().any(|place| *place != self.first) {
            return format!("in {} windows", self.rest.len() + 1);
        }
        match &self.first {
            Place::Nvim => "in nvim".into(),
            Place::Ssh => "over SSH".into(),
            Place::Tmux => "in tmux".into(),
            Place::Terminal(app) => format!("in {app}"),
            Place::Detached => "detached window".into(),
            Place::Unknown => "elsewhere".into(),
        }
    }
}

/// What [0] and the status bar say of an agent another window shows.
pub fn elsewhere_banner(windows: &Windows) -> String {
    format!("⇄ {} · ⏎ {MOVE_HERE}", windows.phrase())
}

/// What the corner says while the keys are in such an agent, and what the
/// peek says of one.
pub fn elsewhere_note(windows: &Windows) -> String {
    format!("⇄ {}", windows.phrase())
}

/// What a look is for, which says how soon its answer is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Keeping the cards and the pane current: nothing waits on it.
    Refresh,
    /// A pane or a choice waits on it, so it isn't queued behind a refresh.
    Decision,
}

/// A look for the windows besides this TUI's and other TUIs' showing any
/// session, answered with its ticket.
pub struct Look {
    pub own: Option<ClientId>,
    pub ticket: Ticket,
    pub need: Need,
}

/// What the TUI does with a session once its check for other windows is back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterCheck {
    Preview,
    Activate,
}

impl AfterCheck {
    pub fn intent(self) -> Intent {
        match self {
            Self::Preview => Intent::Preview,
            Self::Activate => Intent::Activate,
        }
    }
}

/// Whether the last look for other windows worked, so a failure is said once
/// and not at every refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckHealth {
    Working,
    Failing,
}

/// A client of a session, sized to the pane, that keys can go to.
pub struct Connection {
    pub name: SessionName,
    pub term: Terminal<Box<dyn Emulator>>,
    pub mode: Mode,
    pub opened: Instant,
    /// The client as other windows' lookups know it, so it is never counted
    /// as one of them.
    pub id: ClientId,
}

/// The peek float. It shows whatever the pane is on, or a watcher of its
/// own for a session whose client the TUI mustn't be.
pub enum Peek {
    Pane,
    Watch(Watch),
}

pub struct Watch {
    pub name: SessionName,
    pub view: WatchView,
}

pub enum WatchView {
    /// Waiting for the size to start the watcher at.
    Watching(Ticket),
    Watched(Watcher),
}

/// A read-only view of a session another window shows: no client, so the
/// session isn't resized for it and no key reaches it.
pub struct Watcher {
    pub term: Terminal<Box<dyn Emulator>>,
    pub opened: Instant,
    /// The size the session host reported when it started, which a later look
    /// compares to follow the other window being resized.
    pub seen: (NonZeroU16, NonZeroU16),
    pub measure: Measure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Idle,
    Asked(Ticket),
}

/// What the answer to a size request means to a watch.
pub enum Reply {
    Spawn(Observation),
    Failed(Cause),
    Ignore,
}

impl Watch {
    pub fn new(name: SessionName, ticket: Ticket) -> Self {
        Self {
            name,
            view: WatchView::Watching(ticket),
        }
    }

    /// The session the watch is waiting to (re)start for `ticket`.
    pub fn awaiting(&self, ticket: Ticket) -> Option<&SessionName> {
        let waiting = match &self.view {
            WatchView::Watching(t) => *t == ticket,
            WatchView::Watched(w) => w.measure == Measure::Asked(ticket),
        };
        waiting.then_some(&self.name)
    }

    /// Asks for the size again, unless a request is out.
    pub fn measure(&mut self, ticket: Ticket) -> Option<&SessionName> {
        match &mut self.view {
            WatchView::Watched(w) if w.measure == Measure::Idle => {
                w.measure = Measure::Asked(ticket);
                Some(&self.name)
            }
            WatchView::Watching(_) | WatchView::Watched(_) => None,
        }
    }

    pub fn reply(&mut self, ticket: Ticket, result: Result<Observation, Cause>) -> Reply {
        match (&mut self.view, result) {
            (WatchView::Watching(t), Ok(found)) if *t == ticket => Reply::Spawn(found),
            (WatchView::Watching(t), Err(why)) if *t == ticket => Reply::Failed(why),
            (WatchView::Watched(w), Ok(found))
                if w.measure == Measure::Asked(ticket) && w.seen != (found.cols, found.rows) =>
            {
                Reply::Spawn(found)
            }
            (WatchView::Watched(w), _) if w.measure == Measure::Asked(ticket) => {
                w.measure = Measure::Idle;
                Reply::Ignore
            }
            _ => Reply::Ignore,
        }
    }

    /// Shows `term`. A restart keeps the time the watch first opened.
    pub fn install(
        &mut self,
        term: Terminal<Box<dyn Emulator>>,
        seen: (NonZeroU16, NonZeroU16),
        now: Instant,
    ) {
        match &mut self.view {
            WatchView::Watching(_) => {
                self.view = WatchView::Watched(Watcher {
                    term,
                    opened: now,
                    seen,
                    measure: Measure::Idle,
                });
            }
            WatchView::Watched(w) => {
                w.term = term;
                w.seen = seen;
                w.measure = Measure::Idle;
            }
        }
    }

    pub fn watcher_mut(&mut self) -> Option<&mut Watcher> {
        match &mut self.view {
            WatchView::Watched(w) => Some(w),
            WatchView::Watching(_) => None,
        }
    }
}

/// The right-hand pane. Only a live connection takes keyboard input, so
/// AGENT mode can't exist without one.
pub enum Pane {
    Empty,
    /// Asking whether another window shows the session before attaching,
    /// attaching anyway when no answer comes by `until`.
    Checking {
        name: SessionName,
        then: AfterCheck,
        until: Instant,
    },
    Connecting {
        name: SessionName,
        ticket: Ticket,
    },
    Connected(Connection),
    /// Another window shows the session, so this TUI holds no client and
    /// doesn't resize it.
    Elsewhere {
        name: SessionName,
    },
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

    pub fn set_nav(&mut self) {
        if let Some(c) = self.connection_mut() {
            c.mode = Mode::Nav;
        }
    }

    pub fn session(&self) -> Option<&SessionName> {
        match self {
            Self::Empty => None,
            Self::Checking { name, .. }
            | Self::Connecting { name, .. }
            | Self::Elsewhere { name } => Some(name),
            Self::Connected(c) => Some(&c.name),
        }
    }

    /// The session left alone because another window shows it.
    pub fn held_elsewhere(&self) -> Option<&SessionName> {
        match self {
            Self::Elsewhere { name } => Some(name),
            _ => None,
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
    /// Finished an activity the user hasn't looked at.
    Done {
        summary: Option<String>,
        since: SystemTime,
    },
    /// The user stopped the activity, and it is waiting for the next prompt.
    Interrupted {
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

/// Why an activity failed, as far as the harness's reason says.
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
    /// Its activity ended and it waits on this many background subagents.
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
            Self::NoSession
            | Self::Exited
            | Self::Idle
            | Self::Working { .. }
            | Self::Interrupted { .. } => None,
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

impl Reach {
    /// Whether `⏎` would attach to a live session.
    pub fn is_attach(&self) -> bool {
        matches!(
            self,
            Self::Other {
                enter: Enter::Attach,
                ..
            }
        )
    }
}

/// What ⏎ does to an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enter {
    Attach,
    /// Start an exited agent again through `agent`, resuming its
    /// conversation.
    Restart(Startable),
    /// Let the session host replay a session we don't own the checkout of.
    Resurrect,
    Start(Startable),
    /// The session host can't take the agent's name.
    Refused(NameError),
}

impl Enter {
    /// What `⏎` says, or nothing when it can't do anything.
    pub fn verb(&self) -> Option<&'static str> {
        match self {
            Self::Attach => Some("attach"),
            Self::Restart(_) | Self::Resurrect => Some("resurrect"),
            Self::Start(_) => Some("start"),
            Self::Refused(_) => None,
        }
    }
}

/// A filter and a selection within its matches. `offset` is view state:
/// the first visible row, kept stable while the selection moves.
#[derive(Debug, Default, Clone)]
pub struct Query {
    pub text: String,
    index: usize,
    pub offset: usize,
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

/// An agent that can be messaged, with the activity that says when.
pub struct Reachable<'a> {
    name: &'a SessionName,
    activity: &'a Activity,
}

impl<'a> Reachable<'a> {
    pub fn name(&self) -> &'a SessionName {
        self.name
    }
}

/// Why `s` can't message an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreachable {
    NotRunning,
    Own,
    /// No hook activity: whether a menu is open can't be known.
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
    pub offset: usize,
}

/// A comment being typed on lines of a diff.
pub struct CommentDraft {
    pub to: SessionName,
    pub lines: Vec<crate::review::LineInfo>,
    pub text: String,
    /// The commit the lines are from, when they aren't the checkout's.
    pub rev: Option<crate::review::Rev>,
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
    Comment(CommentDraft),
    /// Working out what removing `name` would lose. The confirmation
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

/// The time as one read of both clocks, so a message's wall-clock and
/// monotonic readings agree.
#[derive(Debug, Clone, Copy)]
pub struct Moment {
    pub wall: SystemTime,
    pub mono: Instant,
}

impl Moment {
    pub fn now() -> Self {
        Self {
            wall: SystemTime::now(),
            mono: Instant::now(),
        }
    }
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
    /// How [0]'s document was laid out, while it shows one.
    pub main: Option<MainLayout>,
}

/// How [0]'s document was laid out the last time it was drawn: what the keys
/// that move over it need to know without drawing.
#[derive(Debug, Default)]
pub struct MainLayout {
    /// Rows in the document, and how many show under the rows that stay put.
    pub total: usize,
    pub height: usize,
    /// A log, which follows its end.
    pub tails: bool,
    /// A log's entries and their rows, and the log they belong to.
    pub entries: Vec<crate::log::EntryRows>,
    pub nav: Option<LogKey>,
    /// The document, if it is a diff whose lines the cursor moves over.
    pub diff: Option<crate::document::Slot>,
}

impl MainLayout {
    /// The first row on screen once `docs` has been placed in this layout.
    pub fn top(&self, docs: &Documents) -> usize {
        docs.placed(self.total, self.height, self.tails).top
    }

    /// The entry on screen's first row, or the last one.
    pub fn entry_at(&self, top: usize) -> Option<crate::log::EntryKey> {
        self.entries
            .iter()
            .find(|e| e.start + e.len > top)
            .or_else(|| self.entries.last())
            .map(|e| e.key.clone())
    }

    /// The entry a cursor that is just getting the keys starts on: the one it
    /// is already on if that shows, else the last showing while the log
    /// follows its end and the first showing otherwise.
    pub fn entry_to_start_on(
        &self,
        docs: &Documents,
        on: Option<&crate::log::EntryKey>,
    ) -> Option<crate::log::EntryKey> {
        let placed = docs.placed(self.total, self.height, self.tails);
        let shows = |e: &&crate::log::EntryRows| {
            e.start < placed.top + self.height && e.start + e.len > placed.top
        };
        if let Some(key) =
            on.filter(|key| self.entries.iter().filter(shows).any(|e| &e.key == *key))
        {
            return Some(key.clone());
        }
        let mut showing = self.entries.iter().filter(shows);
        let pick = if placed.following() {
            showing.next_back()
        } else {
            showing.next()
        };
        pick.map(|e| e.key.clone())
            .or_else(|| self.entry_at(placed.top))
    }

    /// Scrolls `docs` so the log's entry `key` shows.
    pub fn reveal_entry(&self, docs: &mut Documents, key: &crate::log::EntryKey) {
        let Some(rows) = self.entries.iter().find(|e| &e.key == key) else {
            return;
        };
        docs.reveal(rows.start, rows.start + rows.len, self.height);
    }
}

/// What a draw leaves for the model to keep: layout facts and the rows it
/// built, never decisions. [`crate::update::drawn`] applies it, so the draw
/// itself only reads the model.
#[derive(Default)]
pub struct Drawn {
    pub areas: Areas,
    /// Whether [3] fits, when the sidebar was laid out.
    pub work_room: Option<WorkRoom>,
    /// Where the list scrolled to, when it was laid out.
    pub scroll: Option<ListScroll>,
    /// The first row an overlay's list showed, kept so the list stays
    /// stable while the selection moves.
    pub overlay_offset: Option<usize>,
    /// The document's rows, when they were built for this frame.
    pub rows: Option<crate::document::Laid>,
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

/// What [0] opened from the branch view: a commit's diff, or the diff of an
/// uncommitted file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    Commit { sha: String, subject: String },
    File { path: PathBuf, state: FileState },
}

/// Why the activity view couldn't take [0] where an entry points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missing {
    /// The subagent isn't one its agent lists any more.
    Subagent,
    /// Neither tab of [3] lists the file.
    File(PathBuf),
    /// [3] isn't shown at this height.
    Room,
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Subagent => f.write_str("that subagent isn't listed any more"),
            Self::File(path) => write!(f, "{} has no diff in [3]", path.display()),
            Self::Room => f.write_str("no room for [3] at this height"),
        }
    }
}

/// What the activity in [0] has that takes more than a lookup to work out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityFiles {
    pub agent: SessionName,
    pub touched: Vec<TouchedFile>,
    /// Whether the activity lists them as rows or leaves the status row to
    /// count them.
    pub shown: Shown,
    pub jump: Option<crate::log::Jump>,
}

/// How an activity says the files it touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// The activity is going, so its status row only counts them.
    Counted,
    Listed,
}

impl ActivityFiles {
    /// The files the activity lists as rows.
    pub fn listed(&self) -> &[TouchedFile] {
        match self.shown {
            Shown::Listed => &self.touched,
            Shown::Counted => &[],
        }
    }
}

/// A subagent by its agent and its id.
pub type SubagentKey = (SessionName, AgentId);

/// What an agent that waits on the user is asking about, as far as its
/// activity can place the question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asking {
    /// A call that puts questions to the user.
    Choice {
        call: ToolId,
        questions: Vec<agent_core::ports::Question>,
    },
    /// Leave to run `call`.
    Permission { call: ToolId },
    /// One of its subagents asks.
    Subagent(AgentId),
}

/// A log [0] reads and the cursor moves over: a subagent's, or the current
/// activity of an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogKey {
    Subagent(SubagentKey),
    Activity(SessionName),
}

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
    /// Most recently used first. No duplicates.
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
    /// The newest look for other windows, the only one whose answer counts.
    latest_look: Option<Ticket>,
    pub check_health: CheckHealth,
    pub actions: Actions,
    pub keys: Keys,
    pub palette: Palette,
    pub name_limit: NameLimit,
    /// `$HOME`, read once at startup to abbreviate paths.
    pub home: Option<PathBuf>,
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
    /// The time this message or tick arrived: wall for rows, counts and
    /// titles, monotonic for debounces and timeouts, so everything one
    /// message decides agrees. Set by whatever delivers the message.
    pub clock: Moment,
    /// The terminal's size, which the layout is a function of.
    pub size: Rect,
    /// How [3] lists the changed files, for the whole session.
    pub changes_view: ChangesView,
    pub changes_scope: ChangesScope,
    /// What is known about each agent, dropped when it leaves the snapshot.
    pub agents: HashMap<SessionName, Tracked>,
    /// The finished activity last marked seen, so it's marked once.
    pub acknowledged: Option<(SessionName, u64)>,
    pub last_bell: Option<Instant>,
    /// A bell held back by the rate limit, to ring when it allows.
    pub bell_due: Option<Instant>,
    /// When to look for other windows again, set by an action that may have
    /// opened one.
    pub elsewhere_due: Option<Instant>,
    pub terminal: TerminalFocus,
    /// Which checkouts [1] lists.
    pub visibility: Visibility,
    facts_reads: FactsEpoch,
    /// [0]'s diff: the cursor over its lines.
    pub diff_view: crate::review::DiffView,
    /// What the branch view opened in [0], until `Esc` goes back to it.
    pub opened: Option<Opened>,
    /// What the activity in [0] has that takes more than a lookup, as of the
    /// last message.
    pub activity_files: Option<ActivityFiles>,
    /// The agent whose terminal the peek float shows.
    pub peek: Option<Peek>,
    /// What the main view [0] reads when it isn't the terminal.
    pub docs: Documents,
    pub screen: Screen,
}

impl Model {
    pub fn new(
        start_dir: PathBuf,
        own_session: Option<SessionName>,
        harness: HarnessId,
        harness_label: String,
        name_limit: NameLimit,
        home: Option<PathBuf>,
    ) -> Self {
        Self {
            name_limit,
            home,
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
            latest_look: None,
            check_health: CheckHealth::Working,
            actions: Actions::default(),
            keys: Keys::default(),
            palette: Palette::DEFAULT,
            catalogue: Vec::new(),
            catalogue_warnings: Vec::new(),
            snapshots: 0,
            activities: None,
            clock: Moment::now(),
            size: Rect::default(),
            agents: HashMap::new(),
            changes_view: ChangesView::default(),
            changes_scope: ChangesScope::default(),
            acknowledged: None,
            last_bell: None,
            bell_due: None,
            elsewhere_due: None,
            terminal: TerminalFocus::Focused,
            visibility: Visibility::Active,
            facts_reads: FactsEpoch::ORIGIN,
            diff_view: crate::review::DiffView::default(),
            opened: None,
            activity_files: None,
            peek: None,
            docs: Documents::default(),
            screen: Screen::Split,
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

    /// A ticket for a new look for other windows. Only the newest look's
    /// answer counts, so asking again drops the answers still on their way.
    pub fn start_look(&mut self) -> Ticket {
        let ticket = self.ticket();
        self.latest_look = Some(ticket);
        ticket
    }

    pub fn is_latest_look(&self, ticket: Ticket) -> bool {
        self.latest_look == Some(ticket)
    }

    #[cfg(test)]
    pub fn current_look(&self) -> Option<Ticket> {
        self.latest_look
    }

    /// Whether `name` is a live agent a client may attach to, as opposed to
    /// one that exited, which attaching would resurrect.
    pub fn attachable(&self, name: &SessionName) -> bool {
        self.snapshot()
            .and_then(|s| s.entry(name))
            .is_some_and(|(_, e)| self.reach(e).is_attach())
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
            Panel::Main(..) => {}
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

    /// Half the rows of [0], at least one.
    pub fn doc_half_page(&self) -> isize {
        let rows = self.areas.main.as_ref().map_or(0, |main| main.height);
        isize::try_from(rows / 2).unwrap_or(isize::MAX).max(1)
    }

    /// Half the focused panel's rows, at least one.
    pub fn half_page(&self) -> isize {
        isize::try_from(self.areas.page / 2)
            .unwrap_or(isize::MAX)
            .max(1)
    }

    pub fn panel_to_edge(&mut self, edge: Edge) {
        match &self.panel {
            Panel::Main(..) => {}
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

    /// The folders and files [3] lists under the readiness row: the tree
    /// built when the model last settled, or built now if it hasn't.
    pub fn change_list(&self) -> std::sync::Arc<Vec<TreeRow>> {
        let Some(name) = self.selected_entry().map(Entry::name) else {
            return std::sync::Arc::new(Vec::new());
        };
        let Some(tracked) = self.agents.get(name) else {
            return std::sync::Arc::new(Vec::new());
        };
        match &tracked.tree {
            Some(cached) if self.tree_fresh(name) => cached.rows.clone(),
            _ => std::sync::Arc::new(tree::build(
                self.changes(),
                self.changes_view,
                &tracked.collapsed,
            )),
        }
    }

    /// The windows besides this TUI's showing `name`, as the newest look
    /// found them.
    pub fn company(&self, name: &SessionName) -> &Company {
        static ALONE: Company = Company::Alone;
        self.agents.get(name).map_or(&ALONE, |t| &t.company)
    }

    /// Notes what a look found of `name`. An agent nothing is known of
    /// isn't tracked just to be alone.
    pub fn record_company(&mut self, name: &SessionName, company: Company) {
        if company == Company::Alone && !self.agents.contains_key(name) {
            return;
        }
        self.track(name).company = company;
    }

    /// Whether `name` is known to be shown in another window, which the pane
    /// leaves alone or squishes.
    pub fn elsewhere(&self, name: &SessionName) -> bool {
        let held = self.pane.held_elsewhere() == Some(name);
        let connected = self.pane.connection().is_some_and(|c| &c.name == name);
        (held || connected) && self.company(name).is_elsewhere()
    }

    /// The connection in NAV to a session another window shows: it squishes
    /// that window, and the pane gives it up as soon as no peek needs it.
    fn nav_client_elsewhere(&self) -> Option<&Connection> {
        self.pane
            .connection()
            .filter(|c| c.mode == Mode::Nav && self.company(&c.name).is_elsewhere())
    }

    /// The session the peek float is over.
    pub fn peek_name(&self) -> Option<&SessionName> {
        match &self.peek {
            Some(Peek::Pane) => self.pane.session(),
            Some(Peek::Watch(watch)) => Some(&watch.name),
            None => None,
        }
    }

    pub fn peek_over(&self, name: &SessionName) -> bool {
        self.peek_name() == Some(name)
    }

    /// The only way to change the pane, which closes a float showing it once
    /// the pane is on another session or none.
    pub fn set_pane(&mut self, pane: Pane) {
        if matches!(self.peek, Some(Peek::Pane)) && self.pane.session() != pane.session() {
            self.peek = None;
        }
        self.pane = pane;
    }

    /// Stops waiting for `ticket`'s connection, if the pane still is.
    pub fn abandon(&mut self, ticket: Ticket) {
        if self.pane.awaiting(ticket).is_some() {
            self.set_pane(Pane::Empty);
        }
    }

    pub fn watch_mut(&mut self) -> Option<&mut Watch> {
        match &mut self.peek {
            Some(Peek::Watch(watch)) => Some(watch),
            Some(Peek::Pane) | None => None,
        }
    }

    /// Closes the float and hands back its watcher.
    pub fn take_watcher(&mut self) -> Option<(SessionName, Watcher)> {
        match self.peek.take() {
            Some(Peek::Watch(Watch {
                name,
                view: WatchView::Watched(watcher),
            })) => Some((name, watcher)),
            other => {
                self.peek = other;
                None
            }
        }
    }

    /// Whether the pane's client squishes another window only for the peek
    /// over it, which a watcher can serve instead.
    pub fn peek_client_squishes(&self) -> bool {
        matches!(self.peek, Some(Peek::Pane)) && self.nav_client_elsewhere().is_some()
    }

    /// The session whose client the pane should give up now: one shared with
    /// another window while no peek is over it.
    pub fn client_to_release(&self) -> Option<SessionName> {
        self.nav_client_elsewhere()
            .filter(|c| !self.peek_over(&c.name))
            .map(|c| c.name.clone())
    }

    /// The windows that squish what the TUI types into, while it is typing
    /// into `name` and another window shows it.
    pub fn squished(&self, name: &SessionName) -> Option<&Windows> {
        self.pane
            .connection()
            .filter(|c| &c.name == name && c.mode == Mode::Agent)
            .and_then(|_| self.company(name).windows())
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

    /// The cursor and folds of the log `key` names.
    pub fn nav(&self, key: &LogKey) -> Option<&Nav> {
        match key {
            LogKey::Subagent(key) => self.watched(key).map(|w| &w.watch.nav),
            LogKey::Activity(name) => self.agents.get(name).map(|t| &t.activity_view.watch.nav),
        }
    }

    pub fn nav_mut(&mut self, key: &LogKey) -> Option<&mut Nav> {
        match key {
            LogKey::Subagent((name, id)) => self
                .agents
                .get_mut(name)?
                .subagents
                .get_mut(id)
                .map(|w| &mut w.watch.nav),
            LogKey::Activity(name) => self
                .agents
                .get_mut(name)
                .map(|t| &mut t.activity_view.watch.nav),
        }
    }

    /// Whether `name` is at work on its activity, so its log grows: working,
    /// delegating or waiting on an answer.
    pub fn activity_running(&self, name: &SessionName) -> bool {
        self.activity(name)
            .is_some_and(|a| a.waiting().is_some() || a.state.is_going())
    }

    /// What `name` is asking about, matched by tool id to its activity. `None`
    /// when it asks nothing or the activity hasn't caught up with the ask.
    pub fn asking(&self, name: &SessionName) -> Option<Asking> {
        let ask = self.activity(name)?.waiting()?;
        if let Asker::Subagent(id) = &ask.asker {
            return Some(Asking::Subagent(id.clone()));
        }
        let AskedBy::ToolEnding(call) = &ask.answered_by else {
            return None;
        };
        let detail = self.activity_detail(name)?;
        if let Progress::Running { asked: Some(asked) } = &detail.progress
            && asked.call == *call
        {
            return Some(Asking::Choice {
                call: call.clone(),
                questions: asked.questions.clone(),
            });
        }
        detail
            .log
            .log
            .iter()
            .find_map(|e| match e {
                agent_core::ports::LogEntry::Called(c) if c.id == *call => Some(c),
                _ => None,
            })
            .filter(|c| c.kind != CallKind::Ask)
            .map(|c| Asking::Permission { call: c.id.clone() })
    }

    /// What `name` ended its activity with, once it has: the message its hook
    /// reported, or else the transcript's last line. It is shown apart from
    /// the activity's log.
    pub fn closing(&self, name: &SessionName) -> Option<String> {
        let reported = || match &self.activity_detail(name)?.log.outcome {
            agent_core::ports::Outcome::Finished { report } => report.clone(),
            _ => None,
        };
        match &self.activity(name)?.state {
            State::Done { summary } => summary.clone().or_else(reported),
            State::Idle | State::Exited => reported(),
            State::Working { .. }
            | State::Delegating
            | State::Failed { .. }
            | State::Interrupted => None,
        }
    }

    /// Whether [0] has a log of `name`'s activity to move over, as opposed to the
    /// hooks' account of it.
    pub fn activity_has_log(&self, name: &SessionName) -> bool {
        self.activity_detail(name)
            .is_some_and(|t| t.log.prompt.is_some() || !t.log.log.is_empty())
    }

    /// `name`'s current activity as last read, if it was read and found.
    pub fn activity_detail(&self, name: &SessionName) -> Option<&ActivityDetail> {
        match self.agents.get(name)?.activity_view.watch.log.as_ref()? {
            Ok(Some(detail)) => Some(detail),
            Ok(None) | Err(_) => None,
        }
    }

    /// `name`'s git facts, or why they couldn't be read, as last read.
    pub fn facts(&self, name: &SessionName) -> Option<Result<&WorktreeFacts, &Cause>> {
        let read = self.agents.get(name)?.facts.as_ref()?;
        Some(read.result.as_ref().map(|i| &i.facts))
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

    /// Records `name`'s freshly read facts: the epoch moves on, the
    /// closed folders that no longer exist are forgotten, and [3]'s cursor
    /// stays on the row it was on.
    pub fn set_facts(&mut self, name: &SessionName, read: Result<Inspected, Cause>) {
        let selected = self.selected_entry().is_some_and(|e| e.name() == name);
        let anchor = selected.then(|| self.changes_anchor()).flatten();
        self.facts_reads = self.facts_reads.next();
        let epoch = self.facts_reads;
        let tracked = self.track(name);
        match &read {
            Ok(read) => {
                let mut existing = tree::folders(&read.facts.changed);
                existing.extend(tree::folders(&read.facts.pending));
                tracked.collapsed.retain(|p| existing.contains(p));
            }
            Err(_) => tracked.collapsed.clear(),
        }
        tracked.facts = Some(FactsRead {
            epoch,
            result: read,
        });
        if let Some(anchor) = anchor {
            self.refresh_tree();
            self.restore_changes_anchor(&anchor);
        }
    }

    /// Facts as if they had been read, for tests that don't care how.
    #[cfg(test)]
    pub fn set_test_facts(&mut self, name: &SessionName, facts: Result<WorktreeFacts, Cause>) {
        self.set_facts(name, facts.map(Inspected::of));
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
            Panel::Changes { file } | Panel::Main(Origin::Changes { file }, _) => *file = to,
            _ => {}
        }
    }

    /// Moves [3]'s cursor to the row for `path`, or the nearest row that
    /// stands for it: the first under it, or the folder that holds it.
    /// Whether there was one.
    fn point_changes_at(&mut self, path: &Path) -> bool {
        let row = self.changes_row_of(path);
        if let Some(row) = row {
            self.set_changes_file(row);
        }
        row.is_some()
    }

    /// The row of [3] that stands for `path`: its own, the first under it, or
    /// the folder that holds it.
    fn changes_row_of(&self, path: &Path) -> Option<usize> {
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
        at.map(|at| at + skip)
    }

    /// What [3]'s cursor is on, if [3] has it.
    pub fn changes_anchor(&self) -> Option<RowId> {
        let (Panel::Changes { file } | Panel::Main(Origin::Changes { file }, _)) = self.panel
        else {
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
        let (Panel::Changes { file } | Panel::Main(Origin::Changes { file }, _)) = self.panel
        else {
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

    /// Moves the keys from the activity at `spot` to [3] on `file`, so [0] shows
    /// its diff. The tab that lists the file is the one shown, and the
    /// folders above it open. `Esc` comes back to the activity.
    pub fn open_diff(&mut self, spot: ActivitySpot, file: &Path) -> Result<(), Missing> {
        self.require_room()?;
        let name = spot.agent.clone();
        let Some(Ok(facts)) = self.facts(&name) else {
            return Err(Missing::File(file.to_path_buf()));
        };
        let scope = [self.changes_scope, self.changes_scope.toggled()]
            .into_iter()
            .find(|scope| scope.lists(facts, file))
            .ok_or_else(|| Missing::File(file.to_path_buf()))?;
        self.changes_scope = scope;
        let above: Vec<PathBuf> = file.ancestors().skip(1).map(Path::to_path_buf).collect();
        self.track(&name).open_folders(&above);
        self.refresh_tree();
        let row = self
            .changes_row_of(file)
            .ok_or_else(|| Missing::File(file.to_path_buf()))?;
        self.panel = Panel::Main(Origin::Changes { file: row }, Some(spot));
        self.opened = None;
        Ok(())
    }

    /// Whether [3] is shown at this height, which the keys that move to it
    /// need.
    pub fn require_room(&self) -> Result<(), Missing> {
        match self.work_room {
            WorkRoom::Fits => Ok(()),
            WorkRoom::TooShort => Err(Missing::Room),
        }
    }

    /// Where the activity in [0] is, if the keys are on it.
    pub fn activity_spot(&self) -> Option<ActivitySpot> {
        let Panel::Main(Origin::Activity { offset, row }, _) = &self.panel else {
            return None;
        };
        Some(ActivitySpot {
            agent: self.selected_entry()?.name().clone(),
            offset: *offset,
            row: row.clone(),
            scope: self.changes_scope,
        })
    }

    /// Where `name`'s checkout is, for telling paths in it from others.
    pub fn roots(&self, name: &SessionName) -> Option<Roots> {
        let (_, entry) = self.snapshot()?.entry(name)?;
        let real = match &self.tracked(name)?.facts.as_ref()?.result {
            Ok(read) => read.real_root.clone(),
            Err(_) => None,
        };
        Some(Roots::new(entry.path()?, real))
    }

    /// Works out what the activity in [0] has that takes more than a lookup,
    /// once per message: the files it touched and where `⏎` would go.
    fn read_activity_files(&self, name: &SessionName) -> Option<ActivityFiles> {
        let detail = self.activity_detail(name)?;
        let roots = self.roots(name)?;
        let tracked = self.tracked(name)?;
        let facts = tracked
            .facts
            .as_ref()
            .and_then(|r| r.result.as_ref().ok())
            .map(|i| &i.facts);
        let touched = touched::files(&detail.log, &roots, facts, &tracked.baseline);
        let running = self.activity_running(name);
        let shown = if running {
            Shown::Counted
        } else {
            Shown::Listed
        };
        let mut files = ActivityFiles {
            agent: name.clone(),
            touched,
            shown,
            jump: None,
        };
        let closing = self.closing(name);
        let entries =
            crate::log::activity_entries(&detail.log, closing.as_deref(), files.listed()).flat();
        files.jump = (!entries.is_empty())
            .then(|| tracked.activity_view.watch.nav.at(running, &entries))
            .and_then(|at| entries.get(at))
            .and_then(|(entry, _)| crate::log::jump_of(entry, &roots));
        Some(files)
    }

    /// What the activity in [0] has that takes more than a lookup, when it
    /// is `name`'s.
    pub fn activity_files(&self, name: &SessionName) -> Option<&ActivityFiles> {
        self.activity_files.as_ref().filter(|f| &f.agent == name)
    }

    /// The files `name`'s activity touched, when it is the activity in [0].
    #[cfg(test)]
    pub fn touched(&self, name: &SessionName) -> Option<&[TouchedFile]> {
        Some(&self.activity_files(name)?.touched)
    }

    /// The files the activity lists as rows, when it is the activity in [0].
    #[cfg(test)]
    pub fn listed(&self, name: &SessionName) -> Option<&[TouchedFile]> {
        Some(self.activity_files(name)?.listed())
    }

    /// Where `⏎` would take [0] from the entry under the activity's cursor.
    pub fn activity_jump(&self) -> Option<&crate::log::Jump> {
        self.activity_files.as_ref()?.jump.as_ref()
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
        let tracked = self.track(&name);
        if matches!(folder.node, Node::Folder { open: false }) {
            tracked.open_folders(&folder.chain);
        } else {
            tracked.collapsed.insert(folder.path);
        }
        self.refresh_tree();
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
        self.refresh_tree();
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
        let epoch = self.tracked(&name)?.facts.as_ref()?.epoch;
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
                    epoch,
                }),
            };
            return Some((hover, root));
        }
        let folder = self.folder_under_cursor().map(|r| r.path);
        let hover = match (self.change_at(row), folder, self.facts(&name)) {
            (None, None, _) if !self.branch_row() => return None,
            (None, Some(path), _) => Hover::Folder(FolderKey {
                files: self.folder_files(&path),
                base: self.changes_scope.diff_base(),
                name,
                path,
                epoch,
            }),
            (Some(change), _, _) => Hover::Diff(DiffKey {
                name,
                change: change.clone(),
                base: self.changes_scope.diff_base(),
                epoch,
            }),
            (None, None, Some(Ok(facts))) => Hover::Branch(BranchKey {
                name,
                ahead: facts.ahead,
                behind: facts.behind,
                dirty: facts.dirty(),
                compared: facts.compared.clone(),
                epoch,
            }),
            (None, None, _) => return None,
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
            Hover::Activity(_) | Hover::Subagent(..) | Hover::Repo(_) => return None,
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

    /// Scrolls so the diff cursor's lines show, as the last draw laid the
    /// document out.
    pub fn reveal_line(&mut self) {
        let Some(main) = self.areas.main.as_ref().filter(|main| main.diff.is_some()) else {
            return;
        };
        let (from, to) = self.diff_view.span();
        let lines = self.docs.lines();
        let (Some(first), Some(last)) = (lines.get(from), lines.get(to)) else {
            return;
        };
        let height = main.height;
        self.docs.reveal(first.row, last.row + last.height, height);
    }

    /// Puts the keys in [0] with the cursor on what shows, so the first key
    /// moves from there rather than from wherever it was left or from an
    /// end the screen isn't at.
    pub fn focus_document(&mut self) {
        self.panel.focus_document();
        let Some(main) = &self.areas.main else {
            return;
        };
        let placed = self.docs.placed(main.total, main.height, main.tails);
        let key = main.nav.clone();
        let on = key
            .as_ref()
            .and_then(|key| self.nav(key))
            .and_then(|nav| nav.cursor.clone());
        let entry = main.entry_to_start_on(&self.docs, on.as_ref());
        if main.diff.is_some() {
            let lines = self.docs.lines();
            let showing = |l: &crate::review::LineInfo| {
                l.row < placed.top + main.height && l.row + l.height > placed.top
            };
            if !lines.get(self.diff_view.cursor).is_some_and(showing) {
                let at = lines
                    .iter()
                    .position(|l| l.selectable() && l.row + l.height > placed.top);
                if let Some(at) = at {
                    self.diff_view.cursor = at;
                    self.diff_view.anchor = None;
                }
            }
        }
        if let (Some(key), Some(entry)) = (key, entry)
            && let Some(nav) = self.nav_mut(&key)
        {
            nav.cursor = Some(entry);
        }
    }

    /// Moves the cursor onto the first line or entry on screen after a page
    /// scroll, as the last draw laid the document out.
    pub fn follow_page(&mut self) {
        let Some(main) = &self.areas.main else {
            return;
        };
        let top = main.top(&self.docs);
        let entry = main.entry_at(top);
        let key = main.nav.clone();
        if main.diff.is_some() {
            let lines = self.docs.lines();
            let at = lines
                .iter()
                .position(|l| l.selectable() && l.row + l.height > top)
                .or_else(|| crate::review::last(lines));
            if let Some(at) = at {
                self.diff_view.cursor = at;
                self.diff_view.anchor = None;
            }
        }
        if let Some(key) = key
            && let Some(nav) = self.nav_mut(&key)
        {
            nav.cursor = entry;
        }
    }

    pub fn fit_sidebar(&mut self, list: Rect) {
        self.areas.sidebar = list;
    }

    /// Brings what follows the cursor and the facts up to date, so a draw
    /// only reads: [0] points at what the cursor is on, and [3]'s tree is
    /// built.
    pub fn settle(&mut self) {
        let hover = self.hover();
        if hover.is_none() {
            self.panel.leave_document();
        }
        if let Some(spot) = self.panel.back()
            && self
                .selected_entry()
                .is_none_or(|e| e.name() != &spot.agent)
        {
            self.panel.forget_back();
        }
        self.docs.settle(hover.as_ref(), self.clock.mono);
        self.refresh_tree();
        self.activity_files = match &hover {
            Some(Hover::Activity(name)) => self.read_activity_files(name),
            _ => None,
        };
    }

    /// Builds the selected agent's tree if its facts, the view, the scope
    /// or its folds changed since it was.
    fn refresh_tree(&mut self) {
        let Some(name) = self.selected_entry().map(|e| e.name().clone()) else {
            return;
        };
        if self.tree_fresh(&name) {
            return;
        }
        let Some(tracked) = self.agents.get(&name) else {
            return;
        };
        let cached = CachedTree {
            epoch: tracked.facts.as_ref().map(|f| f.epoch),
            view: self.changes_view,
            scope: self.changes_scope,
            collapsed: tracked.collapsed.clone(),
            rows: std::sync::Arc::new(tree::build(
                self.changes(),
                self.changes_view,
                &tracked.collapsed,
            )),
        };
        if let Some(tracked) = self.agents.get_mut(&name) {
            tracked.tree = Some(cached);
        }
    }

    fn tree_fresh(&self, name: &SessionName) -> bool {
        self.agents
            .get(name)
            .and_then(|t| Some((t, t.tree.as_ref()?)))
            .is_some_and(|(t, c)| {
                c.epoch == t.facts.as_ref().map(|f| f.epoch)
                    && c.view == self.changes_view
                    && c.scope == self.changes_scope
                    && c.collapsed == t.collapsed
            })
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
            EntryState::Idle(_) | EntryState::Unhostable(_) => return AgentStatus::NoSession,
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
            State::Idle | State::Exited => AgentStatus::Idle,
            State::Working { .. } | State::Delegating => {
                let pulse = if activity.stale(self.clock.wall) {
                    Pulse::Stale
                } else {
                    Pulse::Steady
                };
                let doing = match (&activity.state, activity.current_tool()) {
                    (State::Delegating, _) => NonZeroUsize::new(activity.running())
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
            State::Failed { reason } => AgentStatus::Failed {
                failure: Failure::of(reason),
                since,
            },
            State::Done { summary } => AgentStatus::Done {
                summary: summary.clone(),
                since,
            },
            State::Interrupted => AgentStatus::Interrupted { since },
        }
    }

    /// The agent `s` may message: it's live, isn't the one this TUI runs in,
    /// and reports through hooks, which is the only way to know when a menu
    /// is open in it.
    pub fn reachable<'a>(&'a self, entry: &'a Entry) -> Result<Reachable<'a>, Unreachable> {
        let EntryState::Live = entry.state() else {
            return Err(Unreachable::NotRunning);
        };
        if self.own_session.as_ref() == Some(entry.name()) {
            return Err(Unreachable::Own);
        }
        let activity = self.activity(entry.name()).ok_or(Unreachable::Unreported)?;
        Ok(Reachable {
            name: entry.name(),
            activity,
        })
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

    /// Why text typed now into `name` could land somewhere other than its
    /// prompt, if it could: what its activity shows, or what was typed into
    /// it a moment ago that it hasn't reported on.
    pub fn hold(&self, name: &SessionName) -> Option<Held> {
        Hold::of(self.activity(name), self.not_before(name), self.clock.wall)
            .map(Held::Activity)
            .or_else(|| self.sent_hold(name))
    }

    /// [`Self::hold`] for an agent known to have an activity.
    pub fn hold_of(&self, to: &Reachable<'_>) -> Option<Held> {
        to.activity
            .hold(self.not_before(to.name), self.clock.wall)
            .map(Held::Activity)
            .or_else(|| self.sent_hold(to.name))
    }

    fn sent_hold(&self, name: &SessionName) -> Option<Held> {
        self.tracked(name)?
            .sent
            .as_ref()?
            .hold(self.activity(name).map(|a| a.events), self.clock.mono)
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
        if let Some(ActivityRow::Subagent { by, id }) = self.panel.activity_row()
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
    pub fn hold_activity_row(&mut self) {
        let Some(ActivityRow::Subagent { by, id }) = self.panel.activity_row() else {
            return;
        };
        let live = self
            .selected_entry()
            .is_some_and(|e| e.name() == by && matches!(e.state(), EntryState::Live));
        let listed = live
            .then(|| self.activity(by).and_then(|a| a.subagents.get(id)).cloned())
            .flatten();
        let now = self.clock.wall;
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

    /// Where [2]'s cursor is among its rows: 0 is the activity, then the
    /// subagents. The activity while it points at a subagent that's gone.
    pub fn row_at(&self) -> usize {
        let Some(ActivityRow::Subagent { id, .. }) = self.panel.activity_row() else {
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
            if let Some(row) = self.panel.activity_row_mut() {
                *row = ActivityRow::Main;
            }
            return;
        };
        self.track(&by).subagents.entry(id.clone()).or_default();
        if let Some(row) = self.panel.activity_row_mut() {
            *row = ActivityRow::Subagent { by, id };
        }
        self.hold_activity_row();
    }

    /// Points [0] at the subagent `id`, as [2]'s cursor on it would, from
    /// the activity at `spot`. `Esc` comes back to the activity.
    pub fn open_subagent(&mut self, spot: ActivitySpot, id: &AgentId) -> Result<(), Missing> {
        if !self.subagent_rows().iter().any(|(listed, _)| *listed == id) {
            return Err(Missing::Subagent);
        }
        let by = spot.agent.clone();
        self.panel = Panel::Main(
            Origin::Activity {
                offset: spot.offset,
                row: ActivityRow::Subagent {
                    by: by.clone(),
                    id: id.clone(),
                },
            },
            Some(spot),
        );
        self.track(&by).subagents.entry(id.clone()).or_default();
        self.hold_activity_row();
        Ok(())
    }

    /// The subagent [2]'s cursor is on, while [2] is focused.
    pub fn subagent_selected(&self) -> Option<(SessionName, AgentId)> {
        self.panel.activity_row()?;
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
                _ => self.elsewhere_activity(),
            },
            Listed::Changes { .. } => self.changes_hover().map(|(hover, _)| hover),
            Listed::Agent { .. } => {
                let entry = self.selected_entry()?;
                self.activity(entry.name())?;
                let name = entry.name().clone();
                Some(match self.subagent_selected() {
                    Some((_, id)) => Hover::Subagent(name, id),
                    None => Hover::Activity(name),
                })
            }
        }
    }

    /// The selected agent's activity, which [0] shows while its terminal is
    /// left to another window.
    fn elsewhere_activity(&self) -> Option<Hover> {
        let name = self.selected_entry()?.name();
        (self.pane.held_elsewhere() == Some(name)).then_some(())?;
        self.activity(name)?;
        Some(Hover::Activity(name.clone()))
    }

    /// The agent whose row shows the `⇄ … · ⏎ move here` banner: selected in
    /// [1] with [0] paused on it.
    pub fn moves_here(&self) -> Option<(&SessionName, &Windows)> {
        let selected = self.selected_entry()?.name();
        let held =
            matches!(self.panel, Panel::Agents) && self.pane.held_elsewhere() == Some(selected);
        held.then(|| self.company(selected).windows().map(|w| (selected, w)))
            .flatten()
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
            EntryState::Unhostable(why) => Enter::Refused(why),
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
                        place: Placement::Pinned,
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
                                place: Placement::Solo,
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
                            place: Placement::Nested,
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
                place: Placement::Alone,
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
            self.home.as_deref(),
            self.name_limit,
        )
    }

    /// Whether `name` is a pinned agent in the snapshot.
    fn pinned(&self, name: &SessionName) -> bool {
        self.snapshot()
            .and_then(|s| s.entry(name))
            .is_some_and(|(_, e)| e.pin().is_some())
    }

    /// Moves the cursor. Repo rows only exist in Overview, so in Focus
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
        self.preview_at = Some(self.clock.mono + PREVIEW_DEBOUNCE);
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
            at: self.clock.mono,
        });
    }

    pub fn error(&mut self, text: impl std::fmt::Display) {
        self.status = Some(Status {
            text: text.to_string(),
            kind: StatusKind::Error,
            at: self.clock.mono,
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

/// Case-insensitive subsequence match. Returns matched byte offsets.
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
    /// A typed path (`/`, `~/`, `./`, `../`) whose directory the worker
    /// hasn't been heard on yet.
    Checking(PathBuf),
    /// A typed path to a directory, absolute.
    Path(PathBuf),
    /// A typed path to nothing.
    Missing(PathBuf),
    /// A search. The chosen match, if any.
    Search(Option<RepoRoot>),
    /// `~user/...`, which isn't supported.
    Unsupported,
}

impl RepoInput {
    /// A typed `path`, as far as `checked` says whether it is a directory.
    fn typed(path: PathBuf, checked: &DirCheck) -> Self {
        match checked {
            DirCheck::Answered { path: of, found } if *of == path => match found {
                Found::Dir => Self::Path(path),
                Found::Nothing => Self::Missing(path),
            },
            DirCheck::Unasked | DirCheck::Asked(_) | DirCheck::Answered { .. } => {
                Self::Checking(path)
            }
        }
    }

    /// Why this input names no repo: still being looked at, or nothing.
    fn blocked(&self) -> Blocked {
        match self {
            Self::Checking(_) => Blocked::Checking,
            Self::Default(_)
            | Self::Path(_)
            | Self::Missing(_)
            | Self::Search(_)
            | Self::Unsupported => Blocked::NoRepo,
        }
    }

    pub fn choice(&self) -> Option<RepoChoice> {
        match self {
            Self::Default(root) | Self::Search(root) => root.clone().map(RepoChoice::Known),
            Self::Path(path) => Some(RepoChoice::Path(path.clone())),
            Self::Checking(_) | Self::Missing(_) | Self::Unsupported => None,
        }
    }
}

/// What ⏎ would do with the typed name, decided once per edit or refresh so
/// what the form shows is what submitting does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameCheck {
    Empty,
    TooLong {
        over: std::num::NonZeroUsize,
        limit: usize,
    },
    Invalid(String),
    /// The session the TUI runs in.
    Own,
    /// A repo's main checkout without a session. Start it from the list.
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
    fn of(
        text: &str,
        limit: NameLimit,
        snapshot: Option<&Snapshot>,
        own: Option<&SessionName>,
    ) -> Self {
        use agent_core::model::EntryKind;
        if text.is_empty() {
            return Self::Empty;
        }
        let name = match text.parse::<AgentName>() {
            Ok(name) => name,
            Err(NameError::Reserved(_)) => return Self::Invalid(format!("'{text}' is reserved")),
            Err(_) => {
                let bad = invalid_chars(text)
                    .into_iter()
                    .next()
                    .map_or(' ', |(_, c)| c);
                return Self::Invalid(format!("'{bad}' not allowed: letters, digits, . - _"));
            }
        };
        let host = match limit.admit(name.session()) {
            Ok(host) => host,
            Err(NameError::TooLong { over, limit, .. }) => {
                return Self::TooLong { over, limit };
            }
            Err(other) => return Self::Invalid(other.to_string()),
        };
        let session = host.session();
        if own == Some(session) {
            return Self::Own;
        }
        let Some((repo, entry)) = snapshot.and_then(|s| s.entry(session)) else {
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
            (_, EntryState::Unhostable(why)) => Self::Invalid(why.to_string()),
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
    /// The typed repo path is still being looked at.
    Checking,
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
            Self::Checking => Some("still looking for that directory".into()),
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
            None => Self::Blocked(repo.blocked()),
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
                None => Self::Blocked(repo.blocked()),
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

/// Which edit of the new-agent prompt a name suggestion answers, so a reply
/// to an older edit is dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromptEdit(u64);

impl PromptEdit {
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Asking the harness's model for a name, debounced while the prompt is
/// being typed. `edit` identifies the prompt edit a reply belongs to.
#[derive(Debug, Clone, Default)]
pub struct Naming {
    pub edit: PromptEdit,
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

/// The new-agent form. `name` is what the user typed. When it's empty the
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
    pub error: Option<Cause>,
    pub naming: Naming,
    pub dir: DirCheck,
    pub phase: Phase,
}

/// What the worker has said about the directory the repo field's typed path
/// names, which only it may look at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DirCheck {
    #[default]
    Unasked,
    Asked(PathBuf),
    Answered {
        path: PathBuf,
        found: Found,
    },
}

/// What the worker found at a typed path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    Dir,
    Nothing,
}

impl DirCheck {
    /// Whether a reply for `path` is the one being waited for.
    pub fn awaits(&self, path: &Path) -> bool {
        matches!(self, Self::Asked(asked) if asked == path)
    }
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
    /// What the first field names to check out, if it parses as one.
    pub target: Option<Checkout>,
    /// The first field mentions a GitHub link, which is live feedback
    /// worth giving even mid-word.
    pub link_pasted: bool,
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
            dir: DirCheck::default(),
            phase: Phase::Editing,
        }
    }

    /// The prompt changed: drop any model answer for the old one and, if
    /// the prompt is prose worth naming, ask again once typing pauses.
    pub fn prompt_changed(&mut self, now: Instant, limit: NameLimit) {
        self.naming.edit = self.naming.edit.next();
        let source = agent_core::naming::suggest(&self.task, limit).map(|s| s.source);
        let words = self.task.split_whitespace().count();
        self.naming.state = match (self.kind, source) {
            (FormKind::Checkout, _) | (_, Some(agent_core::naming::Source::Link)) => {
                NamingState::Idle
            }
            _ if words < NAMING_MIN_WORDS => NamingState::Idle,
            _ => NamingState::Due(now + NAMING_DEBOUNCE),
        };
    }

    /// The model replied for `edit`. Kept only if the prompt hasn't
    /// changed since.
    pub fn model_named(&mut self, edit: PromptEdit, name: Option<AgentName>) {
        if edit != self.naming.edit {
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
        home: Option<&Path>,
        limit: NameLimit,
    ) -> Derived {
        use agent_core::naming::{Source, for_checkout, suggest};
        let target = self.task.trim().parse::<Checkout>().ok();
        let suggestion = match self.kind {
            FormKind::Checkout => target.as_ref().and_then(|target| {
                let source = match target {
                    Checkout::Branch(_) => NameSource::Branch,
                    Checkout::PullRequest(_) => NameSource::Link,
                };
                for_checkout(target, limit).map(|name| NameSuggestion {
                    name: name.to_string(),
                    source,
                })
            }),
            FormKind::New => {
                let local = suggest(&self.task, limit);
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
                    name: free_name(&name, limit, snapshot, own),
                    source,
                })
            }
        };
        let name = match (self.name.as_str(), &suggestion) {
            ("", Some(suggestion)) => suggestion.name.clone(),
            (typed, _) => typed.to_string(),
        };
        let check = NameCheck::of(&name, limit, snapshot, own);
        let mut repo_input = self.parse_repo(snapshot, catalogue, start_dir, home);
        // A pull request link names its repo, which wins over the selected
        // one while nothing is typed and exactly one repo has that name.
        if let (FormKind::Checkout, RepoInput::Default(_)) = (self.kind, &repo_input)
            && let Some(Checkout::PullRequest(pr)) = &target
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
            target,
            link_pasted: self.task.contains("github.com"),
            completion,
            submission,
        }
    }

    fn parse_repo(
        &self,
        snapshot: Option<&Snapshot>,
        catalogue: &[RepoRoot],
        start_dir: &Path,
        home: Option<&Path>,
    ) -> RepoInput {
        let text = self.repo.text.trim();
        if text.is_empty() {
            return RepoInput::Default(self.default_repo.clone());
        }
        match typed_path(text, start_dir, home) {
            Some(Typed::Path(path)) => RepoInput::typed(path, &self.dir),
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
fn free_name(
    base: &AgentName,
    limit: NameLimit,
    snapshot: Option<&Snapshot>,
    own: Option<&SessionName>,
) -> String {
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
            let room = limit.bytes().saturating_sub(suffix.len());
            let stem = &base.as_str()[..base.as_str().len().min(room)];
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
            row: ActivityRow::Subagent {
                by,
                id: AgentId::new(id),
            },
        };
    }

    /// `key`'s transcript as if it had been read.
    pub fn set_log(
        &mut self,
        key: SubagentKey,
        log: Result<Option<agent_core::ports::Log>, Cause>,
    ) {
        self.track(&key.0)
            .subagents
            .entry(key.1)
            .or_default()
            .watch
            .log = Some(log);
    }

    /// `name`'s current activity as if it had been read.
    pub fn set_activity(
        &mut self,
        name: &SessionName,
        detail: Result<Option<ActivityDetail>, Cause>,
    ) {
        self.track(name).activity_view.watch.log = Some(detail);
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
    fn a_limit_shorter_than_the_numbered_suffix_gives_a_name_instead_of_panicking() {
        let taken: SessionName = "ab".parse().expect("parses");
        let limit = NameLimit::new(std::num::NonZeroUsize::new(1).expect("nonzero"));
        let name = free_name(&"ab".parse().expect("parses"), limit, None, Some(&taken));
        assert_eq!(name, "-2");
    }

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
