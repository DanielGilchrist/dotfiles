use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use agent_core::actions::Actions;
use agent_core::activity::{Activity, State as ActivityState};
use agent_core::config::Keys;
use agent_core::model::{
    Entry, EntryState, Removable, Repo, RepoKey, Revival, Running, Snapshot, Startable,
};
use agent_core::names::{AgentName, Prompt, SessionName};
use agent_core::ops::{Approved, Hazardous, RemovalPlan};
use agent_core::paths::{RepoRoot, Typed, typed_path};
use agent_core::ports::{Change, HarnessId, WorktreeFacts};
use agent_term::{Emulator, Terminal};
use ratatui::layout::Rect;

pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(150);
pub const INFO_TTL: Duration = Duration::from_secs(4);

/// How long the prompt must be still before the model is asked.
pub const NAMING_DEBOUNCE: Duration = Duration::from_millis(900);
/// Prompts this short are named from their words; asking isn't worth it.
const NAMING_MIN_WORDS: usize = 3;

/// The sidebar panel `j`/`k` act on in NAV, and how far it has moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// [1]: they move the selection.
    Agents,
    /// [2]: they scroll the selected agent's details down `offset` rows.
    Agent { offset: usize },
    /// [3]: they move the cursor over the selected agent's changed files.
    Changes { file: usize },
}

/// Whether the column is tall enough for [3] at all. Until a draw says
/// otherwise there's room, so keys aren't refused on a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkRoom {
    Fits,
    TooShort,
    /// The sidebar isn't drawn at this width.
    Hidden,
}

/// What a move past the end of a list does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrap {
    /// Continues from the other end.
    Around,
    /// Stops at the end.
    Clamp,
}

/// What [2] has beyond its rows, as last drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// Everything fits.
    Whole,
    /// Unfocused, with this many rows left out.
    Hidden(NonZeroUsize),
    /// Focused and scrolled: the rows shown, of all of them.
    Scrolled {
        top: usize,
        bottom: usize,
        total: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Focus,
    Overview,
}

/// What the sidebar selection points at, by identity so it survives refreshes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cursor {
    /// A repo row; only exists in Overview.
    Repo(RepoKey),
    Entry(SessionName),
    /// The copy of an entry in one of Overview's groups above the repos.
    Grouped(Group, SessionName),
}

/// Overview's groups above the repos. Selecting a copy there doesn't
/// expand its repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Agents asking something, failed, or done and not yet seen.
    NeedsYou,
    Pinned,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Pinned => "Pinned",
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
    /// An agent in one of Overview's groups.
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

pub enum Overlay {
    None,
    New(Box<NewAgent>),
    Switcher(Query),
    Picker(Query),
    Confirm(Confirm),
    Help,
    /// The configured actions, to run on this agent.
    Actions(SessionName),
    /// A reply being typed for an agent.
    Reply {
        to: SessionName,
        text: String,
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
    /// What [2] had beyond its rows when last drawn, for the footer.
    pub turn: Turn,
    /// Whether the column had room for [3] when last drawn, so `3` can
    /// say why it did nothing.
    pub work_room: WorkRoom,
    /// An agent to select once a refresh shows it.
    pub follow: Option<Follow>,
    /// Agents the form is launching, by the launch's ticket. A new
    /// worktree shows up before its session has started, and a second
    /// start from `⏎` would race the first and skip its prompt.
    pub launching: Vec<(Ticket, SessionName)>,
    next_ticket: u64,
    pub actions: Actions,
    pub keys: Keys,
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
    /// Each agent's git facts, or why they couldn't be read, as last read.
    pub facts: HashMap<SessionName, Result<WorktreeFacts, String>>,
    /// When each agent's facts were last asked for.
    pub inspections: HashMap<SessionName, Inspection>,
    /// The finished turn last marked seen, so it's marked once.
    pub acknowledged: Option<(SessionName, u64)>,
    pub last_bell: Option<Instant>,
    /// A bell held back by the rate limit, to ring when it allows.
    pub bell_due: Option<Instant>,
    pub terminal: TerminalFocus,
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
            view: View::Focus,
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
            turn: Turn::Whole,
            work_room: WorkRoom::Fits,
            follow: None,
            launching: Vec::new(),
            next_ticket: 0,
            actions: Actions::default(),
            keys: Keys::default(),
            catalogue: Vec::new(),
            catalogue_warnings: Vec::new(),
            snapshots: 0,
            activities: None,
            now: SystemTime::now(),
            facts: HashMap::new(),
            inspections: HashMap::new(),
            acknowledged: None,
            last_bell: None,
            bell_due: None,
            terminal: TerminalFocus::Focused,
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

    /// The selected agent's changed files, as last read.
    pub fn changes(&self) -> &[Change] {
        match self.selected_entry().and_then(|e| self.facts.get(e.name())) {
            Some(Ok(facts)) => &facts.changed,
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
        self.panel = match self.panel {
            Panel::Agents => {
                self.move_cursor_by(delta, wrap);
                Panel::Agents
            }
            Panel::Agent { offset } => Panel::Agent {
                offset: offset.saturating_add_signed(delta),
            },
            Panel::Changes { file } => Panel::Changes {
                file: file
                    .saturating_add_signed(delta)
                    .min(self.changes().len().saturating_sub(1)),
            },
        };
    }

    /// Half the focused panel's rows, at least one.
    pub fn half_page(&self) -> isize {
        isize::try_from(self.areas.page / 2)
            .unwrap_or(isize::MAX)
            .max(1)
    }

    pub fn panel_to_edge(&mut self, edge: Edge) {
        self.panel = match (self.panel, edge) {
            (Panel::Agents, _) => {
                self.move_to_edge(edge);
                Panel::Agents
            }
            (Panel::Agent { .. }, Edge::First) => Panel::Agent { offset: 0 },
            (Panel::Agent { .. }, Edge::Last) => Panel::Agent { offset: usize::MAX },
            (Panel::Changes { .. }, Edge::First) => Panel::Changes { file: 0 },
            (Panel::Changes { .. }, Edge::Last) => Panel::Changes {
                file: self.changes().len().saturating_sub(1),
            },
        };
    }

    /// The selected agent's checkout and the file under [3]'s cursor in it.
    pub fn file_under_cursor(&self) -> Option<(PathBuf, PathBuf)> {
        let Panel::Changes { file } = self.panel else {
            return None;
        };
        let root = self.selected_entry()?.path()?;
        let changes = self.changes();
        changes
            .get(file.min(changes.len().saturating_sub(1)))
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
                    (ActivityState::Delegating, _) => NonZeroUsize::new(activity.subagents.len())
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

    /// The agent `r` can type a reply into: live and at rest. While it works,
    /// a permission prompt can open before its report arrives, and typed
    /// text would pick a menu option.
    pub fn replyable<'a>(&self, entry: &'a Entry) -> Option<&'a SessionName> {
        let EntryState::Live = entry.state() else {
            return None;
        };
        if self.own_session.as_ref() == Some(entry.name()) {
            return None;
        }
        self.activity(entry.name()).filter(|a| a.at_rest())?;
        Some(entry.name())
    }

    /// How many subagents a live agent has running, if any.
    pub fn subagents(&self, entry: &Entry) -> Option<NonZeroUsize> {
        let EntryState::Live = entry.state() else {
            return None;
        };
        NonZeroUsize::new(self.activity(entry.name())?.subagents.len())
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

    pub fn rows(&self) -> Vec<Row> {
        let (Some(snapshot), Some(cursor)) = (self.snapshot(), self.cursor()) else {
            return Vec::new();
        };
        let current = cursor.repo(snapshot).map(Repo::key);
        match self.view {
            View::Focus => current
                .and_then(|k| snapshot.repo(k))
                .map(|repo| self.focus_rows(repo))
                .unwrap_or_default(),
            View::Overview => {
                let mut rows = Vec::new();
                let waiting = self.needs_you();
                if !waiting.is_empty() {
                    rows.push(Row::Header(Group::NeedsYou));
                    rows.extend(
                        waiting
                            .iter()
                            .map(|name| Row::Grouped(Group::NeedsYou, name.clone())),
                    );
                }
                let pinned: Vec<_> = snapshot
                    .pinned()
                    .into_iter()
                    .filter(|(_, e)| !waiting.contains(e.name()))
                    .collect();
                if !pinned.is_empty() {
                    rows.push(Row::Header(Group::Pinned));
                    rows.extend(
                        pinned
                            .iter()
                            .map(|(_, e)| Row::Grouped(Group::Pinned, e.name().clone())),
                    );
                }
                let repos = snapshot.repos();
                let listed = |e: &&Entry| !waiting.contains(e.name());
                // Folds are worked out from the extent's worst case, so
                // agents changing state never open or close a repo.
                let sizes: Vec<usize> = repos
                    .iter()
                    .map(|r| match sole(r) {
                        Some(_) => 0,
                        None => r.entries().len(),
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
                    if !repo.entries().iter().any(|e| listed(&e)) {
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

    /// Focus: the repo's entries, then agents from other repos that need
    /// you.
    fn focus_rows(&self, repo: &Repo) -> Vec<Row> {
        let mut rows: Vec<Row> = repo
            .entries()
            .iter()
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

    /// The sidebar list at its tallest: both views reserve the Needs you
    /// header even while it's hidden, Focus counts every other repo's entry
    /// as a one-line row in it, and Overview counts pinned agents twice, in
    /// their group and their repo.
    pub fn extent(&self) -> Extent {
        let Some(snapshot) = self.snapshot() else {
            return Extent::default();
        };
        let (headers, cards): (usize, Vec<&Entry>) = match self.view {
            View::Focus => {
                let Some((repo, _)) = self.selected() else {
                    return Extent::default();
                };
                let elsewhere = snapshot.entries().count() - repo.entries().len();
                return Extent {
                    rows: 1 + elsewhere + repo.entries().len(),
                    live: repo
                        .entries()
                        .iter()
                        .filter(|e| matches!(e.state(), EntryState::Live))
                        .count(),
                };
            }
            View::Overview => {
                let pinned = snapshot.pinned();
                let groups = 1 + usize::from(!pinned.is_empty());
                let repos = snapshot.repos().iter().filter(|r| sole(r).is_none());
                (
                    groups + repos.count(),
                    pinned
                        .into_iter()
                        .chain(snapshot.entries())
                        .map(|(_, e)| e)
                        .collect(),
                )
            }
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
        let listed_in_needs_you =
            |name: &SessionName| view == View::Overview && waiting.contains(name);
        let Load::Ready { snapshot, cursor } = &mut self.load else {
            return;
        };
        let valid = match cursor.as_ref() {
            None => false,
            Some(Cursor::Repo(key)) => {
                view == View::Overview
                    && snapshot
                        .repo(key)
                        .is_some_and(|r| r.entries().iter().any(|e| !waiting.contains(e.name())))
            }
            Some(Cursor::Grouped(Group::NeedsYou, name)) => {
                view == View::Overview && waiting.contains(name)
            }
            Some(Cursor::Grouped(Group::Pinned, name)) => {
                view == View::Overview
                    && !waiting.contains(name)
                    && snapshot.entry(name).is_some_and(|(_, e)| e.pin().is_some())
            }
            Some(Cursor::Entry(name)) => {
                snapshot.entry(name).is_some() && !listed_in_needs_you(name)
            }
        };
        if valid {
            return;
        }
        let entry_of = |e: &Entry| match e.name() {
            name if listed_in_needs_you(name) => Cursor::Grouped(Group::NeedsYou, name.clone()),
            name => Cursor::Entry(name.clone()),
        };
        let fallback = match cursor.as_ref() {
            Some(Cursor::Grouped(_, name) | Cursor::Entry(name)) => {
                snapshot.entry(name).map(|(_, e)| entry_of(e))
            }
            Some(Cursor::Repo(key)) => snapshot.repo(key).map(|r| entry_of(r.first())),
            None => None,
        };
        let fallback = fallback
            .or_else(|| {
                previous_repo
                    .and_then(|k| snapshot.repo(k))
                    .map(|r| entry_of(r.first()))
            })
            .or_else(|| {
                snapshot
                    .containing(&start_dir)
                    .or_else(|| snapshot.entries().next())
                    .map(|(_, e)| entry_of(e))
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

    /// Moves the cursor; repo rows and the pinned group only exist in
    /// Overview, so in Focus those become the entry they stand for. In
    /// Overview an agent that needs you is listed only under Needs you, and
    /// a repo whose agents all need you has no row of its own.
    pub fn set_cursor(&mut self, next: Cursor) {
        let next = match (self.view, next) {
            (View::Focus, Cursor::Grouped(_, name)) => Cursor::Entry(name),
            (View::Focus, Cursor::Repo(key)) => match self.snapshot().and_then(|s| s.repo(&key)) {
                Some(repo) => Cursor::Entry(repo.first().name().clone()),
                None => return,
            },
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
        let repos = snapshot.repos();
        if repos.is_empty() {
            return;
        }
        let current = self
            .cursor()
            .and_then(|c| c.repo(snapshot))
            .and_then(|r| repos.iter().position(|x| x.key() == r.key()))
            .unwrap_or(0);
        let next = current
            .checked_add_signed(delta)
            .map_or(repos.len() - 1, |n| n % repos.len());
        let key = repos[next].key().clone();
        self.select_repo(&key);
    }

    pub fn select_repo(&mut self, key: &RepoKey) {
        let Some(repo) = self.snapshot().and_then(|s| s.repo(key)) else {
            return;
        };
        let cursor = match (self.view, sole(repo)) {
            (View::Overview, None) => Cursor::Repo(key.clone()),
            (View::Overview, Some(_)) | (View::Focus, _) => {
                Cursor::Entry(repo.first().name().clone())
            }
        };
        self.set_cursor(cursor);
    }

    pub fn toggle_view(&mut self) {
        self.view = match self.view {
            View::Focus => View::Overview,
            View::Overview => View::Focus,
        };
        let repo = self.selected().map(|(r, _)| r.key().clone());
        self.reconcile(repo.as_ref());
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
}

impl Launchable {
    pub fn session(&self) -> SessionName {
        match self {
            Self::Create { name, .. } | Self::Attach { name, .. } | Self::Resurrect(name) => {
                name.session()
            }
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
        }
    }

    /// What's happening while it runs.
    pub fn progress(&self) -> &'static str {
        match self {
            Self::Create { .. } => "creating",
            Self::Attach { .. } => "attaching to",
            Self::Resurrect(_) => "resurrecting",
            Self::Start { .. } => "starting",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// The name check says why.
    Name,
    Prompt(String),
    NoRepo,
    /// Exited, with a prompt that couldn't be delivered.
    ExitedWithPrompt,
}

impl Blocked {
    /// Why ⏎ did nothing, unless the form already shows it.
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Name => None,
            Self::Prompt(why) => Some(format!("prompt: {why}")),
            Self::NoRepo => Some("pick a repo: type its name, or a path".into()),
            Self::ExitedWithPrompt => Some(
                "it exited, so the prompt can't be typed in. Clear it to resurrect, then send it"
                    .into(),
            ),
        }
    }
}

impl Submission {
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

/// The new-agent form. `name` is what the user typed; when it's empty the
/// suggestion is used. `suggestion`, `check` and `repo_input` are derived
/// from the fields and the snapshot, and refreshed whenever either changes.
#[derive(Debug, Clone)]
pub struct NewAgent {
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
    pub fn new(default_repo: Option<RepoRoot>) -> Self {
        Self {
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
        self.naming.state = match source {
            Some(agent_core::naming::Source::Link) => NamingState::Idle,
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
        use agent_core::naming::{Source, suggest};
        let local = suggest(&self.task);
        let answer = match &self.naming.state {
            NamingState::Answered { prompt, name } if *prompt == self.task => Some(name),
            _ => None,
        };
        let suggestion = match (local, answer) {
            (Some(link), _) if link.source == Source::Link => Some((link.name, NameSource::Link)),
            (_, Some(name)) => Some((name.clone(), NameSource::Model)),
            (Some(words), None) => Some((words.name, NameSource::Words)),
            (None, None) => None,
        }
        .map(|(name, source)| NameSuggestion {
            name: free_name(&name, snapshot, own),
            source,
        });
        let name = match (self.name.as_str(), &suggestion) {
            ("", Some(suggestion)) => suggestion.name.clone(),
            (typed, _) => typed.to_string(),
        };
        let check = NameCheck::of(&name, snapshot, own);
        let repo_input = self.parse_repo(snapshot, catalogue, start_dir);
        let completion = match &repo_input {
            RepoInput::Search(Some(root)) => completes(&root.name(), self.repo.text.trim()),
            _ => None,
        };
        let submission = Submission::of(&check, &self.task, &repo_input);
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

/// Known repos (not orphan sessions) whose names fuzzily match `query`,
/// prefix matches first.
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
        assert!(Cursor::Grouped(Group::Pinned, fix.clone()).follows(&needs_you));
        assert!(!Cursor::Grouped(Group::Pinned, fix.clone()).follows(&Cursor::Entry(fix)));
    }
}
