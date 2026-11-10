//! The one table of NAV and peek bindings. The footer and the help both
//! read it, so what they say can't drift apart. The key handlers in
//! `update` still do the dispatching.
//! `nav_keys_do_something_exactly_where_the_keymap_table_says`, in the
//! `view/sidebar.rs` tests, presses every key in every context and fails
//! when a handler and this table disagree.

use crate::document::Hover;
use crate::model::{LEAVE_PAUSED, MOVE_HERE, Model, Panel, Reach, View, Visibility};
use crate::tree::{ChangesScope, ChangesView, Node};
use agent_core::model::Entry;
use agent_core::ports::Compared;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityDoc {
    /// The hooks' account, with no log to move over.
    Hooks,
    /// A log with entries to move over.
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    /// [1], the fleet.
    Agents,
    /// [2], the activity.
    Activity,
    /// [3], Changes.
    Changes,
    /// [0] with the keys.
    Main,
    /// The peek float.
    Peek,
}

impl Ctx {
    pub fn name(self) -> &'static str {
        match self {
            Self::Agents => "[1] Agents",
            Self::Activity => "[2] Activity",
            Self::Changes => "[3] Changes",
            Self::Main => "[0] Main",
            Self::Peek => "Peek",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Ctxs(u8);

impl Ctxs {
    const fn bit(ctx: Ctx) -> u8 {
        1 << ctx as u8
    }

    const fn of(list: &[Ctx]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < list.len() {
            bits |= Self::bit(list[i]);
            i += 1;
        }
        Self(bits)
    }

    const fn has(self, ctx: Ctx) -> bool {
        self.0 & Self::bit(ctx) != 0
    }

    const fn covers(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

const PANELS: Ctxs = Ctxs::of(&[Ctx::Agents, Ctx::Activity, Ctx::Changes, Ctx::Main]);
const EVERYWHERE: Ctxs = Ctxs::of(&[
    Ctx::Agents,
    Ctx::Activity,
    Ctx::Changes,
    Ctx::Main,
    Ctx::Peek,
]);
const AGENTS: Ctxs = Ctxs::of(&[Ctx::Agents]);
const LISTS: Ctxs = Ctxs::of(&[Ctx::Activity, Ctx::Changes]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quoting {
    No,
    Yes,
}

/// What the bindings' labels and ranks depend on, read once from the model.
#[derive(Debug, Clone)]
pub struct Situation {
    pub ctx: Ctx,
    pub selected: Selected,
    pub reading: Reading,
    pub changes_row: ChangesRow,
    pub changes_view: ChangesView,
    pub feed: Feed,
    pub left: Left,
    pub toggle: Toggle,
    /// Where `⏎` takes [0] from the activity's entry under the cursor.
    pub jump: Option<crate::log::Jump>,
    pub reply: Quoting,
    pub next_tab: &'static str,
    /// What `]` names while [3] has the keys.
    pub next_changes_tab: &'static str,
}

#[derive(Debug, Clone)]
pub enum Selected {
    Nothing,
    Repo,
    Agent(AgentKeys),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Avail {
    Offered,
    Absent,
}

impl Avail {
    fn of<T>(offered: Option<&T>) -> Self {
        Self::from(offered.is_some())
    }

    fn then_some<T>(self, value: T) -> Option<T> {
        match self {
            Self::Offered => Some(value),
            Self::Absent => None,
        }
    }
}

impl From<bool> for Avail {
    fn from(offered: bool) -> Self {
        if offered { Self::Offered } else { Self::Absent }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Here,
    /// Another window shows it.
    Elsewhere,
    /// `⏎` on it moves here: its row shows the banner.
    MovesHere,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    Pinned,
    Unpinned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Draft {
    /// Comments are drafted on the agent's diffs.
    Comments,
    Empty,
}

#[derive(Debug, Clone)]
pub struct AgentKeys {
    pub enter: Option<&'static str>,
    pub kill: Avail,
    pub remove: Avail,
    pub send: Avail,
    pub peek: Avail,
    pub actions: Avail,
    pub window: Window,
    pub pin: Pin,
    pub draft: Draft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Nothing,
    /// A file's, a folder's or a commit's diff.
    Diff(Lines),
    /// The branch, with the cursor on a commit or not.
    Branch(BranchRow),
    /// The activity document, and what kind it is.
    Activity(ActivityDoc),
    Subagent,
    Repo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    Cursor,
    /// A range of diff lines is selected.
    Range,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchRow {
    Commit,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesRow {
    Other,
    File,
    Folder(Fold),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Open,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feed {
    Following,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Left {
    Nobody,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    /// The pinned tab lists every pin whatever the filter.
    Absent,
    Offered(&'static str),
    /// Worth a footer slot of its own: something is hidden, or everything is shown.
    Worth(&'static str),
}

/// What `⏎` says, and whether kill, remove and peek apply, from what an
/// entry's `reach` allows.
struct ReachKeys {
    enter: Option<&'static str>,
    kill: Avail,
    remove: Avail,
    peek: Avail,
}

fn reach_keys(reach: &Reach, moves_here: bool) -> ReachKeys {
    let peek = Avail::from(reach.is_attach());
    match reach {
        Reach::Other {
            enter,
            kill,
            remove,
        } => ReachKeys {
            enter: enter
                .verb()
                .map(|verb| if moves_here { MOVE_HERE } else { verb }),
            kill: Avail::of(kill.as_ref()),
            remove: Avail::of(remove.as_ref()),
            peek,
        },
        Reach::Own => ReachKeys {
            enter: None,
            kill: Avail::Absent,
            remove: Avail::Absent,
            peek,
        },
    }
}

impl Situation {
    fn agent(&self) -> Option<&AgentKeys> {
        match &self.selected {
            Selected::Agent(keys) => Some(keys),
            Selected::Nothing | Selected::Repo => None,
        }
    }

    fn on_agent(&self) -> bool {
        self.agent().is_some()
    }

    fn can(&self, key: impl Fn(&AgentKeys) -> Avail) -> Avail {
        self.agent().map_or(Avail::Absent, key)
    }

    fn window(&self) -> Option<Window> {
        self.agent().map(|a| a.window)
    }

    fn hover(&self) -> bool {
        self.reading != Reading::Nothing
    }

    fn diff(&self) -> bool {
        matches!(self.reading, Reading::Diff(_))
    }

    fn branch(&self) -> bool {
        matches!(self.reading, Reading::Branch(_))
    }

    fn activity(&self) -> Option<ActivityDoc> {
        match self.reading {
            Reading::Activity(doc) => Some(doc),
            _ => None,
        }
    }

    fn subagent_row(&self) -> bool {
        self.reading == Reading::Subagent
    }

    fn file_row(&self) -> bool {
        self.changes_row == ChangesRow::File
    }

    fn folder(&self) -> Option<Fold> {
        match self.changes_row {
            ChangesRow::Folder(fold) => Some(fold),
            ChangesRow::File | ChangesRow::Other => None,
        }
    }

    fn paused(&self) -> bool {
        self.feed == Feed::Paused
    }

    /// [0] has the keys on an activity that has a log.
    fn reads_activity(&self) -> bool {
        self.ctx == Ctx::Main && self.activity() == Some(ActivityDoc::Log)
    }

    fn reads_entries(&self) -> bool {
        self.subagent_row() || self.reads_log()
    }

    fn reads_log(&self) -> bool {
        self.activity() == Some(ActivityDoc::Log)
    }

    pub fn of(m: &Model) -> Self {
        let ctx = match m.panel {
            _ if m.peeking() => Ctx::Peek,
            Panel::Agents => Ctx::Agents,
            Panel::Agent { .. } => Ctx::Activity,
            Panel::Changes { .. } => Ctx::Changes,
            Panel::Main(..) => Ctx::Main,
        };
        let selected = m.selected();
        let entry = selected.and_then(|(_, e)| e);
        let reading = Self::reading(m);
        let selected_keys = match (selected, entry) {
            (None, _) => Selected::Nothing,
            (Some(_), None) => Selected::Repo,
            (Some(_), Some(entry)) => Selected::Agent(Self::agent_keys(m, entry)),
        };
        let left = m
            .pane
            .connection()
            .is_some_and(|c| entry.is_none_or(|e| e.name() != &c.name));
        Self {
            ctx,
            selected: selected_keys,
            reading,
            changes_row: Self::changes_row(m),
            changes_view: m.changes_view,
            feed: if m.docs.paused() {
                Feed::Paused
            } else {
                Feed::Following
            },
            left: if left { Left::Agent } else { Left::Nobody },
            toggle: Self::toggle(m),
            jump: m.activity_jump().cloned(),
            reply: match &m.reply {
                Some(_) => Quoting::Yes,
                None => Quoting::No,
            },
            next_changes_tab: match m.changes_scope.toggled() {
                ChangesScope::Uncommitted => "tab: uncommitted",
                ChangesScope::Since => {
                    let trunk = entry
                        .and_then(|e| m.facts(e.name()))
                        .and_then(Result::ok)
                        .is_some_and(|f| matches!(f.compared, Compared::Trunk { .. }));
                    if trunk {
                        "tab: unpushed"
                    } else {
                        "tab: branch"
                    }
                }
            },
            next_tab: match m.view.next() {
                View::Focus => "tab: repo",
                View::Overview => "tab: agents",
                View::Pinned => "tab: pinned",
            },
        }
    }

    fn reading(m: &Model) -> Reading {
        match &m.hover() {
            None => Reading::Nothing,
            Some(Hover::Diff(_) | Hover::Folder(_) | Hover::Commit(_)) => {
                Reading::Diff(if m.diff_view.anchor.is_some() {
                    Lines::Range
                } else {
                    Lines::Cursor
                })
            }
            Some(Hover::Branch(_)) => {
                let on_commit = m
                    .docs
                    .lines()
                    .get(m.diff_view.cursor)
                    .is_some_and(|l| l.kind == crate::review::LineKind::Commit);
                Reading::Branch(if on_commit {
                    BranchRow::Commit
                } else {
                    BranchRow::Other
                })
            }
            Some(Hover::Activity(name)) if m.activity_has_log(name) => {
                Reading::Activity(ActivityDoc::Log)
            }
            Some(Hover::Activity(_)) => Reading::Activity(ActivityDoc::Hooks),
            Some(Hover::Subagent(..)) => Reading::Subagent,
            Some(Hover::Repo(_)) => Reading::Repo,
        }
    }

    fn changes_row(m: &Model) -> ChangesRow {
        if m.file_under_cursor().is_some() {
            return ChangesRow::File;
        }
        match m.folder_under_cursor().map(|r| r.node) {
            Some(Node::Folder { open: true }) => ChangesRow::Folder(Fold::Open),
            Some(Node::Folder { open: false }) => ChangesRow::Folder(Fold::Closed),
            Some(Node::File(_)) | None => ChangesRow::Other,
        }
    }

    fn toggle(m: &Model) -> Toggle {
        if m.view == View::Pinned {
            return Toggle::Absent;
        }
        let label = match m.visibility {
            Visibility::All => "active only",
            Visibility::Active => "show all",
        };
        if m.visibility == Visibility::All || m.hidden() > 0 {
            Toggle::Worth(label)
        } else {
            Toggle::Offered(label)
        }
    }

    fn agent_keys(m: &Model, entry: &Entry) -> AgentKeys {
        let moves_here = m.moves_here().is_some_and(|(name, _)| name == entry.name());
        let reach = m.reach(entry);
        let ReachKeys {
            enter,
            kill,
            remove,
            peek,
        } = reach_keys(&reach, moves_here);
        AgentKeys {
            enter,
            kill,
            remove,
            send: Avail::from(m.reachable(entry).is_ok()),
            peek,
            actions: Avail::from(!m.actions.is_empty()),
            window: if moves_here {
                Window::MovesHere
            } else if m.elsewhere(entry.name()) {
                Window::Elsewhere
            } else {
                Window::Here
            },
            pin: if entry.pin().is_some() {
                Pin::Pinned
            } else {
                Pin::Unpinned
            },
            draft: if m.draft(entry.name()).is_empty() {
                Draft::Empty
            } else {
                Draft::Comments
            },
        }
    }
}

enum Label {
    Fixed(&'static str),
    /// `None` when the binding does nothing here.
    By(fn(&Situation) -> Option<&'static str>),
}

enum Rank {
    /// Only in the help.
    Hidden,
    Fixed(u8),
    By(fn(&Situation) -> Option<u8>),
}

pub struct Binding {
    keys: &'static [&'static str],
    also: &'static [&'static str],
    contexts: Ctxs,
    rank: Rank,
    label: Label,
    long: &'static str,
}

const NAV_KEY: &str = "{nav}";

impl Binding {
    const fn new(
        keys: &'static [&'static str],
        contexts: Ctxs,
        rank: Rank,
        label: Label,
        long: &'static str,
    ) -> Self {
        Self {
            keys,
            also: &[],
            contexts,
            rank,
            label,
            long,
        }
    }

    const fn with(mut self, also: &'static [&'static str]) -> Self {
        self.also = also;
        self
    }

    /// Whether it's listed beside the focused panel's own keys rather than
    /// with the ones every panel has.
    fn local(&self) -> bool {
        !self.contexts.covers(PANELS)
    }

    fn label_in(&self, s: &Situation) -> Option<&'static str> {
        if !self.contexts.has(s.ctx) {
            return None;
        }
        match &self.label {
            Label::Fixed(label) => Some(label),
            Label::By(f) => f(s),
        }
    }

    fn rank_in(&self, s: &Situation) -> Option<u8> {
        match &self.rank {
            Rank::Hidden => None,
            Rank::Fixed(n) => Some(*n),
            Rank::By(f) => f(s),
        }
    }

    /// Every key it answers to, for the help and for the tests.
    pub fn all_keys(&self) -> impl Iterator<Item = &'static str> {
        self.keys.iter().chain(self.also).copied()
    }
}

fn expand(text: &str, nav: &str) -> String {
    text.replace(NAV_KEY, nav)
}

use Label::{By, Fixed as Say};
use Rank::{By as RankBy, Fixed as At, Hidden};

pub const BINDINGS: &[Binding] = &[
    Binding::new(
        &["⏎"],
        AGENTS,
        At(9),
        By(|s| match &s.selected {
            Selected::Repo => Some("focus repo"),
            Selected::Agent(keys) => keys.enter,
            Selected::Nothing => None,
        }),
        "On an agent: attach, so your keys go to its terminal (starts or resurrects the session first if needed). On an agent open in another window: move here, which attaches and then closes that window, from [1] on the banner row only. On a repo: show that repo's agents.",
    )
    .with(&["0"]),
    Binding::new(
        &["⏎"],
        LISTS,
        RankBy(|s| s.hover().then_some(9)),
        By(|s| match s.folder() {
            Some(Fold::Open) if s.ctx == Ctx::Changes => Some("collapse"),
            Some(Fold::Closed) if s.ctx == Ctx::Changes => Some("expand"),
            _ => s.hover().then_some("open"),
        }),
        "Open the item under the cursor in [0] and move the keys there: the activity, a subagent's transcript, a diff or the branch. On a folder in [3] it collapses or expands the folder, and 0 opens its file list.",
    )
    .with(&["0"]),
    Binding::new(
        &["⏎"],
        Ctxs::of(&[Ctx::Peek]),
        At(9),
        By(|s| match s.window() {
            None => None,
            Some(Window::Here) => Some("attach"),
            Some(Window::Elsewhere | Window::MovesHere) => Some(LEAVE_PAUSED),
        }),
        "Close the peek and attach, so your keys go to the agent's terminal. On an agent open in another window the peek closes and the agent stays paused, with ⏎ on its row in [1] to move here.",
    ),
    Binding::new(
        &["J", "K"],
        Ctxs::of(&[Ctx::Main]),
        RankBy(|s| s.diff().then_some(6)),
        By(|s| s.diff().then_some("file")),
        "Jump to the previous or next file: in a file's diff it moves [3]'s cursor to the neighbouring file, in a folder or commit it jumps to the file's section.",
    ),
    Binding::new(
        &["J", "K"],
        LISTS,
        RankBy(|s| s.hover().then_some(8)),
        By(|s| s.hover().then_some("scroll")),
        "Scroll [0] two rows at a time without leaving the list.",
    ),
    Binding::new(
        &["e"],
        Ctxs::of(&[Ctx::Changes]),
        RankBy(|s| (s.file_row() || s.folder().is_some()).then_some(7)),
        By(|s| match (s.file_row(), s.folder()) {
            (true, _) => Some("edit file"),
            (_, Some(_)) => Some("edit folder"),
            _ => None,
        }),
        "Open the file under the cursor in $EDITOR, or on a folder the editor with that folder as its directory.",
    ),
    Binding::new(
        &["`"],
        Ctxs::of(&[Ctx::Changes]),
        RankBy(|_| Some(3)),
        By(|s| Some(if s.changes_view == ChangesView::Tree { "flat" } else { "tree" })),
        "Switch [3] between the directory tree and the flat list of files. The choice lasts for the session.",
    ),
    Binding::new(
        &["-", "="],
        Ctxs::of(&[Ctx::Changes]),
        Hidden,
        By(|s| (s.changes_view == ChangesView::Tree).then_some("fold all / unfold all")),
        "Collapse every folder in [3], or expand them all.",
    ),
    Binding::new(
        &["y"],
        Ctxs::of(&[Ctx::Main]),
        RankBy(|s| {
            if s.branch() {
                Some(5)
            } else {
                (s.diff() || s.activity().is_some() || s.subagent_row()).then_some(3)
            }
        }),
        By(|s| {
            if s.branch() {
                Some(if s.reading == Reading::Branch(BranchRow::Commit) { "copy sha" } else { "copy path" })
            } else if s.diff() {
                Some(if s.reading == Reading::Diff(Lines::Range) { "copy lines" } else { "copy path:line" })
            } else if s.reads_log() {
                Some("copy entry")
            } else if s.activity().is_some() {
                Some("copy message")
            } else {
                s.subagent_row().then_some("copy entry")
            }
        }),
        "Copy to the clipboard through the terminal (OSC 52): path:line or the selected lines of a diff, a commit's sha or an uncommitted file's path, the agent's message, or the log entry under the cursor.",
    ),
    Binding::new(
        &["V"],
        Ctxs::of(&[Ctx::Main]),
        Hidden,
        By(|s| s.diff().then_some("select lines")),
        "Start or end a selection of lines at the cursor. c then comments on all of them, and Esc drops the selection.",
    ),
    Binding::new(
        &["⏎"],
        Ctxs::of(&[Ctx::Main]),
        RankBy(|s| s.branch().then_some(9)),
        By(|s| s.branch().then_some("open")),
        "Open the commit under the cursor: the files it changed with each diff against its parent. On an uncommitted file it opens that file's diff. Esc goes back to the branch.",
    ),
    Binding::new(
        &["e"],
        Ctxs::of(&[Ctx::Main]),
        RankBy(|s| s.diff().then_some(7)),
        By(|s| s.diff().then_some("edit line")),
        "Open the file in $EDITOR at the line under the cursor. A removed line opens at the line that now follows it. Not available in a commit's diff.",
    ),
    Binding::new(
        &["e"],
        Ctxs::of(&[Ctx::Activity, Ctx::Main]),
        RankBy(|s| (s.subagent_row() || s.reads_activity()).then_some(7)),
        By(|s| (s.subagent_row() || s.reads_activity()).then_some("edit transcript")),
        "Open the subagent's transcript, or from [0] the activity, as Markdown in $EDITOR, at the entry under the cursor.",
    ),
    Binding::new(
        &["⏎"],
        Ctxs::of(&[Ctx::Main]),
        RankBy(|s| s.reads_entries().then_some(8)),
        By(|s| match s.jump {
            Some(crate::log::Jump::Subagent(_)) => Some("open subagent"),
            Some(crate::log::Jump::File(_)) => Some("open diff"),
            None => s.reads_entries().then_some("fold"),
        }),
        "Expand or collapse the log entry under the cursor: the whole prompt or text, a call's input and result, or a run of calls. On an Agent call that started a subagent it opens the subagent's log, and on an edit or a touched file it opens the file's diff in [3]. Esc goes back to the activity.",
    ),
    Binding::new(
        &["=", "-"],
        Ctxs::of(&[Ctx::Main]),
        Hidden,
        By(|s| s.reads_entries().then_some("expand all / collapse all")),
        "Expand every entry of a log, or collapse them all. Folded phases of an activity open too.",
    ),
    Binding::new(
        &["e"],
        Ctxs::of(&[Ctx::Agents, Ctx::Main]),
        RankBy(|s| (s.ctx == Ctx::Agents).then_some(0)),
        By(|s| (!matches!(s.selected, Selected::Nothing) && !(s.ctx == Ctx::Main && (s.subagent_row() || s.diff() || s.reads_activity()))).then_some("edit checkout")),
        "Open the agent's checkout (or the repo's) in $EDITOR.",
    ),
    Binding::new(
        &["[", "]"],
        AGENTS,
        RankBy(|_| Some(6)),
        By(|s| Some(s.next_tab)),
        "Cycle [1]'s tabs: the repo, all agents, pinned. The footer names the next one.",
    ),
    Binding::new(
        &["[", "]"],
        Ctxs::of(&[Ctx::Changes]),
        RankBy(|_| Some(4)),
        By(|s| Some(s.next_changes_tab)),
        "Cycle [3]'s tabs: Uncommitted, the files not yet committed, counted against the last commit, and the second tab: Branch, what this branch changes compared with where it left origin's default branch, like a PR plus your uncommitted work, or Unpushed on the trunk, the commits not yet on origin plus uncommitted work. The choice lasts for the session.",
    ),
    Binding::new(
        &["[", "]"],
        Ctxs::of(&[Ctx::Main]),
        RankBy(|s| s.diff().then_some(7)),
        By(|s| s.diff().then_some("hunk")),
        "Jump to the previous or next hunk of the diff.",
    ),
    Binding::new(
        &["[", "]"],
        Ctxs::of(&[Ctx::Main]),
        RankBy(|s| s.reads_entries().then_some(7)),
        By(|s| s.reads_entries().then_some(if s.subagent_row() { "message" } else { "phase" })),
        "Move the entry cursor to the first entry of the previous or next phase of the activity. A subagent's log has no folded phases, so there it goes to the previous or next message, or the first call after a queued message or a compaction. It stays put at the first or last, and going back from inside a phase goes to its start.",
    ),
    Binding::new(
        &["."],
        PANELS,
        RankBy(|s| (s.ctx == Ctx::Agents && matches!(s.toggle, Toggle::Worth(_))).then_some(1)),
        By(|s| match s.toggle {
            Toggle::Absent => None,
            Toggle::Offered(label) | Toggle::Worth(label) => Some(label),
        }),
        "Show every checkout, or only those with a session. The pinned tab always lists every pin.",
    ),
    Binding::new(
        &["Esc"],
        Ctxs::of(&[Ctx::Activity, Ctx::Changes, Ctx::Main]),
        At(9),
        Say("back"),
        "Go one level out: a focused document in [0] returns to its panel, or to the activity it was opened from. [2] and [3] return to [1].",
    ),
    Binding::new(
        &["Esc"],
        AGENTS,
        RankBy(|s| (s.left == Left::Agent).then_some(5)),
        By(|s| (s.left == Left::Agent).then_some("back")),
        "Attach the agent you left with the nav key.",
    ),
    Binding::new(
        &["i"],
        EVERYWHERE,
        RankBy(|s| {
            (s.on_agent() && matches!(s.ctx, Ctx::Activity | Ctx::Changes | Ctx::Main))
                .then_some(if s.diff() { 4 } else { 8 })
        }),
        By(|s| match s.window() {
            Some(Window::Here) => Some("attach"),
            Some(Window::MovesHere) => Some(MOVE_HERE),
            Some(Window::Elsewhere) | None => None,
        }),
        "Attach the selected agent from any panel, so the nav key returns to where you were. On an agent open in another window it moves here from [1] only, where its row says so.",
    ),
    Binding::new(
        &["s"],
        EVERYWHERE,
        RankBy(|s| s.can(|a| a.send).then_some(7)),
        By(|s| {
            s.on_agent().then_some(if s.agent().is_some_and(|a| a.draft == Draft::Comments) {
                "send review"
            } else if s.reply == Quoting::Yes {
                "reply"
            } else {
                "send"
            })
        }),
        "Compose a message to the selected agent without attaching. It is typed at once, and refused only while the agent is asking, its session is starting, or it ended. The box wraps and grows with the text. alt-Enter, ctrl-J and shift-Enter add a line, Enter sends, and the arrows move the caret. What you typed stays for the next s: Esc keeps it and ctrl-U clears it. With the entry cursor on an entry in [0]'s activity or a subagent's log it reads reply: the entry's first line shows above the input and goes before your message, only when you typed one. Backspace on empty text drops the quote. In a peek it always sends without a quote. Review comments drafted for the agent go after the message.",
    ),
    Binding::new(
        &["v"],
        EVERYWHERE,
        RankBy(|s| (s.ctx != Ctx::Peek && s.can(|a| a.peek) == Avail::Offered).then_some(5)),
        By(|s| match s.ctx {
            Ctx::Peek => Some("close"),
            _ => s.on_agent().then_some("peek"),
        }),
        "Float the agent's live terminal over everything, read-only. Nothing you type reaches it.",
    ),
    Binding::new(
        &["Esc"],
        Ctxs::of(&[Ctx::Peek]),
        At(9),
        Say("close"),
        "Close the peek. v does too.",
    ),
    Binding::new(
        &["h", "l"],
        PANELS,
        Hidden,
        Say("prev / next panel"),
        "Focus the previous or next left panel, [1] to [3], wrapping and skipping one with nothing to focus. From [0] it continues from the panel [0] came from.",
    )
    .with(&["⇧Tab", "Tab"]),
    Binding::new(
        &["1", "2", "3"],
        PANELS,
        Hidden,
        Say("focus panel"),
        "Focus [1] the fleet, [2] the activity or [3] Changes.",
    ),
    Binding::new(
        &["j", "k"],
        PANELS,
        RankBy(|s| (s.ctx == Ctx::Main).then_some(8)),
        By(|s| {
            Some(match (s.ctx, s.diff() || s.branch()) {
                (Ctx::Main, true) if s.branch() => "move",
                (Ctx::Main, true) => "line",
                (Ctx::Main, false) => "scroll",
                _ => "move",
            })
        }),
        "Move the selection or cursor in the focused panel, or scroll [0] when it has the keys.",
    )
    .with(&["↓", "↑"]),
    Binding::new(
        &["g"],
        PANELS,
        Hidden,
        Say("first"),
        "Jump to the first row, or the top of [0].",
    )
    .with(&["Home"]),
    Binding::new(
        &["G"],
        PANELS,
        RankBy(|s| (s.ctx == Ctx::Main).then_some(if s.paused() { 9 } else { 6 })),
        By(|s| {
            Some(match (s.ctx, s.paused()) {
                (Ctx::Main, true) => "follow",
                (Ctx::Main, false) => "end",
                _ => "last",
            })
        }),
        "Jump to the last row, or the end of [0]. While a transcript is paused it resumes following.",
    )
    .with(&["End"]),
    Binding::new(
        &["⌃D", "⌃U"],
        PANELS,
        Hidden,
        Say("half page"),
        "Half a page of the list, or of [0] from [2] and [3].",
    ),
    Binding::new(
        &["PgDn", "PgUp"],
        PANELS,
        Hidden,
        Say("page"),
        "A page of the focused panel as last drawn.",
    ),
    Binding::new(
        &["Space"],
        PANELS,
        RankBy(|s| Some(if s.ctx == Ctx::Agents { 6 } else { 4 })),
        Say("switch"),
        "Fuzzy switcher over every agent. Enter attaches the pick.",
    )
    .with(&["/"]),
    Binding::new(
        &["m"],
        PANELS,
        RankBy(|s| s.on_agent().then_some(3)),
        By(|s| s.on_agent().then_some(if s.agent().is_some_and(|a| a.pin == Pin::Pinned) { "unpin" } else { "pin" })),
        "Pin the agent to the pinned tab, or unpin it.",
    ),
    Binding::new(
        &["n"],
        PANELS,
        At(4),
        Say("new"),
        "New agent: a prompt, a name and a repo. In an idle worktree it starts an agent there.",
    ),
    Binding::new(
        &["c"],
        Ctxs::of(&[Ctx::Main]),
        RankBy(|s| s.diff().then_some(8)),
        By(|s| s.diff().then_some("comment")),
        "Add a comment on the line, or the selected lines, to this agent's review. s sends every comment as one message, typed at once and held under the same conditions as any message.",
    ),
    Binding::new(
        &["c"],
        Ctxs::of(&[Ctx::Agents, Ctx::Activity, Ctx::Changes]),
        Hidden,
        Say("check out"),
        "Check out a branch or a GitHub PR link for review in its own worktree.",
    ),
    Binding::new(
        &["d"],
        PANELS,
        RankBy(|s| s.can(|a| a.kill).then_some(2)),
        By(|s| s.can(|a| a.kill).then_some("kill")),
        "Kill the agent's session after confirming. The conversation survives and attaching resumes it.",
    ),
    Binding::new(
        &["D"],
        PANELS,
        RankBy(|s| s.can(|a| a.remove).then_some(1)),
        By(|s| s.can(|a| a.remove).then_some("remove")),
        "Remove the session, worktree and branch. It lists anything that would be lost and asks first.",
    ),
    Binding::new(
        &["x"],
        PANELS,
        RankBy(|s| s.can(|a| a.actions).then_some(0)),
        By(|s| s.on_agent().then_some("actions")),
        "Run one of the actions from config.toml on the agent.",
    ),
    Binding::new(
        &["a"],
        PANELS,
        Hidden,
        Say("attach next needing you"),
        "Attach the agent that needs you most: asking, then failed, then finished. The count at the right edge says how many.",
    ),
    Binding::new(
        &["{", "}"],
        PANELS,
        Hidden,
        Say("prev / next repo"),
        "Move to the previous or next repo.",
    ),
    Binding::new(
        &["p"],
        PANELS,
        Hidden,
        Say("repo picker"),
        "Focus a listed repo, or add one from the catalogue or by path.",
    ),
    Binding::new(
        &["+", "_"],
        PANELS,
        Hidden,
        Say("[0] wide / back"),
        "Give [0] the full width, or put it back.",
    ),
    Binding::new(
        &["q"],
        PANELS,
        Hidden,
        Say("quit"),
        "Quit. Agents keep running. It confirms first when review comments are drafted or a message is written and not yet typed.",
    ),
    Binding::new(
        &["?"],
        PANELS,
        At(9),
        Say("keys"),
        "This list.",
    ),
    Binding::new(
        &["{nav}"],
        PANELS,
        Hidden,
        Say("nav (from an agent)"),
        "While attached, leave the terminal and go back to NAV on that agent.",
    ),
    Binding::new(
        &["{nav} {nav}"],
        PANELS,
        Hidden,
        Say("previous agent"),
        "While attached, jump to the agent you were in before.",
    ),
    Binding::new(
        &["{nav} a"],
        PANELS,
        Hidden,
        Say("attach next needing you"),
        "While attached, jump to the agent that needs you most.",
    ),
];

#[derive(Debug, Clone)]
pub struct Row {
    pub keys: String,
    pub label: &'static str,
    pub long: &'static str,
}

/// Every key the table binds in `ctx`, for the test that presses them.
#[cfg(test)]
pub fn keys_in(ctx: Ctx) -> Vec<&'static str> {
    BINDINGS
        .iter()
        .filter(|b| b.contexts.has(ctx))
        .flat_map(Binding::all_keys)
        .collect()
}

pub struct FooterHint {
    pub keys: String,
    pub label: &'static str,
    pub rank: u8,
}

/// The footer's candidates in table order, one per key and label. The
/// caller drops the lowest ranked until the rest fit.
pub fn footer(s: &Situation, nav: &str) -> Vec<FooterHint> {
    let mut out: Vec<FooterHint> = Vec::new();
    for b in BINDINGS {
        let (Some(label), Some(rank)) = (b.label_in(s), b.rank_in(s)) else {
            continue;
        };
        let keys = expand(&b.keys.join("/"), nav);
        if !out.iter().any(|h| h.keys == keys && h.label == label) {
            out.push(FooterHint { keys, label, rank });
        }
    }
    out
}

/// The help's rows for the focused context: its own keys, then the global
/// ones, both narrowed to what `filter` appears in, case-insensitively.
#[derive(Debug, Clone, Default)]
pub struct Help {
    pub local: Vec<Row>,
    pub global: Vec<Row>,
}

impl Help {
    pub fn len(&self) -> usize {
        self.local.len() + self.global.len()
    }

    pub fn get(&self, i: usize) -> Option<&Row> {
        self.local
            .get(i)
            .or_else(|| self.global.get(i.checked_sub(self.local.len())?))
    }
}

pub fn help(s: &Situation, nav: &str, filter: &str) -> Help {
    let needle = filter.to_lowercase();
    let mut help = Help::default();
    for b in BINDINGS {
        let Some(label) = b.label_in(s) else {
            continue;
        };
        let keys = expand(&b.all_keys().collect::<Vec<_>>().join(" "), nav);
        if !needle.is_empty()
            && !keys.to_lowercase().contains(&needle)
            && !label.to_lowercase().contains(&needle)
        {
            continue;
        }
        let row = Row {
            keys,
            label,
            long: b.long,
        };
        if b.local() {
            help.local.push(row);
        } else {
            help.global.push(row);
        }
    }
    help
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_keys() -> AgentKeys {
        AgentKeys {
            enter: Some("attach"),
            kill: Avail::Offered,
            remove: Avail::Offered,
            send: Avail::Offered,
            peek: Avail::Offered,
            actions: Avail::Offered,
            window: Window::Here,
            pin: Pin::Unpinned,
            draft: Draft::Empty,
        }
    }

    fn hint(s: &Situation, key: &str) -> Option<&'static str> {
        footer(s, "⌃\\")
            .into_iter()
            .find(|h| h.keys == key)
            .map(|h| h.label)
    }

    fn situation(ctx: Ctx) -> Situation {
        Situation {
            ctx,
            selected: Selected::Agent(agent_keys()),
            reading: Reading::Subagent,
            changes_row: ChangesRow::File,
            changes_view: ChangesView::Tree,
            feed: Feed::Following,
            left: Left::Agent,
            toggle: Toggle::Worth("show all"),
            jump: None,
            reply: Quoting::No,
            next_tab: "tab: agents",
            next_changes_tab: "tab: branch",
        }
    }

    #[test]
    fn brackets_name_the_next_tab_of_whichever_panel_has_the_keys() {
        let label = |ctx| {
            BINDINGS
                .iter()
                .find(|b| b.keys == ["[", "]"] && b.contexts.has(ctx))
                .and_then(|b| b.label_in(&situation(ctx)))
        };
        assert_eq!(label(Ctx::Agents), Some("tab: agents"));
        assert_eq!(label(Ctx::Changes), Some("tab: branch"));
        assert_eq!(label(Ctx::Activity), None);
    }

    const CTXS: [Ctx; 5] = [
        Ctx::Agents,
        Ctx::Activity,
        Ctx::Changes,
        Ctx::Main,
        Ctx::Peek,
    ];

    #[test]
    fn enter_in_the_activity_names_where_it_goes_and_e_edits_its_transcript_not_the_checkout() {
        use crate::log::Jump;
        use agent_core::activity::AgentId;
        let on_an_activity = |jump| Situation {
            reading: Reading::Activity(ActivityDoc::Log),
            jump,
            ..situation(Ctx::Main)
        };
        let enter = |s: &Situation| hint(s, "⏎");
        assert_eq!(enter(&on_an_activity(None)), Some("fold"));
        assert_eq!(
            enter(&on_an_activity(Some(Jump::Subagent(AgentId::new("a"))))),
            Some("open subagent")
        );
        assert_eq!(
            enter(&on_an_activity(Some(Jump::File("a.rs".into())))),
            Some("open diff")
        );
        let help = help(&on_an_activity(None), "⌃\\", "");
        let labels: Vec<_> = help.local.iter().chain(&help.global).collect();
        assert!(
            labels
                .iter()
                .any(|r| r.keys == "e" && r.label == "edit transcript")
        );
        assert!(labels.iter().all(|r| r.label != "edit checkout"));
    }

    #[test]
    fn enter_reads_move_here_on_an_agent_open_elsewhere_and_only_in_the_list() {
        let label = hint;
        let elsewhere = |ctx| Situation {
            selected: Selected::Agent(AgentKeys {
                window: if ctx == Ctx::Agents {
                    Window::MovesHere
                } else {
                    Window::Elsewhere
                },
                enter: Some("move here"),
                ..agent_keys()
            }),
            ..situation(ctx)
        };
        assert_eq!(label(&elsewhere(Ctx::Agents), "⏎"), Some("move here"));
        assert_eq!(label(&elsewhere(Ctx::Peek), "⏎"), Some("leave paused"));
        assert_eq!(label(&elsewhere(Ctx::Main), "i"), None);
        let help = help(&elsewhere(Ctx::Agents), "⌃\\", "");
        assert!(
            help.local
                .iter()
                .chain(&help.global)
                .any(|r| r.keys == "i" && r.label == "move here")
        );
    }

    #[test]
    fn the_footer_never_repeats_a_hint_in_any_context() {
        for ctx in CTXS {
            let hints = footer(&situation(ctx), "⌃\\");
            for (i, a) in hints.iter().enumerate() {
                for b in &hints[i + 1..] {
                    assert!(
                        a.keys != b.keys && a.label != b.label,
                        "{ctx:?}: {} {} and {} {}",
                        a.keys,
                        a.label,
                        b.keys,
                        b.label
                    );
                }
            }
        }
    }

    #[test]
    fn the_help_lists_a_panels_own_keys_before_the_global_ones() {
        for ctx in CTXS {
            let help = help(&situation(ctx), "⌃\\", "");
            assert!(!help.local.is_empty(), "{ctx:?} has keys of its own");
            assert!(!help.global.is_empty(), "{ctx:?}");
            let all: Vec<&str> = (0..help.len())
                .filter_map(|i| help.get(i))
                .map(|r| r.keys.as_str())
                .collect();
            assert_eq!(all.len(), help.local.len() + help.global.len());
        }
        let detail = help(&situation(Ctx::Activity), "⌃\\", "");
        assert!(detail.local.iter().any(|r| r.label == "open"));
        assert!(detail.local.iter().any(|r| r.label == "edit transcript"));
        assert!(detail.global.iter().all(|r| r.label != "open"));
        let agents = help(&situation(Ctx::Agents), "⌃\\", "");
        assert!(agents.local.iter().any(|r| r.label == "attach"));
        assert!(agents.local.iter().all(|r| r.label != "edit file"));
    }

    #[test]
    fn the_filter_matches_keys_and_descriptions_in_any_case() {
        let s = situation(Ctx::Agents);
        let by_label = help(&s, "⌃\\", "PEEK");
        assert!(by_label.global.iter().any(|r| r.keys == "v"));
        let by_key = help(&s, "⌃\\", "pgdn");
        assert_eq!(by_key.len(), 1);
        assert_eq!(help(&s, "⌃\\", "zzzz").len(), 0);
        assert!(help(&s, "⌃\\", "").len() > by_label.len());
    }

    #[test]
    fn the_same_key_is_never_bound_twice_in_one_context() {
        for ctx in CTXS {
            let s = situation(ctx);
            let mut seen: Vec<&str> = Vec::new();
            for b in BINDINGS.iter().filter(|b| b.label_in(&s).is_some()) {
                for key in b.all_keys() {
                    assert!(!seen.contains(&key), "{ctx:?} binds {key} twice");
                    seen.push(key);
                }
            }
        }
    }
}
