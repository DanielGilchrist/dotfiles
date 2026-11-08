//! The one table of NAV and peek bindings. The footer and the help both
//! read it, so what they say can't drift apart. The key handlers in
//! `update` still do the dispatching; a test checks every key they match
//! on is in this table.

use crate::model::{Model, Panel, Reach, View, Visibility};
use crate::tree::{ChangesScope, ChangesView, Node};
use agent_core::ports::Compared;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    /// [1], the fleet.
    Agents,
    /// [2], the turn.
    Turn,
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
            Self::Turn => "[2] Turn",
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

const PANELS: Ctxs = Ctxs::of(&[Ctx::Agents, Ctx::Turn, Ctx::Changes, Ctx::Main]);
const EVERYWHERE: Ctxs = Ctxs::of(&[Ctx::Agents, Ctx::Turn, Ctx::Changes, Ctx::Main, Ctx::Peek]);
const AGENTS: Ctxs = Ctxs::of(&[Ctx::Agents]);
const LISTS: Ctxs = Ctxs::of(&[Ctx::Turn, Ctx::Changes]);

/// What the bindings' labels and ranks depend on, read once from the model.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct Situation {
    pub ctx: Ctx,
    pub on_agent: bool,
    pub on_repo: bool,
    pub enter: Option<&'static str>,
    pub can_kill: bool,
    pub can_remove: bool,
    pub can_send: bool,
    pub can_peek: bool,
    pub pinned: bool,
    pub paused: bool,
    pub hover: bool,
    pub file_row: bool,
    /// Whether the folder under [3]'s cursor is open.
    pub folder: Option<bool>,
    pub tree: bool,
    pub subagent_row: bool,
    pub left_agent: bool,
    pub can_edit_checkout: bool,
    pub has_actions: bool,
    pub toggle: Option<&'static str>,
    pub toggle_worth_showing: bool,
    pub next_tab: &'static str,
    /// What `]` names while [3] has the keys.
    pub next_changes_tab: &'static str,
}

impl Situation {
    pub fn of(m: &Model) -> Self {
        let ctx = match m.panel {
            _ if m.peeking() => Ctx::Peek,
            Panel::Agents => Ctx::Agents,
            Panel::Agent { .. } => Ctx::Turn,
            Panel::Changes { .. } => Ctx::Changes,
            Panel::Main(_) => Ctx::Main,
        };
        let selected = m.selected();
        let entry = selected.and_then(|(_, e)| e);
        let reach = entry.map(|e| m.reach(e));
        let (enter, can_kill, can_remove, can_peek) = match &reach {
            Some(Reach::Other {
                enter,
                kill,
                remove,
            }) => (
                Some(enter.verb()),
                kill.is_some(),
                remove.is_some(),
                matches!(enter, crate::model::Enter::Attach),
            ),
            Some(Reach::Own) | None => (None, false, false, false),
        };
        let left_agent = m
            .pane
            .connection()
            .is_some_and(|c| entry.is_none_or(|e| e.name() != &c.name));
        Self {
            ctx,
            on_agent: entry.is_some(),
            on_repo: matches!(selected, Some((_, None))),
            enter,
            can_kill,
            can_remove,
            can_send: entry.is_some_and(|e| m.talkable(e).is_ok()),
            can_peek,
            pinned: entry.is_some_and(|e| e.pin().is_some()),
            paused: m.docs.paused(),
            hover: m.hover().is_some(),
            file_row: m.file_under_cursor().is_some(),
            folder: m
                .folder_under_cursor()
                .map(|r| matches!(r.node, Node::Folder { open: true })),
            tree: m.changes_view == ChangesView::Tree,
            subagent_row: m.subagent_selected().is_some(),
            left_agent,
            can_edit_checkout: selected.is_some(),
            has_actions: entry.is_some() && !m.actions.is_empty(),
            toggle: (m.view != View::Pinned).then_some(match m.visibility {
                Visibility::All => "active only",
                Visibility::Active => "show all",
            }),
            toggle_worth_showing: m.visibility == Visibility::All || m.hidden() > 0,
            next_changes_tab: match m.changes_scope.toggled() {
                ChangesScope::Uncommitted => "tab: uncommitted",
                ChangesScope::Since => {
                    let trunk = entry
                        .and_then(|e| m.facts.get(e.name()))
                        .and_then(|f| f.as_ref().ok())
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
        By(|s| match (s.on_repo, s.enter) {
            (true, _) => Some("focus repo"),
            (_, verb) => verb,
        }),
        "On an agent: attach, so your keys go to its terminal (starts or resurrects the session first if needed). On a repo: show that repo's agents.",
    )
    .with(&["0"]),
    Binding::new(
        &["⏎"],
        LISTS,
        RankBy(|s| s.hover.then_some(9)),
        By(|s| match s.folder {
            Some(true) if s.ctx == Ctx::Changes => Some("collapse"),
            Some(false) if s.ctx == Ctx::Changes => Some("expand"),
            _ => s.hover.then_some("open"),
        }),
        "Open the item under the cursor in [0] and move the keys there: the turn, a subagent's transcript, a diff or the branch. On a folder in [3] it collapses or expands the folder, and 0 opens its file list.",
    )
    .with(&["0"]),
    Binding::new(
        &["⏎"],
        Ctxs::of(&[Ctx::Peek]),
        At(9),
        By(|s| s.on_agent.then_some("attach")),
        "Close the peek and attach, so your keys go to the agent's terminal.",
    ),
    Binding::new(
        &["J", "K"],
        LISTS,
        RankBy(|s| s.hover.then_some(8)),
        By(|s| s.hover.then_some("scroll")),
        "Scroll [0] two rows at a time without leaving the list.",
    ),
    Binding::new(
        &["e"],
        Ctxs::of(&[Ctx::Changes]),
        RankBy(|s| (s.file_row || s.folder.is_some()).then_some(7)),
        By(|s| match (s.file_row, s.folder) {
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
        By(|s| Some(if s.tree { "flat" } else { "tree" })),
        "Switch [3] between the directory tree and the flat list of files. The choice lasts for the session.",
    ),
    Binding::new(
        &["-", "="],
        Ctxs::of(&[Ctx::Changes]),
        Hidden,
        By(|s| s.tree.then_some("fold all / unfold all")),
        "Collapse every folder in [3], or expand them all.",
    ),
    Binding::new(
        &["e"],
        Ctxs::of(&[Ctx::Turn, Ctx::Main]),
        RankBy(|s| s.subagent_row.then_some(7)),
        By(|s| s.subagent_row.then_some("edit transcript")),
        "Open the subagent's transcript as Markdown in $EDITOR. From [0] it opens at the entry under the cursor.",
    ),
    Binding::new(
        &["⏎"],
        Ctxs::of(&[Ctx::Main]),
        RankBy(|s| s.subagent_row.then_some(8)),
        By(|s| s.subagent_row.then_some("fold")),
        "Expand or collapse the log entry under the cursor: the whole prompt or text, a call's input and result, or a run of calls.",
    ),
    Binding::new(
        &["=", "-"],
        Ctxs::of(&[Ctx::Main]),
        Hidden,
        By(|s| s.subagent_row.then_some("expand all / collapse all")),
        "Expand every entry of a subagent's log, or collapse them all.",
    ),
    Binding::new(
        &["e"],
        Ctxs::of(&[Ctx::Agents, Ctx::Main]),
        RankBy(|s| (s.ctx == Ctx::Agents).then_some(0)),
        By(|s| (s.can_edit_checkout && !(s.ctx == Ctx::Main && s.subagent_row)).then_some("edit checkout")),
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
        &["."],
        AGENTS,
        RankBy(|s| s.toggle_worth_showing.then_some(1)),
        By(|s| s.toggle),
        "Show every checkout, or only those with a session. The pinned tab always lists every pin.",
    ),
    Binding::new(
        &["Esc"],
        Ctxs::of(&[Ctx::Turn, Ctx::Changes, Ctx::Main]),
        At(9),
        Say("back"),
        "Go one level out: a focused document in [0] returns to its panel, [2] and [3] return to [1].",
    ),
    Binding::new(
        &["Esc"],
        AGENTS,
        RankBy(|s| s.left_agent.then_some(5)),
        By(|s| s.left_agent.then_some("back")),
        "Attach the agent you left with the nav key.",
    ),
    Binding::new(
        &["i"],
        EVERYWHERE,
        RankBy(|s| (s.on_agent && matches!(s.ctx, Ctx::Turn | Ctx::Changes | Ctx::Main)).then_some(8)),
        By(|s| s.on_agent.then_some("attach")),
        "Attach the selected agent from any panel, so the nav key returns to where you were.",
    ),
    Binding::new(
        &["s"],
        EVERYWHERE,
        RankBy(|s| s.can_send.then_some(7)),
        By(|s| s.on_agent.then_some("send")),
        "Compose a message to the selected agent without attaching. It is sent now if the agent is at rest, otherwise queued until it is.",
    ),
    Binding::new(
        &["v"],
        EVERYWHERE,
        RankBy(|s| (s.ctx != Ctx::Peek && s.can_peek).then_some(5)),
        By(|s| match s.ctx {
            Ctx::Peek => Some("close"),
            _ => s.on_agent.then_some("peek"),
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
        "Focus [1] the fleet, [2] the turn or [3] Changes.",
    ),
    Binding::new(
        &["j", "k"],
        PANELS,
        RankBy(|s| (s.ctx == Ctx::Main).then_some(8)),
        By(|s| Some(if s.ctx == Ctx::Main { "scroll" } else { "move" })),
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
        RankBy(|s| (s.ctx == Ctx::Main).then_some(if s.paused { 9 } else { 6 })),
        By(|s| {
            Some(match (s.ctx, s.paused) {
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
        RankBy(|s| s.on_agent.then_some(3)),
        By(|s| s.on_agent.then_some(if s.pinned { "unpin" } else { "pin" })),
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
        PANELS,
        Hidden,
        Say("check out"),
        "Check out a branch or a GitHub PR link for review in its own worktree.",
    ),
    Binding::new(
        &["d"],
        PANELS,
        RankBy(|s| s.can_kill.then_some(2)),
        By(|s| s.can_kill.then_some("kill")),
        "Kill the agent's session after confirming. The conversation survives and attaching resumes it.",
    ),
    Binding::new(
        &["D"],
        PANELS,
        RankBy(|s| s.can_remove.then_some(1)),
        By(|s| s.can_remove.then_some("remove")),
        "Remove the session, worktree and branch. It lists anything that would be lost and asks first.",
    ),
    Binding::new(
        &["x"],
        PANELS,
        RankBy(|s| s.has_actions.then_some(0)),
        By(|s| s.on_agent.then_some("actions")),
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
        "Quit. Agents keep running. It confirms first when messages are queued.",
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

    fn situation(ctx: Ctx) -> Situation {
        Situation {
            ctx,
            on_agent: true,
            on_repo: false,
            enter: Some("attach"),
            can_kill: true,
            can_remove: true,
            can_send: true,
            can_peek: true,
            pinned: false,
            paused: false,
            hover: true,
            file_row: true,
            folder: Some(true),
            tree: true,
            subagent_row: true,
            left_agent: true,
            can_edit_checkout: true,
            has_actions: true,
            toggle: Some("show all"),
            toggle_worth_showing: true,
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
        assert_eq!(label(Ctx::Turn), None);
    }

    const CTXS: [Ctx; 5] = [Ctx::Agents, Ctx::Turn, Ctx::Changes, Ctx::Main, Ctx::Peek];

    #[test]
    fn every_key_the_handlers_match_is_in_the_table() {
        let source = include_str!("update.rs");
        let start = source.find("fn on_peek_key").expect("peek handler");
        let end = source.find("fn run_action").expect("end of the handlers");
        let handlers = &source[start..end];
        let table: Vec<&str> = BINDINGS.iter().flat_map(Binding::all_keys).collect();
        let mut missing = Vec::new();
        for part in handlers.split("KeyCode::Char(").skip(1) {
            let mut chars = part.chars();
            if chars.next() != Some('\'') {
                continue;
            }
            let key = match chars.next() {
                Some(' ') => "Space".to_string(),
                Some(c) => c.to_string(),
                None => continue,
            };
            let ctrl_only = chars.as_str().starts_with("') if ctrl");
            if key != "t" && !ctrl_only && !table.contains(&key.as_str()) {
                missing.push(key);
            }
        }
        for (code, shown) in [
            ("KeyCode::Enter", "⏎"),
            ("KeyCode::Esc", "Esc"),
            ("KeyCode::Tab", "Tab"),
            ("KeyCode::BackTab", "⇧Tab"),
            ("KeyCode::PageDown", "PgDn"),
            ("KeyCode::PageUp", "PgUp"),
            ("KeyCode::Home", "Home"),
            ("KeyCode::End", "End"),
            ("KeyCode::Up", "↑"),
            ("KeyCode::Down", "↓"),
        ] {
            if handlers.contains(code) && !table.contains(&shown) {
                missing.push(shown.to_string());
            }
        }
        assert!(
            missing.is_empty(),
            "handled but not in the table: {missing:?}"
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
        let turn = help(&situation(Ctx::Turn), "⌃\\", "");
        assert!(turn.local.iter().any(|r| r.label == "open"));
        assert!(turn.local.iter().any(|r| r.label == "edit transcript"));
        assert!(turn.global.iter().all(|r| r.label != "open"));
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
