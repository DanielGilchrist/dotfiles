use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use agent_core::actions::{Action, CommandLine, Context, EventName, Invocation};
use agent_core::activity::{Activity, AgentId, State as ActivityState};
use agent_core::config::ControlKey;
use agent_core::model::{EntryState, RepoKey};
use agent_core::names::{CarriageReturn, InvalidPrompt, Prompt, SessionName, clean};
use agent_core::ops::{Approved, Launch};
use agent_core::paths::{RepoRoot, Typed, typed_path};
use agent_core::ports::{ClientId, Listing};
use agent_term::{CellPos, encode_key, encode_mouse};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::document::Key;
use crate::keymap::Situation;
use crate::model::{
    Answer, Chord, Confirm, Cursor, Direction, Edge, Enter, Field, Focus, Follow, FormKind,
    HOLD_FOR, HelpState, Hold, INFO_TTL, Inspection, Intent, Mode, Model, NamingState, NewAgent,
    Overlay, Pane, Panel, Phase, Query, Reach, Screen, StatusKind, SubagentRead, Submission,
    TerminalFocus, Ticket, Unreachable, View, WorkRoom, fuzzy_match, match_rank, repo_matches,
    unlisted,
};
use crate::tree::ChangesView;
use crate::worker::{Job, Msg, Outcome, SendError};

/// How long the pane shows a finished agent, terminal focused, before it
/// counts as seen.
const SEEN_AFTER: Duration = Duration::from_secs(3);
/// At most one bell this often.
const BELL_EVERY: Duration = Duration::from_secs(10);
/// A turn this long rings when it finishes; shorter ones you were probably
/// watching.
const LONG_TURN: Duration = Duration::from_secs(30);
/// An agent's git facts refresh on its activity, at most this often.
const REINSPECT_AFTER: Duration = Duration::from_secs(3);
/// And this often whatever it does.
const SLOW_REFRESH: Duration = Duration::from_secs(30);
/// The subagent under [2]'s cursor is read again on its agent's activity, at
/// most this often.
const SUBAGENT_REFRESH: Duration = Duration::from_secs(2);
/// And this often whatever it does, since a transcript can be written
/// without a hook firing.
const SUBAGENT_SLOW_REFRESH: Duration = Duration::from_secs(10);

/// Rows `J` and `K` scroll the main view by.
const SCROLL: isize = 2;

/// What `$EDITOR` opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Open {
    /// A checkout, as the editor's working directory.
    Checkout(PathBuf),
    /// A file, relative to the checkout the editor runs in.
    File { root: PathBuf, file: PathBuf },
    /// A file at a line.
    At {
        root: PathBuf,
        file: PathBuf,
        line: NonZeroUsize,
    },
    /// A subagent's transcript as Markdown, at `line`.
    Transcript {
        root: PathBuf,
        name: String,
        text: String,
        line: NonZeroUsize,
    },
}

pub enum Effect {
    Job(Job),
    /// Read git facts about an agent's worktree.
    Inspect {
        name: SessionName,
        worktree: PathBuf,
    },
    /// Read a diff or a branch log in a worktree.
    ReadDocument {
        key: Key,
        worktree: PathBuf,
        /// Diffs already read, for a folder's to take.
        reuse: Vec<(PathBuf, agent_core::ports::FileDiff)>,
    },
    /// Read what a subagent of an agent working in `workdir` has done.
    ReadSubagent {
        name: SessionName,
        id: AgentId,
        workdir: PathBuf,
    },
    /// Attach to an existing session (resurrecting it if it exited).
    Attach {
        name: SessionName,
        intent: Intent,
        ticket: Ticket,
    },
    Connect {
        launch: Launch,
        intent: Intent,
        ticket: Ticket,
    },
    /// Suspend the TUI and open `$EDITOR`.
    Edit(Open),
    /// Suspend the TUI and edit the new-agent task in `$EDITOR`.
    EditTask(String),
    Quit,
    /// Ring the terminal's bell.
    Bell,
    /// Put text on the clipboard through the terminal (OSC 52).
    Copy(String),
    /// Ask the terminal the TUI runs in to set a user variable.
    TerminalEvent {
        name: EventName,
        value: OsString,
    },
    /// Suspend the TUI and run an action's command in its terminal.
    Interactive {
        label: String,
        launch: CommandLine,
    },
}

/// Whether `key` is `control`, in either encoding terminals send.
pub fn pressed(key: &KeyEvent, control: ControlKey) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char(c) if c == control.char() || Some(c) == control.legacy_digit())
}

fn ctrl(key: &KeyEvent, c: char) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(c)
}

fn own_client(m: &Model) -> Option<ClientId> {
    m.pane
        .connection()
        .and_then(|c| c.term.pid())
        .map(ClientId::new)
}

pub fn update(m: &mut Model, msg: Msg) -> Vec<Effect> {
    let effects = step(m, msg);
    m.hold_hovered_subagent();
    forget_subagents(m);
    effects
}

fn step(m: &mut Model, msg: Msg) -> Vec<Effect> {
    match msg {
        Msg::Key(key) if key.kind == KeyEventKind::Release => Vec::new(),
        Msg::Key(key) => {
            if m.status
                .as_ref()
                .is_some_and(|s| s.kind == StatusKind::Error)
            {
                m.status = None;
            }
            on_key(m, key)
        }
        Msg::Mouse(mouse) => on_mouse(m, mouse),
        Msg::Paste(text) if matches!(m.overlay, Overlay::New(_)) => {
            edit_form(m, |form| {
                for c in sanitize_paste(&text).chars() {
                    type_into(form, c);
                }
            });
            Vec::new()
        }
        Msg::Paste(text) if matches!(m.overlay, Overlay::Talk { .. } | Overlay::Comment { .. }) => {
            if let Overlay::Talk { text: typed, .. } | Overlay::Comment { text: typed, .. } =
                &mut m.overlay
            {
                typed.push_str(&clean(&text, CarriageReturn::Newline, None));
            }
            Vec::new()
        }
        Msg::Paste(text) => {
            if let Some(c) = m.pane.connection().filter(|c| c.mode == Mode::Agent) {
                let text = sanitize_paste(&text);
                let bytes = if c.term.modes().bracketed_paste {
                    format!("\x1b[200~{text}\x1b[201~")
                } else {
                    text
                };
                c.term.send(bytes.into_bytes());
            }
            Vec::new()
        }
        Msg::Resize => Vec::new(),
        Msg::Snapshot(Ok((snapshot, warnings))) => {
            m.load(snapshot);
            track_live(m);
            forget_gone(m);
            if !warnings.is_empty() && warnings != m.last_warnings {
                let more = if warnings.len() > 1 {
                    format!(" (+{} more in `agent ls`)", warnings.len() - 1)
                } else {
                    String::new()
                };
                m.error(format!("discovery: {}{more}", warnings[0]));
            }
            m.last_warnings = warnings;
            Vec::new()
        }
        Msg::Snapshot(Err(e)) | Msg::Done(Err(e)) => {
            m.error(e);
            Vec::new()
        }
        Msg::RemovalPlanned { ticket, result } => {
            if !matches!(&m.overlay, Overlay::Planning { ticket: t, .. } if *t == ticket) {
                return Vec::new();
            }
            m.overlay = Overlay::None;
            match result {
                Ok(plan) => m.overlay = Overlay::Confirm(Confirm::Remove(plan.into())),
                Err(e) => m.error(e),
            }
            Vec::new()
        }
        Msg::Launched { ticket, result } => on_launched(m, ticket, result),
        Msg::Done(Ok(outcome)) => {
            report(m, outcome);
            Vec::new()
        }
        Msg::NameSuggested { generation, name } => {
            if let Overlay::New(form) = &mut m.overlay {
                form.model_named(generation, name);
            }
            Vec::new()
        }
        Msg::Shared { name, others } => {
            if let Some(c) = m.pane.connection_mut().filter(|c| c.name == name) {
                c.others = NonZeroUsize::new(others);
            }
            Vec::new()
        }
        Msg::Sent { to, result } => on_sent(m, &to, result),
        Msg::Activities(activities) => on_activities(m, activities),
        Msg::Catalogue { repos, warnings } => {
            m.catalogue = repos;
            if !warnings.is_empty() && warnings != m.catalogue_warnings {
                let more = if warnings.len() > 1 {
                    format!(" (+{} more)", warnings.len() - 1)
                } else {
                    String::new()
                };
                m.error(format!("{}{more}", warnings[0]));
            }
            m.catalogue_warnings = warnings;
            Vec::new()
        }
        Msg::Focus(focus) => {
            m.terminal = focus;
            Vec::new()
        }
        Msg::Inspected { name, facts } => {
            if m.inspections.contains_key(&name) {
                m.set_facts(&name, facts);
            }
            Vec::new()
        }
        Msg::Document { key, result } => {
            m.docs.loaded(&key, result);
            Vec::new()
        }
        Msg::Subagent { name, id, detail } => {
            let key = (name, id);
            if m.subagent_reads.contains_key(&key) {
                m.subagent_logs.insert(key, detail);
            }
            Vec::new()
        }
        Msg::TookOver { name, detached } => {
            match detached {
                Ok(n) => {
                    if let Some(c) = m.pane.connection_mut().filter(|c| c.name == name) {
                        c.others = None;
                    }
                    if n > 0 {
                        m.info(format!("took over {name} from {n} other client(s)"));
                    }
                }
                Err(e) => m.error(e),
            }
            Vec::new()
        }
    }
}

/// Asks the harness to name the form's prompt once typing has paused.
fn ask_for_name(m: &mut Model) -> Option<Effect> {
    let Overlay::New(form) = &mut m.overlay else {
        return None;
    };
    let NamingState::Due(at) = form.naming.state else {
        return None;
    };
    if Instant::now() < at {
        return None;
    }
    form.naming.state = NamingState::Idle;
    let prompt = form.task.parse::<agent_core::names::Prompt>().ok()?;
    form.naming.state = NamingState::Asking;
    let generation = form.naming.generation;
    Some(Effect::Job(Job::SuggestName {
        prompt,
        harness: m.harness.clone(),
        generation,
    }))
}

/// Periodic work: debounced preview attach, naming, and status expiry.
pub fn tick(m: &mut Model) -> Vec<Effect> {
    if m.status
        .as_ref()
        .is_some_and(|s| s.kind == StatusKind::Info && s.at.elapsed() > INFO_TTL)
    {
        m.status = None;
    }
    m.now = SystemTime::now();
    let mut effects: Vec<Effect> = ask_for_name(m).into_iter().collect();
    effects.extend(bell(m));
    effects.extend(flush(m));
    effects.extend(acknowledge(m, SEEN_AFTER).map(Effect::Job));
    effects.extend(preview(m));
    effects.extend(inspect(m));
    effects.extend(read_subagent(m));
    effects.extend(read_document(m));
    effects
}

/// Attaches the selected live agent once the cursor has rested on it.
fn preview(m: &mut Model) -> Option<Effect> {
    let due = m.preview_at.is_some_and(|t| Instant::now() >= t);
    if !due || matches!(m.focus(), Focus::Agent(_)) {
        return None;
    }
    m.preview_at = None;
    let target = m.selected_entry().and_then(|e| match m.reach(e) {
        Reach::Other {
            enter: Enter::Attach,
            ..
        } => Some(e.name().clone()),
        _ => None,
    });
    match target {
        Some(name) if m.pane.session() == Some(&name) => None,
        Some(name) => {
            let ticket = m.ticket();
            m.pane = Pane::Connecting {
                name: name.clone(),
                ticket,
            };
            Some(Effect::Attach {
                name,
                intent: Intent::Preview,
                ticket,
            })
        }
        None => {
            m.pane = Pane::Empty;
            None
        }
    }
}

/// A start or create finished: connect, or say why it failed where the
/// user is looking (the form that asked, or the status bar).
fn on_launched(m: &mut Model, ticket: Ticket, result: Result<Launch, String>) -> Vec<Effect> {
    m.launching.retain(|(t, _)| *t != ticket);
    match result {
        Ok(launch) => {
            m.launched_at
                .insert(launch.session.clone(), SystemTime::now());
            if matches!(&m.overlay, Overlay::New(form) if matches!(form.phase, Phase::Submitting { ticket: t, .. } if t == ticket))
            {
                m.overlay = Overlay::None;
            }
            // A checkout reaches the worktree already on its branch, whatever
            // name the form had.
            if let Some(
                Follow::Submitted { name, ticket: t } | Follow::Backgrounded { name, ticket: t },
            ) = &mut m.follow
                && *t == ticket
            {
                name.clone_from(&launch.session);
            }
            // A backgrounded launch no longer has the pane waiting on it, so
            // this connects nothing; its follow previews it instead.
            vec![Effect::Connect {
                launch,
                intent: Intent::Activate,
                ticket,
            }]
        }
        Err(e) => {
            m.pane.abandon(ticket);
            m.follow = m.follow.take().filter(|f| {
                !matches!(f, Follow::Submitted { ticket: t, .. } | Follow::Backgrounded { ticket: t, .. } if *t == ticket)
            });
            if let Overlay::New(form) = &mut m.overlay
                && matches!(form.phase, Phase::Submitting { ticket: t, .. } if t == ticket)
            {
                form.phase = Phase::Editing;
                form.error = Some(e);
                return Vec::new();
            }
            m.error(e);
            Vec::new()
        }
    }
}

/// Says how a finished job went.
fn report(m: &mut Model, outcome: Outcome) {
    match outcome {
        Outcome::Killed(name) => m.info(format!("killed {name} · ⏎ resurrects")),
        Outcome::Removed { name, warnings } if warnings.is_empty() => {
            m.info(format!("removed {name}"));
        }
        Outcome::Removed { name, warnings } => {
            m.error(format!("removed {name}, but {}", warnings.join(", ")));
        }
        Outcome::Pinned(name) => m.info(format!("pinned {name}")),
        Outcome::Listed(listed) => {
            let name = listed.repo.name().into_owned();
            match listed.listing {
                Listing::Added => m.info(format!("listed {name}")),
                Listing::AlreadyListed => m.info(format!("{name} is already listed")),
            }
            // A launch in flight keeps its follow: the repo is still listed.
            if !matches!(
                m.follow,
                Some(Follow::Submitted { .. } | Follow::Backgrounded { .. })
            ) {
                m.follow = Some(Follow::Repo {
                    root: listed.repo,
                    until: m.snapshots + 2,
                });
            }
        }
        Outcome::Unpinned(name) => m.info(format!("unpinned {name}")),
        Outcome::Started(label) => m.info(format!("started {label}")),
        Outcome::Finished(label) => m.info(format!("{label} finished")),
    }
}

/// New reports from the agents: queue a bell if one newly needs the user,
/// and move the cursor off a Needs you row that's gone.
fn on_activities(m: &mut Model, activities: HashMap<SessionName, Activity>) -> Vec<Effect> {
    if newly_waiting(m, &activities) {
        let allowed = m.last_bell.map_or_else(Instant::now, |at| at + BELL_EVERY);
        m.bell_due = Some(m.bell_due.map_or(allowed, |due| due.min(allowed)));
    }
    m.activities = Some(activities);
    forget_subagents(m);
    m.revalidate();
    release_holds(m);
    flush(m)
}

/// Drops what's known about subagents their agents no longer list.
fn forget_subagents(m: &mut Model) {
    let Some(activities) = &m.activities else {
        return;
    };
    let held = m.held_subagent.as_ref().map(|h| (&h.by, &h.id));
    let listed = |(name, id): &(SessionName, AgentId)| {
        held == Some((name, id))
            || activities
                .get(name)
                .is_some_and(|a| a.subagents.contains_key(id))
    };
    m.subagent_reads.retain(|key, _| listed(key));
    m.subagent_logs.retain(|key, _| listed(key));
    m.log_view.navs.retain(|key, _| listed(key));
}

/// Asks for what [3]'s cursor is on once it has rested there.
fn read_document(m: &mut Model) -> Option<Effect> {
    m.settle_main();
    let (key, worktree) = m.hovered_document()?;
    let due = m.docs.due(Instant::now())?;
    debug_assert_eq!(due, key);
    let reuse = match &due {
        Key::Folder(folder) => {
            m.docs
                .cached_diffs(&folder.name, folder.base, folder.generation, &folder.files)
        }
        Key::Diff(_) | Key::Branch(_) | Key::Commit(_) => Vec::new(),
    };
    Some(Effect::ReadDocument {
        key: due,
        worktree,
        reuse,
    })
}

/// Asks for what the subagent under [2]'s cursor has done: when it's first
/// shown, soon after its agent reports something, and every
/// [`SUBAGENT_SLOW_REFRESH`] regardless.
fn read_subagent(m: &mut Model) -> Option<Effect> {
    let (name, id) = m.subagent_selected()?;
    let workdir = m.selected_entry()?.path().map(Path::to_path_buf)?;
    let events = m.activity(&name)?.events;
    let now = Instant::now();
    let key = (name.clone(), id.clone());
    if matches!(m.subagent_logs.get(&key), Some(Ok(Some(detail))) if detail.finished) {
        return None;
    }
    let due = m.subagent_reads.get(&key).is_none_or(|last| {
        now >= last.at + SUBAGENT_SLOW_REFRESH
            || (last.events != events && now >= last.at + SUBAGENT_REFRESH)
    });
    if !due {
        return None;
    }
    m.subagent_reads
        .insert(key, SubagentRead { events, at: now });
    Some(Effect::ReadSubagent { name, id, workdir })
}

/// The agent in the pane has finished a turn and been in view for `after`
/// with the terminal focused: mark it seen.
fn acknowledge(m: &mut Model, after: Duration) -> Option<Job> {
    let c = m
        .pane
        .connection()
        .filter(|c| c.opened.elapsed() >= after)
        .filter(|_| m.terminal_visible())?;
    if m.terminal == TerminalFocus::Unfocused {
        return None;
    }
    let activity = m.activity(&c.name)?;
    let ActivityState::Done { .. } = activity.state else {
        return None;
    };
    let mark = (c.name.clone(), activity.events);
    if m.acknowledged.as_ref() == Some(&mark) {
        return None;
    }
    m.acknowledged = Some(mark);
    Some(Job::Acknowledge(c.name.clone()))
}

/// Drops what's known about entries that are no longer in the snapshot.
fn forget_gone(m: &mut Model) {
    let Some(snapshot) = m.snapshot() else {
        return;
    };
    let known: HashSet<SessionName> = snapshot.entries().map(|(_, e)| e.name().clone()).collect();
    m.inspections.retain(|name, _| known.contains(name));
    m.subagent_reads.retain(|(name, _), _| known.contains(name));
    m.subagent_logs.retain(|(name, _), _| known.contains(name));
    m.facts.retain(|name, _| known.contains(name));
    m.outboxes.retain(|name, _| known.contains(name));
    m.drafts.retain(|name, _| known.contains(name));
    m.live_since.retain(|name, _| known.contains(name));
    m.launched_at.retain(|name, _| known.contains(name));
    if m.peek.as_ref().is_some_and(|name| !known.contains(name)) {
        m.peek = None;
    }
    m.docs.forget(|name| known.contains(name));
}

/// Asks for git facts about every agent with a checkout: soon after it
/// reports something, and every [`SLOW_REFRESH`] regardless, since the
/// user's own edits fire no hook. A checkout git can't read is only retried
/// at the slow pace.
fn inspect(m: &mut Model) -> Vec<Effect> {
    let Some(snapshot) = m.snapshot() else {
        return Vec::new();
    };
    let now = Instant::now();
    let due: Vec<_> = snapshot
        .entries()
        .filter(|(_, e)| e.head().is_some())
        .filter_map(|(_, entry)| {
            let events = m.activity(entry.name()).map(|a| a.events);
            let failing = matches!(m.facts.get(entry.name()), Some(Err(_)));
            let due = m.inspections.get(entry.name()).is_none_or(|last| {
                now >= last.at + SLOW_REFRESH
                    || (!failing && last.events != events && now >= last.at + REINSPECT_AFTER)
            });
            if !due {
                return None;
            }
            Some((entry.name().clone(), entry.path()?.to_path_buf(), events))
        })
        .collect();
    due.into_iter()
        .map(|(name, worktree, events)| {
            m.inspections
                .insert(name.clone(), Inspection { events, at: now });
            Effect::Inspect { name, worktree }
        })
        .collect()
}

/// Rings the queued bell once the rate limit allows.
fn bell(m: &mut Model) -> Option<Effect> {
    m.bell_due.filter(|due| Instant::now() >= *due)?;
    m.bell_due = None;
    m.last_bell = Some(Instant::now());
    Some(Effect::Bell)
}

/// Whether an agent newly needs the user: asking, failed, or finished a
/// turn that took a while. Not the agent being typed into or the TUI's own
/// session, and nothing on the first report, when all of it is old news.
fn newly_waiting(m: &Model, next: &HashMap<SessionName, Activity>) -> bool {
    let Some(previous) = m.activities.as_ref() else {
        return false;
    };
    let typing_into = match m.focus() {
        Focus::Agent(c) => Some(&c.name),
        Focus::Overlay(_) | Focus::Nav(_) => None,
    };
    next.iter()
        .filter(|(name, _)| Some(*name) != typing_into && Some(*name) != m.own_session.as_ref())
        .any(|(name, activity)| {
            let before = previous.get(name);
            let asks_anew = activity
                .asks
                .iter()
                .any(|ask| before.is_none_or(|b| !b.asks.contains(ask)));
            asks_anew
                || match (&activity.state, before.map(|b| (&b.state, b.since))) {
                    (ActivityState::Failed { .. }, Some((ActivityState::Failed { .. }, _)))
                    | (ActivityState::Done { .. }, Some((ActivityState::Done { .. }, _))) => false,
                    (ActivityState::Failed { .. }, _) => true,
                    (
                        ActivityState::Done { .. },
                        Some((ActivityState::Working { .. } | ActivityState::Delegating, started)),
                    ) => activity.since.duration_since(started).unwrap_or_default() >= LONG_TURN,
                    _ => false,
                }
        })
}

/// Keys now go to the connected agent; other clients are detached so the
/// session takes this pane's size.
pub fn enter_agent(m: &mut Model) -> Vec<Effect> {
    let own = own_client(m);
    let Some(c) = m.pane.connection_mut() else {
        return Vec::new();
    };
    c.mode = Mode::Agent;
    let name = c.name.clone();
    m.peek = None;
    m.touch_mru(&name);
    m.preview_at = None;
    let mut effects = vec![Effect::Job(Job::TakeOver { name, own })];
    effects.extend(acknowledge(m, Duration::ZERO).map(Effect::Job));
    effects
}

fn activate(m: &mut Model, name: &SessionName) -> Vec<Effect> {
    let Some(reach) = m
        .snapshot()
        .and_then(|s| s.entry(name))
        .map(|(_, e)| m.reach(e))
    else {
        return Vec::new();
    };
    m.overlay = Overlay::None;
    m.peek = None;
    if !matches!(m.cursor(), Some(Cursor::Grouped(_, n)) if n == name) {
        m.set_cursor(Cursor::Entry(name.clone()));
    }
    m.preview_at = None;
    // Its worktree can show up before its launch has finished, and a
    // second start from here would race it and skip the first prompt.
    if m.launching.iter().any(|(_, n)| n == name) {
        m.info(format!("{name} is still starting"));
        return Vec::new();
    }
    let Reach::Other { enter, .. } = reach else {
        m.error(format!("this TUI is running inside {name}"));
        return Vec::new();
    };
    if m.pane.connection().is_some_and(|c| &c.name == name) {
        return enter_agent(m);
    }
    let ticket = m.ticket();
    m.pane = Pane::Connecting {
        name: name.clone(),
        ticket,
    };
    if !matches!(enter, Enter::Attach) {
        m.launched_at.insert(name.clone(), SystemTime::now());
    }
    match enter {
        Enter::Attach | Enter::Resurrect => vec![Effect::Attach {
            name: name.clone(),
            intent: Intent::Activate,
            ticket,
        }],
        Enter::Start(at) => {
            m.info(format!("starting {name}…"));
            vec![Effect::Job(Job::Start {
                at,
                harness: m.harness.clone(),
                ticket,
            })]
        }
        Enter::Restart(at) => {
            m.info(format!("resurrecting {name}…"));
            vec![Effect::Job(Job::Start {
                at,
                harness: m.harness.clone(),
                ticket,
            })]
        }
    }
}

fn on_key(m: &mut Model, key: KeyEvent) -> Vec<Effect> {
    if !matches!(m.overlay, Overlay::None) {
        return on_overlay_key(m, key);
    }
    let nav = m.keys.nav();
    if let Some(c) = m.pane.connection_mut().filter(|c| c.mode == Mode::Agent) {
        if pressed(&key, nav) {
            c.mode = Mode::Nav;
            m.chord = Chord::Armed;
            let name = c.name.clone();
            if m.selected_entry().is_none_or(|e| e.name() != &name) {
                m.set_cursor(Cursor::Entry(name));
                m.preview_at = None;
            }
        } else {
            c.term.send(encode_key(key, c.term.modes()));
        }
        return Vec::new();
    }
    if m.peek.is_some() {
        return on_peek_key(m, key.code);
    }
    on_nav_key(m, key)
}

const SEND_MOVED: &str = "send is s now";

/// The peek float reads the terminal and passes nothing to it.
fn on_peek_key(m: &mut Model, code: KeyCode) -> Vec<Effect> {
    let Some(name) = m.peek.clone() else {
        return Vec::new();
    };
    match code {
        KeyCode::Esc | KeyCode::Char('v') => m.peek = None,
        KeyCode::Char('s') => open_talk(m, &name),
        KeyCode::Char('t') => m.info(SEND_MOVED),
        KeyCode::Char('i') | KeyCode::Enter => return activate(m, &name),
        _ => {}
    }
    Vec::new()
}

/// `e` on a subagent row: its whole transcript, in the agent's checkout.
fn edit_transcript(m: &mut Model) -> Option<Vec<Effect>> {
    let key = m.subagent_selected()?;
    let root = m.selected_entry()?.path()?.to_path_buf();
    match m.subagent_logs.get(&key) {
        Some(Ok(Some(detail))) if !detail.transcript.as_os_str().is_empty() => {
            Some(vec![Effect::Edit(Open::File {
                root,
                file: detail.transcript.clone(),
            })])
        }
        _ => {
            m.info("no transcript for this subagent yet");
            Some(Vec::new())
        }
    }
}

/// Copies `text` to the clipboard and says so.
fn copy(m: &mut Model, text: String) -> Vec<Effect> {
    if text.is_empty() {
        m.info("nothing to copy");
        return Vec::new();
    }
    let shown: String = text
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(40)
        .collect();
    m.info(format!("copied {shown}"));
    vec![Effect::Copy(text)]
}

/// The commit the diff in [0] is of, when it is one.
pub fn commit_rev(m: &Model) -> Option<String> {
    match m.hover() {
        Some(crate::document::Hover::Commit(key)) => Some(key.sha),
        _ => None,
    }
}

/// Puts a comment on `lines` into `to`'s draft, replacing the one that is
/// already on the first of them. Empty text deletes that one instead.
fn add_comment(
    m: &mut Model,
    to: &SessionName,
    lines: &[crate::review::LineInfo],
    text: &str,
    rev: Option<String>,
) {
    let refs: Vec<&crate::review::LineInfo> = lines.iter().collect();
    let Some(comment) = crate::review::Comment::on(&refs, rev.as_deref(), text.trim()) else {
        return;
    };
    if let Some(c) = agent_core::names::first_control(text) {
        m.info(format!(
            "the comment has a control character ({c:?}) · Esc closes"
        ));
        m.overlay = Overlay::Comment {
            to: to.clone(),
            lines: lines.to_vec(),
            text: text.to_string(),
            rev,
        };
        return;
    }
    let at = m
        .drafts
        .get(to)
        .and_then(|d| d.iter().position(|c| c.same_place(&comment)));
    match (at, text.trim().is_empty()) {
        (None, true) => {
            m.info("type a comment first · Esc closes");
            m.overlay = Overlay::Comment {
                to: to.clone(),
                lines: lines.to_vec(),
                text: String::new(),
                rev,
            };
            return;
        }
        (Some(i), true) => {
            m.drafts.entry(to.clone()).or_default().remove(i);
        }
        (Some(i), false) => m.drafts.entry(to.clone()).or_default()[i] = comment,
        (None, false) => m.drafts.entry(to.clone()).or_default().push(comment),
    }
    if m.drafts.get(to).is_some_and(Vec::is_empty) {
        m.drafts.remove(to);
    }
    m.diff_view.anchor = None;
    let n = m.drafts.get(to).map_or(0, Vec::len);
    m.info(format!(
        "{n} comment{} drafted for {to} · s sends them",
        if n == 1 { "" } else { "s" }
    ));
}

/// Opens the comment box on the cursor's lines, filled with the comment
/// already on them.
fn open_comment(m: &mut Model) {
    let Some(name) = m.selected_entry().map(|e| e.name().clone()) else {
        return;
    };
    let (from, to) = m.diff_view.span();
    let lines: Vec<crate::review::LineInfo> = m
        .docs
        .lines()
        .get(from..=to)
        .unwrap_or_default()
        .iter()
        .filter(|l| l.selectable())
        .cloned()
        .collect();
    let Some(first) = lines.first() else {
        return;
    };
    let rev = commit_rev(m);
    let existing = m
        .drafts
        .get(&name)
        .and_then(|d| d.iter().find(|c| c.covers(first, rev.as_deref())));
    m.overlay = Overlay::Comment {
        to: name,
        text: existing.map(|c| c.text.clone()).unwrap_or_default(),
        lines,
        rev,
    };
}

/// Keys for [0] while it reads the branch: a cursor over its commits and
/// uncommitted files, `⏎` into one's diff and `e` on a file.
fn branch_key(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    if !matches!(m.hover(), Some(crate::document::Hover::Branch(_))) {
        return None;
    }
    let lines = m.docs.lines();
    if lines.is_empty() {
        return None;
    }
    let at = m.diff_view.cursor;
    let mut effects = Vec::new();
    let to = match code {
        KeyCode::Char('j') | KeyCode::Down => crate::review::step(lines, at, 1),
        KeyCode::Char('k') | KeyCode::Up => crate::review::step(lines, at, -1),
        KeyCode::Char('g') | KeyCode::Home => crate::review::first(lines),
        KeyCode::Char('G') | KeyCode::End => crate::review::last(lines),
        KeyCode::Enter => {
            let line = lines.get(at)?;
            m.opened = Some(match line.kind {
                crate::review::LineKind::Commit => {
                    let (sha, subject) = line.text.split_once(' ').unwrap_or((&line.text, ""));
                    crate::model::Opened::Commit {
                        sha: sha.to_string(),
                        subject: subject.to_string(),
                    }
                }
                crate::review::LineKind::Entry => crate::model::Opened::File {
                    path: line.path.clone(),
                    code: line.text.clone(),
                },
                _ => return Some(effects),
            });
            m.diff_view.owner = None;
            return Some(effects);
        }
        KeyCode::Char('y') => {
            let line = lines.get(at)?;
            let text = match line.kind {
                crate::review::LineKind::Commit => {
                    line.text.split(' ').next().unwrap_or_default().to_string()
                }
                _ => line.path.to_string_lossy().into_owned(),
            };
            return Some(copy(m, text));
        }
        KeyCode::Char('e') => {
            let line = lines.get(at)?;
            if line.kind != crate::review::LineKind::Entry {
                return None;
            }
            let root = m.selected_entry()?.path()?.to_path_buf();
            effects.push(Effect::Edit(Open::File {
                root,
                file: line.path.clone(),
            }));
            return Some(effects);
        }
        KeyCode::PageDown | KeyCode::PageUp => {
            let half = m.docs.half_page() * 2;
            m.docs.scroll_by(if code == KeyCode::PageDown {
                half
            } else {
                -half
            });
            m.diff_view.follow_page = true;
            return Some(effects);
        }
        _ => return None,
    };
    if let Some(to) = to {
        m.diff_view.cursor = to;
        m.diff_view.reveal = true;
    }
    Some(effects)
}

/// `e` on a diff line: the checkout's file at that line. A commit's lines
/// aren't the checkout's, so there it says so instead.
fn edit_line(m: &mut Model) -> Option<Vec<Effect>> {
    if commit_rev(m).is_some() {
        m.info("e opens the checkout, not a commit");
        return Some(Vec::new());
    }
    let root = m.selected_entry()?.path()?.to_path_buf();
    let line = m.docs.lines().get(m.diff_view.cursor)?;
    Some(vec![Effect::Edit(Open::At {
        root,
        file: line.path.clone(),
        line: line.edit,
    })])
}

/// Keys for [0] while it reads a diff: the line cursor, hunk jumps and
/// editing at the line. `None` leaves the key to the plain document keys.
fn diff_key(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    if !matches!(
        m.hover(),
        Some(
            crate::document::Hover::Diff(_)
                | crate::document::Hover::Folder(_)
                | crate::document::Hover::Commit(_)
        )
    ) {
        return None;
    }
    let lines = m.docs.lines();
    if lines.is_empty() {
        return None;
    }
    let at = m.diff_view.cursor;
    let to = match code {
        KeyCode::Char('j') | KeyCode::Down => crate::review::step(lines, at, 1),
        KeyCode::Char('k') | KeyCode::Up => crate::review::step(lines, at, -1),
        KeyCode::Char('g') | KeyCode::Home => crate::review::first(lines),
        KeyCode::Char('G') | KeyCode::End => crate::review::last(lines),
        KeyCode::Char(']') => crate::review::hunk(lines, at, true),
        KeyCode::Char('[') => crate::review::hunk(lines, at, false),
        KeyCode::PageDown | KeyCode::PageUp => {
            let half = m.docs.half_page() * 2;
            m.docs.scroll_by(if code == KeyCode::PageDown {
                half
            } else {
                -half
            });
            m.diff_view.follow_page = true;
            return Some(Vec::new());
        }
        KeyCode::Char('c') => {
            open_comment(m);
            return Some(Vec::new());
        }
        KeyCode::Char('y') => {
            let (from, to) = m.diff_view.span();
            let selected: Vec<&crate::review::LineInfo> = lines
                .get(from..=to)
                .unwrap_or_default()
                .iter()
                .filter(|l| l.selectable())
                .collect();
            let text = if m.diff_view.anchor.is_some() {
                selected
                    .iter()
                    .map(|l| l.text.chars().skip(1).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                let line = lines.get(at)?;
                crate::review::label(&line.path, commit_rev(m).as_deref(), &[line])
            };
            m.diff_view.anchor = None;
            return Some(copy(m, text));
        }
        KeyCode::Char('V') => {
            m.diff_view.anchor = match m.diff_view.anchor {
                Some(_) => None,
                None => Some(at),
            };
            return Some(Vec::new());
        }
        KeyCode::Char(c @ ('J' | 'K')) => {
            let forward = c == 'J';
            if matches!(m.hover(), Some(crate::document::Hover::Diff(_))) {
                if !m.step_file(forward) {
                    m.info(if forward { "last file" } else { "first file" });
                }
                return Some(Vec::new());
            }
            match crate::review::file(lines, at, forward) {
                Some(to) => {
                    m.diff_view.cursor = to;
                    m.diff_view.anchor = None;
                    m.diff_view.reveal = true;
                }
                None => m.info(if forward { "last file" } else { "first file" }),
            }
            return Some(Vec::new());
        }
        KeyCode::Char('e') => return edit_line(m),
        _ => return None,
    };
    if let Some(to) = to {
        m.diff_view.cursor = to;
        m.diff_view.reveal = true;
    }
    Some(Vec::new())
}

/// Keys for [0] while it reads a subagent's log: the entry cursor, folds and
/// the transcript in the editor. `None` leaves the key to the plain
/// document keys.
fn log_key(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    let key = m.subagent_selected()?;
    let Some(Ok(Some(detail))) = m.subagent_logs.get(&key) else {
        return None;
    };
    let entries = crate::log::entries(detail);
    let last = entries.len().checked_sub(1)?;
    let running = !detail.finished;
    let nav = m.log_view.navs.entry(key.clone()).or_default();
    let at = nav
        .cursor
        .unwrap_or(if running { last } else { 0 })
        .min(last);
    let mut effects = Vec::new();
    match code {
        KeyCode::Char('j') | KeyCode::Down => {
            nav.cursor = Some((at + 1).min(last));
            m.log_view.reveal = true;
            if running && at + 1 >= last {
                m.docs.scroll_to_end();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            nav.cursor = Some(at.saturating_sub(1));
            m.log_view.reveal = true;
            m.docs.pause();
        }
        KeyCode::Char('g') | KeyCode::Home => {
            nav.cursor = Some(0);
            m.docs.scroll_to_start();
        }
        KeyCode::Char('G') | KeyCode::End => {
            nav.cursor = Some(last);
            m.docs.scroll_to_end();
        }
        KeyCode::PageDown | KeyCode::PageUp => {
            let half = m.docs.half_page() * 2;
            m.docs.scroll_by(if code == KeyCode::PageDown {
                half
            } else {
                -half
            });
            m.log_view.follow_page = true;
        }
        KeyCode::Enter => {
            nav.cursor = Some(at);
            nav.toggle(at);
        }
        KeyCode::Char('=') => nav.all = true,
        KeyCode::Char('-') => {
            nav.all = false;
            nav.open.clear();
        }
        KeyCode::Char('y') => {
            let text = crate::log::entry_text(entries.get(at)?);
            nav.cursor = Some(at);
            return Some(copy(m, text));
        }
        KeyCode::Char('e') => {
            let root = m.selected_entry()?.path()?.to_path_buf();
            let title = m
                .subagent_rows()
                .into_iter()
                .find(|(id, _)| **id == key.1)
                .and_then(|(_, s)| s.description.clone())
                .unwrap_or_else(|| "subagent".into());
            let (text, starts) = crate::log::markdown(detail, &title);
            let line = starts.get(at).copied().unwrap_or(NonZeroUsize::MIN);
            effects.push(Effect::Edit(Open::Transcript {
                root,
                name: format!("{}-{}", key.0.as_str(), key.1.as_str()),
                text,
                line,
            }));
        }
        _ => return None,
    }
    Some(effects)
}

/// Control characters other than newlines and tabs could end a bracketed
/// paste early and run the rest as keystrokes.
fn sanitize_paste(text: &str) -> String {
    clean(text, CarriageReturn::Keep, None)
}

/// Keys that choose the panel `j`/`k` act on, and those a panel gives its
/// own meaning. `None` leaves the key to NAV.
fn on_panel_key(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    if m.main_focused()
        && code == KeyCode::Char('y')
        && let Some(crate::document::Hover::Turn(name)) = m.hover()
    {
        let message = m
            .selected_entry()
            .and_then(|e| crate::view::turn_message(m, e));
        let Some(text) = message else {
            m.info(format!("{name} has no message to copy"));
            return Some(Vec::new());
        };
        return Some(copy(m, text));
    }
    if m.main_focused()
        && code == KeyCode::Esc
        && m.diff_view.anchor.is_some()
        && matches!(
            m.hover(),
            Some(
                crate::document::Hover::Diff(_)
                    | crate::document::Hover::Folder(_)
                    | crate::document::Hover::Commit(_)
            )
        )
    {
        m.diff_view.anchor = None;
        return Some(Vec::new());
    }
    if m.main_focused() && code == KeyCode::Esc && m.opened.take().is_some() {
        m.diff_view.owner = None;
        return Some(Vec::new());
    }
    if m.main_focused()
        && let Some(effects) = log_key(m, code)
            .or_else(|| diff_key(m, code))
            .or_else(|| branch_key(m, code))
    {
        return Some(effects);
    }
    if m.main_focused() {
        match code {
            KeyCode::Esc | KeyCode::Char('1' | '2' | '3') => m.panel = m.panel.listed(),
            KeyCode::Char('j') | KeyCode::Down => m.docs.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => m.docs.scroll_by(-1),
            KeyCode::Char('g') | KeyCode::Home => m.docs.scroll_to_start(),
            KeyCode::Char('G') | KeyCode::End => m.docs.scroll_to_end(),
            KeyCode::PageDown => m.docs.scroll_by(m.docs.half_page() * 2),
            KeyCode::PageUp => m.docs.scroll_by(-m.docs.half_page() * 2),
            KeyCode::Enter | KeyCode::Char('0') => {}
            _ => return None,
        }
        if !matches!(code, KeyCode::Char('1' | '2' | '3')) {
            return Some(Vec::new());
        }
    }
    match (code, m.panel.listed()) {
        (KeyCode::Esc | KeyCode::Char('1'), Panel::Agent { .. } | Panel::Changes { .. }) => {
            m.panel = Panel::Agents;
            Some(Vec::new())
        }
        (KeyCode::Char('2'), _) if m.selected_entry().is_none() => {
            m.info("select an agent to see it");
            Some(Vec::new())
        }
        (KeyCode::Char('2'), _)
            if m.selected_entry()
                .is_some_and(|e| m.activity(e.name()).is_none()) =>
        {
            m.info("no turn recorded · d then ⏎ restarts it through agent");
            Some(Vec::new())
        }
        (KeyCode::Char('2'), _) => {
            m.panel = Panel::Agent { offset: 0 };
            m.subagent = None;
            Some(Vec::new())
        }
        (KeyCode::Char('3'), _) if m.change_rows() == 0 => {
            m.info("no files to move to");
            Some(Vec::new())
        }
        (KeyCode::Char('3'), _) if m.work_room == WorkRoom::TooShort => {
            m.info("no room for [3] at this height");
            Some(Vec::new())
        }
        (KeyCode::Char('3'), _) => {
            m.panel = Panel::Changes { file: 0 };
            Some(Vec::new())
        }
        (KeyCode::Enter, Panel::Changes { .. }) if m.folder_under_cursor().is_some() => {
            m.toggle_folder();
            Some(Vec::new())
        }
        (KeyCode::Char('`'), Panel::Changes { .. }) => {
            m.toggle_changes_view();
            Some(Vec::new())
        }
        (KeyCode::Char('-' | '='), Panel::Changes { .. })
            if m.changes_view == ChangesView::Flat =>
        {
            m.info("the flat list has no folders · ` shows the tree");
            Some(Vec::new())
        }
        (KeyCode::Char(c @ ('-' | '=')), Panel::Changes { .. }) => {
            m.fold_all(c == '-');
            Some(Vec::new())
        }
        (KeyCode::Char('e'), Panel::Changes { .. }) if m.folder_under_cursor().is_some() => {
            let root = m
                .selected_entry()
                .and_then(|e| e.path())
                .map(Path::to_path_buf);
            let folder = m.folder_under_cursor().map(|r| r.path);
            match root.zip(folder).map(|(root, folder)| root.join(folder)) {
                Some(dir) if dir.is_dir() => Some(vec![Effect::Edit(Open::Checkout(dir))]),
                _ => {
                    m.info("that folder is gone from the checkout");
                    Some(Vec::new())
                }
            }
        }
        (KeyCode::Enter | KeyCode::Char('0'), Panel::Changes { .. } | Panel::Agent { .. })
            if m.hover().is_some() =>
        {
            m.panel = m.panel.into_main();
            m.opened = None;
            Some(Vec::new())
        }
        (KeyCode::Char('0'), Panel::Agents) => m
            .selected_entry()
            .map(|e| e.name().clone())
            .map(|name| activate(m, &name)),
        (KeyCode::Char('e'), Panel::Changes { .. }) => m
            .file_under_cursor()
            .map(|(root, file)| vec![Effect::Edit(Open::File { root, file })]),
        (KeyCode::Char('e'), Panel::Agent { .. }) => edit_transcript(m),
        _ => None,
    }
}

fn cycle_tab(m: &mut Model, direction: Direction) {
    if m.panel == Panel::Agents {
        m.cycle_view(direction);
    } else if matches!(m.panel, Panel::Changes { .. }) {
        m.toggle_changes_scope();
    } else {
        m.info("this panel has no tabs");
    }
}

fn panel_focusable(m: &Model, number: usize) -> bool {
    match number {
        1 => true,
        2 => m
            .selected_entry()
            .is_some_and(|e| m.activity(e.name()).is_some()),
        _ => m.change_rows() > 0 && m.work_room != WorkRoom::TooShort,
    }
}

fn cycle_panel(m: &mut Model, direction: Direction) -> Vec<Effect> {
    let current = match m.panel.listed() {
        Panel::Agent { .. } => 2,
        Panel::Changes { .. } => 3,
        _ => 1,
    };
    let step = match direction {
        Direction::Forward => 1,
        Direction::Back => 2,
    };
    let target = (1..=3)
        .map(|k| (current - 1 + k * step) % 3 + 1)
        .find(|n| panel_focusable(m, *n))
        .unwrap_or(1);
    let digit = match target {
        2 => '2',
        3 => '3',
        _ => '1',
    };
    on_panel_key(m, KeyCode::Char(digit)).unwrap_or_default()
}

fn on_nav_key(m: &mut Model, key: KeyEvent) -> Vec<Effect> {
    let armed = std::mem::replace(&mut m.chord, Chord::Idle) == Chord::Armed;
    if pressed(&key, m.keys.nav()) {
        if armed {
            let current = m.pane.session().cloned();
            let previous = m
                .mru()
                .iter()
                .find(|n| Some(*n) != current.as_ref())
                .cloned();
            if let Some(name) = previous {
                return activate(m, &name);
            }
        }
        return enter_agent(m);
    }
    let selected = m.selected_entry().cloned();
    if let Some(effects) = on_panel_key(m, key.code) {
        return effects;
    }
    match key.code {
        KeyCode::Char('q') => {
            let queued: usize = m
                .outboxes
                .values()
                .map(crate::model::Outbox::len)
                .sum::<usize>()
                + m.drafts.values().map(Vec::len).sum::<usize>();
            if queued == 0 {
                return vec![Effect::Quit];
            }
            m.overlay = Overlay::Confirm(Confirm::Quit(queued));
            return Vec::new();
        }
        KeyCode::Esc => return enter_agent(m),
        KeyCode::Char('i') => {
            if let Some(entry) = selected {
                return activate(m, entry.name());
            }
        }
        KeyCode::Enter => match (m.cursor().cloned(), selected) {
            (Some(Cursor::Repo(key)), _) => {
                m.view = View::Focus;
                m.select_repo(&key);
            }
            (_, Some(entry)) => return activate(m, entry.name()),
            _ => {}
        },
        KeyCode::Char('d') if ctrl(&key, 'd') => match m.panel {
            Panel::Changes { .. } | Panel::Agent { .. } | Panel::Main(_) => {
                m.docs.scroll_by(m.docs.half_page());
                m.log_view.follow_page = m.main_focused();
                m.diff_view.follow_page = m.main_focused();
            }
            Panel::Agents => m.page_in_panel(m.half_page()),
        },
        KeyCode::Char('u') if ctrl(&key, 'u') => match m.panel {
            Panel::Changes { .. } | Panel::Agent { .. } | Panel::Main(_) => {
                m.docs.scroll_by(-m.docs.half_page());
                m.log_view.follow_page = m.main_focused();
                m.diff_view.follow_page = m.main_focused();
            }
            Panel::Agents => m.page_in_panel(-m.half_page()),
        },
        KeyCode::Char('J') if m.hover().is_some() => m.docs.scroll_by(SCROLL),
        KeyCode::Char('K') if m.hover().is_some() => m.docs.scroll_by(-SCROLL),
        KeyCode::PageDown => m.page_in_panel(m.half_page() * 2),
        KeyCode::PageUp => m.page_in_panel(-m.half_page() * 2),
        KeyCode::Char('j') | KeyCode::Down => m.move_in_panel(1),
        KeyCode::Char('k') | KeyCode::Up => m.move_in_panel(-1),
        KeyCode::Char('g') | KeyCode::Home => m.panel_to_edge(Edge::First),
        KeyCode::Char('G') | KeyCode::End => m.panel_to_edge(Edge::Last),
        KeyCode::Char('}') => m.jump_repo(1),
        KeyCode::Char('{') => m.jump_repo(-1),
        KeyCode::Char(']') => cycle_tab(m, Direction::Forward),
        KeyCode::Char('[') => cycle_tab(m, Direction::Back),
        KeyCode::Tab | KeyCode::Char('l') => return cycle_panel(m, Direction::Forward),
        KeyCode::BackTab | KeyCode::Char('h') => return cycle_panel(m, Direction::Back),
        KeyCode::Char('+') => m.screen = Screen::Full,
        KeyCode::Char('_') => m.screen = Screen::Split,
        KeyCode::Char('.') if m.view == View::Pinned => {
            m.info("pinned lists every pin · . filters the repo and agents tabs");
        }
        KeyCode::Char('.') => m.visibility = m.visibility.toggled(),
        KeyCode::Char('p') => m.overlay = Overlay::Picker(Query::default()),
        KeyCode::Char(' ' | '/') => m.overlay = Overlay::Switcher(Query::default()),
        KeyCode::Char('?') => m.overlay = Overlay::Help(HelpState::default()),
        KeyCode::Char('m') => {
            if let Some(entry) = selected {
                let name = entry.name().clone();
                return vec![Effect::Job(match entry.pin() {
                    Some(_) => Job::Unpin(name),
                    None => Job::Pin(name),
                })];
            }
        }
        KeyCode::Char('n') => open_form(m, FormKind::New),
        KeyCode::Char('c') if !m.main_focused() => open_form(m, FormKind::Checkout),
        KeyCode::Char('s') => {
            if let Some(entry) = selected {
                open_talk(m, entry.name());
            }
        }
        KeyCode::Char('t') => m.info(SEND_MOVED),
        KeyCode::Char('v') => open_peek(m),
        KeyCode::Char('e') => {
            let path = m.selected().and_then(|(repo, e)| {
                e.and_then(|e| e.path())
                    .or_else(|| repo.key().root().map(agent_core::paths::RepoRoot::path))
                    .map(PathBuf::from)
            });
            if let Some(path) = path {
                return vec![Effect::Edit(Open::Checkout(path))];
            }
        }
        KeyCode::Char('d') => match selected.map(|e| (m.reach(&e), e)) {
            Some((
                Reach::Other {
                    kill: Some(running),
                    ..
                },
                _,
            )) => m.overlay = Overlay::Confirm(Confirm::Kill(running)),
            Some((Reach::Own, entry)) => {
                m.error(format!("can't kill {}: this TUI runs in it", entry.name()));
            }
            Some((
                Reach::Other {
                    enter: Enter::Resurrect | Enter::Restart(_),
                    ..
                },
                entry,
            )) => {
                m.info(format!("{} isn't running. D removes it", entry.name()));
            }
            Some((Reach::Other { .. }, _)) | None => m.info("no session to kill"),
        },
        KeyCode::Char('D') => return request_removal(m),
        KeyCode::Char('a') => {
            let here = m.visible_session().cloned();
            let next = m
                .needs_you()
                .into_iter()
                .find(|name| Some(name) != here.as_ref());
            match next {
                Some(name) => return activate(m, &name),
                None => m.info("Nothing needs you"),
            }
        }
        KeyCode::Char('x') => match selected {
            _ if m.actions.is_empty() => m.info(format!(
                "no actions · add [[action]] to {}",
                crate::CONFIG_HINT
            )),
            Some(entry) => m.overlay = Overlay::Actions(entry.name().clone()),
            None => m.info("select an agent to run an action on"),
        },
        _ => {}
    }
    Vec::new()
}

/// Runs the action bound to `key` on `target`, refusing if a placeholder
/// it uses has nothing to fill it.
fn run_action(m: &mut Model, target: &SessionName, action: &Action) -> Vec<Effect> {
    let Some((repo, entry)) = m.snapshot().and_then(|s| s.entry(target)) else {
        m.error(format!("{target} is gone"));
        return Vec::new();
    };
    let ctx = Context {
        name: entry.name(),
        path: entry.path().filter(|p| p.is_dir()),
        repo: repo.key().root().map(agent_core::paths::RepoRoot::path),
    };
    match action.invoke(ctx) {
        Ok(Invocation::Run(launch)) => vec![Effect::Job(Job::Run {
            label: format!("{} on {target}", action.label),
            launch,
        })],
        Ok(Invocation::Interactive(launch)) => vec![Effect::Interactive {
            label: action.label.to_string(),
            launch,
        }],
        Ok(Invocation::TerminalEvent { name, value }) => {
            m.info(format!("sent {} for {target}", action.label));
            vec![Effect::TerminalEvent { name, value }]
        }
        Err(missing) => {
            m.error(format!("{}: {target} has no {}", action.label, missing.0));
            Vec::new()
        }
    }
}

fn open_form(m: &mut Model, kind: FormKind) {
    let default_repo = m
        .selected()
        .and_then(|(repo, _)| repo.key().root().cloned())
        .or_else(|| {
            m.snapshot()
                .and_then(|s| s.containing(&m.start_dir))
                .and_then(|(r, _)| r.key().root().cloned())
        });
    m.overlay = Overlay::New(Box::new(NewAgent::new(kind, default_repo)));
}

/// Plans removing the selected agent; the confirmation opens when the plan
/// arrives.
fn request_removal(m: &mut Model) -> Vec<Effect> {
    let Some(entry) = m.selected_entry().cloned() else {
        return Vec::new();
    };
    match m.reach(&entry) {
        Reach::Own => m.error(format!(
            "can't remove {}: this TUI runs in it",
            entry.name()
        )),
        Reach::Other {
            remove: Some(target),
            ..
        } => {
            let ticket = m.ticket();
            m.overlay = Overlay::Planning {
                ticket,
                name: entry.name().clone(),
            };
            return vec![Effect::Job(Job::PlanRemoval {
                target,
                ticket,
                own: m.own_session.clone(),
            })];
        }
        Reach::Other { remove: None, .. } => {
            m.info(format!("{} has nothing to remove", entry.name()));
        }
    }
    Vec::new()
}

/// Opens the message box for `name`, or says why it can't be messaged.
fn open_talk(m: &mut Model, name: &SessionName) {
    let Some(entry) = m.snapshot().and_then(|s| s.entry(name)).map(|(_, e)| e) else {
        return;
    };
    match m.talkable(entry) {
        Ok(to) => {
            m.overlay = Overlay::Talk {
                to: to.clone(),
                text: String::new(),
            }
        }
        Err(Unreachable::NotRunning) => m.info(format!("{name} isn't running · ⏎ starts it")),
        Err(Unreachable::Own) => m.info(format!("{name} is where this TUI runs")),
        Err(Unreachable::Unreported) => m.info(format!(
            "{name} reports no activity, so its turn's end can't be known · ⏎ attaches"
        )),
    }
}

/// Opens the peek float on the selected live agent.
fn open_peek(m: &mut Model) {
    let Some(entry) = m.selected_entry() else {
        m.info("select an agent to peek at");
        return;
    };
    let name = entry.name().clone();
    match m.reach(entry) {
        Reach::Other {
            enter: Enter::Attach,
            ..
        } => {
            if m.pane.session() != Some(&name) {
                m.schedule_preview();
            }
            m.peek = Some(name);
        }
        Reach::Own => m.info(format!("{name} is where this TUI runs")),
        Reach::Other { .. } => m.info(format!("{name} isn't running · ⏎ starts it")),
    }
}

/// Queues a message for its agent and sends the first one that can go.
fn talk(m: &mut Model, to: SessionName, text: String) -> Vec<Effect> {
    let draft = m.drafts.get(&to).cloned().unwrap_or_default();
    let message = if draft.is_empty() {
        text.clone()
    } else {
        crate::review::review_message(&text, &draft)
    };
    let prompt = match message.parse::<Prompt>() {
        Ok(prompt) => prompt,
        Err(e) => {
            m.info(match e {
                InvalidPrompt::Empty => "type a message first · Esc closes".to_string(),
                InvalidPrompt::Control(c) => {
                    format!("the message has a control character ({c:?}) · Esc closes")
                }
            });
            m.overlay = Overlay::Talk { to, text };
            return Vec::new();
        }
    };
    m.drafts.remove(&to);
    m.outboxes
        .entry(to.clone())
        .or_default()
        .push(prompt, draft);
    let effects = flush(m);
    if m.outboxes.get(&to).is_some_and(|o| o.in_flight) {
        m.info(format!("sent to {to}"));
    } else if m.awaiting_report(&to) {
        m.info(format!("queued · waiting for {to} to report"));
    } else {
        m.info(format!("queued for {to} · sends when it's at rest"));
    }
    effects
}

/// Puts comments that weren't delivered back in `to`'s draft, ahead of any
/// drafted since, and returns how many.
fn restore_comments(
    m: &mut Model,
    to: &SessionName,
    mut comments: Vec<crate::review::Comment>,
) -> usize {
    let n = comments.len();
    if n > 0 {
        let draft = m.drafts.entry(to.clone()).or_default();
        comments.append(draft);
        *draft = comments;
    }
    n
}

/// Starts typing the first queued message into every agent that is live,
/// ready for text (at rest, reported since its session started, past its
/// boot), not being typed into by the user, and has reported since the last
/// one went in. The worker checks the store again just before typing.
fn flush(m: &mut Model) -> Vec<Effect> {
    let ready: Vec<SessionName> = m
        .outboxes
        .iter()
        .filter(|(_, o)| !o.in_flight && !o.is_empty())
        .filter(|(_, o)| o.hold.is_none_or(|h| h.at.elapsed() > HOLD_FOR))
        .filter(|(name, o)| {
            m.ready_for_text(name)
                && !m.typing_into(name)
                && o.sent_at
                    .is_none_or(|at| m.activity(name).is_some_and(|a| a.events != at))
                && m.snapshot()
                    .and_then(|s| s.entry(name))
                    .is_some_and(|(_, e)| m.talkable(e).is_ok())
        })
        .map(|(name, _)| name.clone())
        .collect();
    let mut effects = Vec::new();
    for name in ready {
        let not_before = m.not_before(&name);
        if let Some(outbox) = m.outboxes.get_mut(&name)
            && let Some(prompt) = outbox.front().cloned()
        {
            outbox.in_flight = true;
            outbox.hold = None;
            effects.push(Effect::Job(Job::Reply {
                to: name,
                prompt,
                not_before,
            }));
        }
    }
    effects
}

/// A typed message was delivered, or wasn't: the first of the queue is
/// done if it was, kept for fresh activity if the agent wasn't ready or its
/// record couldn't be read, and dropped with the reason otherwise.
fn on_sent(m: &mut Model, to: &SessionName, result: Result<(), SendError>) -> Vec<Effect> {
    let events = m.activity(to).map(|a| a.events);
    let Some(outbox) = m.outboxes.get_mut(to) else {
        return Vec::new();
    };
    outbox.in_flight = false;
    match result {
        Ok(()) => {
            outbox.pop();
            outbox.sent_at = events;
            outbox.sent_instant = Some(Instant::now());
            let left = outbox.len();
            if left == 0 {
                m.outboxes.remove(to);
                m.info(format!("sent to {to}"));
            } else {
                m.info(format!("sent to {to} · {left} queued"));
            }
        }
        Err(SendError::Busy) => {
            outbox.hold = Some(Hold {
                events: Some(events.unwrap_or_default()),
                at: Instant::now(),
            });
        }
        Err(SendError::Unreadable(e)) => {
            outbox.hold = Some(Hold {
                events: None,
                at: Instant::now(),
            });
            m.error(format!("couldn't read {to}'s activity: {e} · will retry"));
        }
        Err(SendError::Failed(e)) => {
            let dropped = outbox.pop();
            if outbox.is_empty() {
                m.outboxes.remove(to);
            }
            let (words, comments) = dropped.map_or_else(Default::default, |q| {
                (first_words(q.prompt.as_ref()), q.comments)
            });
            let back = match restore_comments(m, to, comments) {
                0 => String::new(),
                1 => " · its comment is back in the draft".to_string(),
                n => format!(" · its {n} comments are back in the draft"),
            };
            m.error(format!(
                "couldn't send to {to}: {e} · dropped \"{words}\"{back}"
            ));
        }
    }
    flush(m)
}

/// Lifts the holds a fresh read of the store answers.
fn release_holds(m: &mut Model) {
    let events: HashMap<SessionName, u64> = m
        .activities
        .iter()
        .flatten()
        .map(|(name, a)| (name.clone(), a.events))
        .collect();
    for (name, outbox) in &mut m.outboxes {
        if let Some(hold) = outbox.hold
            && hold
                .events
                .is_none_or(|held| events.get(name).copied().unwrap_or_default() != held)
        {
            outbox.hold = None;
        }
    }
}

/// Notes which sessions are live now: one that wasn't before gets the
/// moment it appeared, which a queued message needs a report to postdate.
fn track_live(m: &mut Model) {
    let Some(snapshot) = m.snapshot() else {
        return;
    };
    let live: HashSet<SessionName> = snapshot
        .entries()
        .filter(|(_, e)| matches!(e.state(), EntryState::Live))
        .map(|(_, e)| e.name().clone())
        .collect();
    let first = m.snapshots <= 1;
    m.live_since.retain(|name, _| live.contains(name));
    for name in live {
        m.live_since
            .entry(name)
            .or_insert_with(|| (!first).then(SystemTime::now));
    }
}

/// Drops what's queued for `to`, unless its first message is already being
/// typed.
fn clear_queue(m: &mut Model, to: &SessionName) {
    if m.outboxes.get(to).is_some_and(|o| o.in_flight) {
        m.info(format!("the first message to {to} is being typed"));
    } else if let Some(cleared) = m.outboxes.remove(to) {
        let queued = cleared.len();
        let back = match restore_comments(m, to, cleared.into_comments()) {
            0 => String::new(),
            n => format!(
                " · {n} drafted comment{} kept",
                if n == 1 { "" } else { "s" }
            ),
        };
        m.info(format!("cleared {queued} queued for {to}{back}"));
    }
}

/// The start of a message, for a line with little room.
pub fn first_words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Shared query editing for the switcher and picker. Other keys are ignored.
fn edit_query(query: &mut Query, key: &KeyEvent, len: usize, nav: ControlKey) {
    match key.code {
        KeyCode::Down => query.step(1, len),
        KeyCode::Up => query.step(-1, len),
        _ if ctrl(key, 'n') || pressed(key, nav) => query.step(1, len),
        _ if ctrl(key, 'p') => query.step(-1, len),
        KeyCode::Backspace => query.pop(),
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => query.push(c),
        _ => {}
    }
}

pub fn switcher_matches(m: &Model, query: &str) -> Vec<SessionName> {
    m.switcher_items()
        .into_iter()
        .filter(|(repo, e)| fuzzy_match(query, &format!("{} {}", e.name(), repo.name())).is_some())
        .map(|(_, e)| e.name().clone())
        .collect()
}

/// A row of the repo picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// A repo the sidebar shows: ⏎ focuses it.
    Listed(RepoKey),
    /// A catalogue repo the sidebar doesn't show: ⏎ lists it.
    Found(RepoRoot),
    /// A typed path: ⏎ lists the repo holding it. Whether it is one is
    /// checked off the UI thread, so a slow mount can't stall typing.
    Path(PathBuf),
}

impl Pick {
    /// What ⏎ does with it, for the hint.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Listed(_) => "focus",
            Self::Found(_) | Self::Path(_) => "add",
        }
    }
}

/// The picker's rows for `query`, or why there are none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picks {
    Rows(Vec<Pick>),
    /// Nothing matches the name.
    NoMatch,
    /// A path that can't be used.
    Unusable(Unusable),
}

/// Why a typed path can't be a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unusable {
    /// `~user/...`.
    UserHome,
    /// `~` with no home directory set.
    NoHome,
}

impl Picks {
    pub fn rows(&self) -> &[Pick] {
        match self {
            Self::Rows(rows) => rows,
            Self::NoMatch | Self::Unusable(_) => &[],
        }
    }
}

/// A typed path is the one row. Otherwise the listed repos matching
/// `query`, then the catalogue's, each group with exact matches first,
/// then prefixes.
pub fn picker_matches(m: &Model, query: &str) -> Picks {
    match typed_path(query, &m.start_dir) {
        Some(Typed::Path(path)) => return Picks::Rows(vec![Pick::Path(path)]),
        Some(Typed::Unsupported) => return Picks::Unusable(Unusable::UserHome),
        Some(Typed::NoHome) => return Picks::Unusable(Unusable::NoHome),
        None => {}
    }
    let query_lower = query.to_lowercase();
    let mut listed: Vec<(u8, Pick)> = m
        .snapshot()
        .map(|s| {
            s.repos()
                .iter()
                .filter(|r| r.key().root().is_some())
                .filter(|r| fuzzy_match(query, &r.name()).is_some())
                .map(|r| {
                    (
                        match_rank(&r.name(), &query_lower),
                        Pick::Listed(r.key().clone()),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    listed.sort_by_key(|(rank, _)| *rank);
    let mut found: Vec<(u8, Pick)> = unlisted(m.snapshot(), &m.catalogue)
        .filter(|root| fuzzy_match(query, &root.name()).is_some())
        .map(|root| {
            (
                match_rank(&root.name(), &query_lower),
                Pick::Found(root.clone()),
            )
        })
        .collect();
    found.sort_by_key(|(rank, _)| *rank);
    let rows: Vec<Pick> = listed
        .into_iter()
        .chain(found)
        .map(|(_, pick)| pick)
        .collect();
    if rows.is_empty() {
        Picks::NoMatch
    } else {
        Picks::Rows(rows)
    }
}

/// Whether the list stays open.
fn on_help_key(m: &Model, help: &mut HelpState, key: &KeyEvent) -> bool {
    let rows =
        crate::keymap::help(&Situation::of(m), &m.keys.nav().to_string(), &help.filter).len();
    let last = rows.saturating_sub(1);
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let step = |help: &mut HelpState, by: isize| {
        help.selected = help.selected.saturating_add_signed(by).min(last);
    };
    match key.code {
        KeyCode::Esc | KeyCode::Char('?' | 'q') if !help.typing => return false,
        KeyCode::Enter => help.typing = false,
        KeyCode::Esc => {
            help.filter.clear();
            help.typing = false;
            help.selected = 0;
        }
        KeyCode::Down | KeyCode::Tab => step(help, 1),
        KeyCode::Up | KeyCode::BackTab => step(help, -1),
        KeyCode::Char('n') if control => step(help, 1),
        KeyCode::Char('p') if control => step(help, -1),
        KeyCode::PageDown => step(help, 8),
        KeyCode::PageUp => step(help, -8),
        KeyCode::Home => help.selected = 0,
        KeyCode::End => help.selected = last,
        KeyCode::Char('j') if !help.typing => step(help, 1),
        KeyCode::Char('k') if !help.typing => step(help, -1),
        KeyCode::Char('/') if !help.typing => help.typing = true,
        KeyCode::Backspace => {
            help.filter.pop();
            help.selected = 0;
        }
        KeyCode::Char(c) if !control => {
            help.typing = true;
            help.filter.push(c);
            help.selected = 0;
        }
        _ => {}
    }
    true
}

fn on_overlay_key(m: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match std::mem::replace(&mut m.overlay, Overlay::None) {
        Overlay::None => Vec::new(),
        Overlay::Help(mut help) => {
            if on_help_key(m, &mut help, &key) {
                m.overlay = Overlay::Help(help);
            }
            Vec::new()
        }
        Overlay::New(form) => {
            m.overlay = Overlay::New(form);
            on_new_key(m, &key)
        }
        Overlay::Talk { to, mut text } => match key.code {
            KeyCode::Esc => Vec::new(),
            KeyCode::Enter => talk(m, to, text),
            _ if ctrl(&key, 'x') => {
                clear_queue(m, &to);
                m.overlay = Overlay::Talk { to, text };
                Vec::new()
            }
            _ => {
                match key.code {
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    _ if ctrl(&key, 'u') => text.clear(),
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        text.push(c);
                    }
                    _ => {}
                }
                m.overlay = Overlay::Talk { to, text };
                Vec::new()
            }
        },
        Overlay::Comment {
            to,
            lines,
            mut text,
            rev,
        } => match key.code {
            KeyCode::Esc => Vec::new(),
            KeyCode::Enter => {
                add_comment(m, &to, &lines, &text, rev);
                Vec::new()
            }
            _ => {
                match key.code {
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    _ if ctrl(&key, 'u') => text.clear(),
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        text.push(c);
                    }
                    _ => {}
                }
                m.overlay = Overlay::Comment {
                    to,
                    lines,
                    text,
                    rev,
                };
                Vec::new()
            }
        },
        Overlay::Switcher(mut query) => {
            let matches = switcher_matches(m, &query.text);
            match key.code {
                KeyCode::Esc | KeyCode::Tab => Vec::new(),
                KeyCode::Enter => {
                    if let Some(name) = query.pick(&matches).cloned() {
                        activate(m, &name)
                    } else {
                        m.overlay = Overlay::Switcher(query);
                        Vec::new()
                    }
                }
                _ => {
                    edit_query(&mut query, &key, matches.len(), m.keys.nav());
                    m.overlay = Overlay::Switcher(query);
                    Vec::new()
                }
            }
        }
        Overlay::Picker(mut query) => {
            let matches = picker_matches(m, &query.text);
            let rows = matches.rows();
            match key.code {
                KeyCode::Esc | KeyCode::Tab => Vec::new(),
                KeyCode::Enter => match query.pick(rows).cloned() {
                    Some(Pick::Listed(key)) => {
                        m.select_repo(&key);
                        Vec::new()
                    }
                    Some(Pick::Found(root)) => {
                        vec![Effect::Job(Job::ListRepo(root.path().to_path_buf()))]
                    }
                    Some(Pick::Path(path)) => vec![Effect::Job(Job::ListRepo(path))],
                    None => {
                        m.overlay = Overlay::Picker(query);
                        Vec::new()
                    }
                },
                _ => {
                    edit_query(&mut query, &key, rows.len(), m.keys.nav());
                    m.overlay = Overlay::Picker(query);
                    Vec::new()
                }
            }
        }
        Overlay::Confirm(confirm) => on_confirm_key(m, confirm, &key),
        Overlay::Planning { ticket, name } => {
            if !matches!(key.code, KeyCode::Esc | KeyCode::Char('n' | 'q')) {
                m.overlay = Overlay::Planning { ticket, name };
            }
            Vec::new()
        }
        Overlay::Actions(target) => {
            let action = match key.code {
                KeyCode::Char(c) if (key.modifiers - KeyModifiers::SHIFT).is_empty() => {
                    m.actions.get(c).cloned()
                }
                _ => None,
            };
            match (key.code, action) {
                (KeyCode::Esc, _) => Vec::new(),
                (_, Some(action)) => run_action(m, &target, &action),
                (_, None) => {
                    m.overlay = Overlay::Actions(target);
                    Vec::new()
                }
            }
        }
    }
}

/// Types `c` into the focused field of the new-agent form, dropping what
/// the field can't hold.
fn type_into(form: &mut NewAgent, c: char) {
    match form.focus {
        Field::Task if !c.is_control() || c == '\n' || c == '\t' => form.task.push(c),
        Field::Name if c.is_whitespace() => form.name.push('-'),
        Field::Name if !c.is_control() => form.name.push(c),
        Field::Repo if !c.is_control() => form.repo.push(c),
        _ => {}
    }
}

/// Deletes the last word of `text`, and the space before it.
fn delete_word(text: &mut String) {
    let trimmed = text.trim_end_matches([' ', '\n', '-', '/']).len();
    let start = text[..trimmed]
        .rfind([' ', '\n', '-', '/'])
        .map_or(0, |i| i + 1);
    text.truncate(start);
}

/// Applies `edit` to the open form (unless it's creating), then refreshes
/// its checks.
fn edit_form(m: &mut Model, edit: impl FnOnce(&mut NewAgent)) {
    if let Overlay::New(form) = &mut m.overlay {
        if let Phase::Submitting { .. } = form.phase {
            return;
        }
        let before = form.task.clone();
        edit(form);
        form.error = None;
        if form.task != before {
            form.prompt_changed();
        }
    }
}

fn on_new_key(m: &mut Model, key: &KeyEvent) -> Vec<Effect> {
    let Overlay::New(form) = &m.overlay else {
        return Vec::new();
    };
    if let Phase::Submitting { ticket, launch } = &form.phase {
        // Esc backgrounds it: the form closes, the pane stops waiting, and
        // the agent previews once a refresh shows it.
        if key.code == KeyCode::Esc {
            let (ticket, name) = (*ticket, launch.session());
            m.pane.abandon(ticket);
            m.follow = Some(Follow::Backgrounded { name, ticket });
            m.overlay = Overlay::None;
        }
        return Vec::new();
    }
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let accept = m
        .derive(form)
        .completion
        .filter(|_| form.focus == Field::Repo && matches!(key.code, KeyCode::Tab | KeyCode::Right));
    if let Some(rest) = accept {
        edit_form(m, |f| rest.chars().for_each(|c| f.repo.push(c)));
        return Vec::new();
    }
    let repos = repo_matches(m.snapshot(), &m.catalogue, &form.repo.text).len();
    match key.code {
        KeyCode::Esc => m.overlay = Overlay::None,
        KeyCode::Enter if !alt => return submit(m),
        _ if ctrl(key, 'e') && form.kind == FormKind::New => {
            let Overlay::New(form) = &mut m.overlay else {
                return Vec::new();
            };
            form.focus = Field::Task;
            return vec![Effect::EditTask(form.task.clone())];
        }
        _ => edit_form(m, |form| match key.code {
            KeyCode::Enter if form.focus == Field::Task && form.kind == FormKind::New => {
                form.task.push('\n');
            }
            _ if ctrl(key, 'j') && form.focus == Field::Task && form.kind == FormKind::New => {
                form.task.push('\n');
            }
            _ if ctrl(key, 'r') => form.name.clear(),
            _ if ctrl(key, 'u') => match form.focus {
                Field::Task => form.task.clear(),
                Field::Name => form.name.clear(),
                Field::Repo => form.repo = Query::default(),
            },
            _ if ctrl(key, 'w') => match form.focus {
                Field::Task => delete_word(&mut form.task),
                Field::Name => delete_word(&mut form.name),
                Field::Repo => {
                    let mut text = form.repo.text.clone();
                    delete_word(&mut text);
                    form.repo.set_text(text);
                }
            },
            _ if form.focus == Field::Repo && ctrl(key, 'n') => form.repo.step(1, repos),
            _ if form.focus == Field::Repo && ctrl(key, 'p') => form.repo.step(-1, repos),
            KeyCode::Tab => form.focus = form.focus.next(),
            KeyCode::BackTab => form.focus = form.focus.previous(),
            KeyCode::Backspace => match form.focus {
                Field::Task => {
                    form.task.pop();
                }
                Field::Name => {
                    form.name.pop();
                }
                Field::Repo => form.repo.pop(),
            },
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                type_into(form, c);
            }
            _ => {}
        }),
    }
    Vec::new()
}

/// Does what the form's check says ⏎ does: create the agent, start its
/// idle worktree, or attach to it. A task can't reach an exited agent, so
/// that's refused rather than silently dropped.
fn submit(m: &mut Model) -> Vec<Effect> {
    let ticket = m.ticket();
    let Overlay::New(form) = &m.overlay else {
        return Vec::new();
    };
    let submission = m.derive(form).submission;
    let Overlay::New(form) = &mut m.overlay else {
        return Vec::new();
    };
    if let Phase::Submitting { .. } = form.phase {
        return Vec::new();
    }
    let launch = match submission {
        Submission::Ready(launch) => launch,
        Submission::Blocked(blocked) => {
            form.error = blocked.reason();
            return Vec::new();
        }
    };
    form.phase = Phase::Submitting {
        ticket,
        launch: launch.clone(),
    };
    let session = launch.session();
    m.pane = Pane::Connecting {
        name: session.clone(),
        ticket,
    };
    m.launching.push((ticket, session.clone()));
    m.follow = Some(Follow::Submitted {
        name: session,
        ticket,
    });
    vec![Effect::Job(Job::Launch {
        launch,
        harness: m.harness.clone(),
        ticket,
    })]
}

fn drop_connection_to(m: &mut Model, name: &SessionName) {
    if m.pane.session() == Some(name) {
        m.pane = Pane::Empty;
    }
}

fn remove(m: &mut Model, approved: Approved) -> Vec<Effect> {
    drop_connection_to(m, &approved.name());
    vec![Effect::Job(Job::Remove {
        approved,
        own: m.own_session.clone(),
    })]
}

fn on_confirm_key(m: &mut Model, confirm: Confirm, key: &KeyEvent) -> Vec<Effect> {
    match (confirm, key.code) {
        (Confirm::Kill(running), KeyCode::Char('y') | KeyCode::Enter) => {
            drop_connection_to(m, running.name());
            vec![Effect::Job(Job::Kill(running))]
        }
        (Confirm::Quit(_), KeyCode::Char('y') | KeyCode::Enter) => vec![Effect::Quit],
        (_, KeyCode::Esc | KeyCode::Char('n' | 'q')) => Vec::new(),
        (Confirm::Remove(proposal), KeyCode::Char(key)) => match proposal.answer(key) {
            Ok(Answer::Remove(approved)) => remove(m, approved),
            Ok(Answer::KillOnly(running)) => {
                drop_connection_to(m, running.name());
                vec![Effect::Job(Job::Kill(running))]
            }
            Err(proposal) => {
                m.overlay = Overlay::Confirm(Confirm::Remove(proposal));
                Vec::new()
            }
        },
        (confirm, _) => {
            m.overlay = Overlay::Confirm(confirm);
            Vec::new()
        }
    }
}

/// A click over [2] or [3] focuses it with the cursor on the row clicked,
/// taking the keys back from the agent. The wheel moves the cursor of the
/// panel already focused and is ignored elsewhere, so it never changes
/// focus. `None` when it's over neither.
fn on_lower_panel(
    m: &mut Model,
    mouse: MouseEvent,
    at: Position,
    clicked: bool,
) -> Option<Vec<Effect>> {
    let step = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
        -1
    } else {
        1
    };
    let row_at = |rows: &[(u16, usize)]| rows.iter().find(|(y, _)| *y == mouse.row).map(|r| r.1);
    let typing = matches!(m.focus(), Focus::Agent(_));
    let over_agent = m.areas.agent_panel.contains(at);
    let over_changes = m.areas.changes_panel.contains(at);
    if !(over_agent || over_changes) {
        return None;
    }
    if !clicked {
        let focused = match m.panel {
            Panel::Agent { .. } => over_agent,
            Panel::Changes { .. } => over_changes,
            _ => false,
        };
        if focused && !typing {
            m.move_in_panel(step);
        }
        return Some(Vec::new());
    }
    if let Some(c) = m.pane.connection_mut() {
        c.mode = Mode::Nav;
    }
    if over_agent {
        let row = row_at(&m.areas.agent_rows);
        let offset = match m.panel.listed() {
            Panel::Agent { offset } => offset,
            _ => 0,
        };
        m.panel = Panel::Agent { offset };
        if let Some(row) = row {
            m.point_at_row(|_, _| row);
        }
        return Some(Vec::new());
    }
    {
        let row = row_at(&m.areas.change_rows);
        let file = match m.panel.listed() {
            Panel::Changes { file } => file,
            _ => 0,
        };
        m.panel = Panel::Changes {
            file: row.unwrap_or(file),
        };
        Some(Vec::new())
    }
}

fn on_mouse(m: &mut Model, mouse: MouseEvent) -> Vec<Effect> {
    let at = Position::new(mouse.column, mouse.row);
    let clicked = matches!(mouse.kind, MouseEventKind::Down(_));
    if let Focus::Overlay(_) = m.focus() {
        return Vec::new();
    }
    if m.peeking() {
        return Vec::new();
    }
    if let Some(cell) = CellPos::within(m.areas.terminal, at) {
        let scrolling = matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        );
        if clicked && m.shows_document() {
            m.panel = m.panel.into_main();
            return Vec::new();
        }
        if scrolling && m.shows_document() {
            let up = matches!(mouse.kind, MouseEventKind::ScrollUp);
            m.docs.scroll_by(if up { -SCROLL } else { SCROLL });
            return Vec::new();
        }
        let forward = match m.focus() {
            Focus::Agent(c) => Some(c),
            Focus::Nav(Some(_)) if clicked => return enter_agent(m),
            Focus::Nav(Some(c)) if scrolling => Some(c),
            Focus::Nav(_) | Focus::Overlay(_) => None,
        };
        if let Some(c) = forward
            && let Some(bytes) = encode_mouse(mouse.kind, cell, c.term.modes())
        {
            c.term.send(bytes);
        }
        return Vec::new();
    }
    let scrolling = matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    );
    if (clicked || scrolling)
        && let Some(effects) = on_lower_panel(m, mouse, at, clicked)
    {
        return effects;
    }
    if clicked {
        if let Some(c) = m.pane.connection_mut() {
            c.mode = Mode::Nav;
        }
        let hit = m
            .areas
            .rows
            .iter()
            .find(|(y, _)| *y == mouse.row)
            .map(|(_, c)| c.clone());
        if let Some(cursor) = hit {
            m.panel = Panel::Agents;
            m.set_cursor(cursor);
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use agent_core::discover::{Observation, assemble};
    use agent_core::paths::{Layout, RepoRoot};
    use agent_core::ports::{RepoWorktrees, SessionInfo, SessionStatus};
    use crossterm::event::KeyEvent;

    use super::*;
    use crate::fixtures::{asking, name, reported, wt};
    use crate::model::{Blocked, Launchable, NameCheck, Row, Visibility};

    fn snapshot(sessions: Vec<SessionInfo>, pins: &[&str]) -> agent_core::model::Snapshot {
        let obs = Observation {
            sessions,
            repos: vec![
                (
                    RepoRoot::new("/src/api".into()).expect("absolute"),
                    RepoWorktrees {
                        main: wt("/src/api"),
                        linked: vec![wt("/w/api/fix"), wt("/w/api/idle")],
                    },
                ),
                (
                    RepoRoot::new("/src/web".into()).expect("absolute"),
                    RepoWorktrees {
                        main: wt("/src/web"),
                        linked: Vec::new(),
                    },
                ),
            ],
            warnings: Vec::new(),
        };
        let pins: Vec<SessionName> = pins.iter().map(|p| name(p)).collect();
        assemble(&obs, &Layout::new("/w".into()).expect("absolute"), &pins).0
    }

    fn root(path: &str) -> RepoRoot {
        RepoRoot::new(path.into()).expect("absolute")
    }

    /// A snapshot with `extra` repos (main checkout only) beyond `api` and `web`.
    fn snapshot_with(extra: &[&str]) -> agent_core::model::Snapshot {
        let mut obs = Observation {
            sessions: Vec::new(),
            repos: vec![
                (
                    root("/src/api"),
                    RepoWorktrees {
                        main: wt("/src/api"),
                        linked: Vec::new(),
                    },
                ),
                (
                    root("/src/web"),
                    RepoWorktrees {
                        main: wt("/src/web"),
                        linked: Vec::new(),
                    },
                ),
            ],
            warnings: Vec::new(),
        };
        for path in extra {
            obs.repos.push((
                root(path),
                RepoWorktrees {
                    main: wt(path),
                    linked: Vec::new(),
                },
            ));
        }
        assemble(&obs, &Layout::new("/w".into()).expect("absolute"), &[]).0
    }

    #[test]
    fn ctrl_d_and_ctrl_u_move_half_a_page_in_the_focused_panel() {
        let mut m = model(&[]);
        m.areas.page = 6;
        let selected_before = selected(&m);
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        );
        assert_ne!(selected(&m), selected_before, "moves the selection in [1]");
        assert_eq!(
            selected(&m).as_deref(),
            Some("idle"),
            "stops at the end rather than wrapping"
        );
        m.panel = Panel::Agent { offset: 0 };
        m.docs.page = 6;
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        );
        assert_eq!(m.docs.scroll, 3, "from [2] it's half a page of [0]");
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        );
        assert_eq!(m.docs.scroll, 0);
        m.docs.page = 1;
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        );
        assert_eq!(m.docs.scroll, 1, "at least one row");
    }

    #[test]
    fn enter_on_an_agent_still_being_created_does_nothing_until_its_launch_lands() {
        let mut m = model(&[]);
        let ticket = m.ticket();
        m.launching.push((ticket, name("idle")));
        m.set_cursor(Cursor::Entry(name("idle")));
        assert!(update(&mut m, key(KeyCode::Enter)).is_empty());
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("still starting"))
        );
        assert!(matches!(m.pane, Pane::Empty), "no connection was started");
        // A refresh showing the worktree idle doesn't lift the guard.
        update(
            &mut m,
            Msg::Snapshot(Ok((snapshot(Vec::new(), &[]), Vec::new()))),
        );
        m.set_cursor(Cursor::Entry(name("idle")));
        assert!(update(&mut m, key(KeyCode::Enter)).is_empty());
        // The launch landing does, whichever way it went.
        update(
            &mut m,
            Msg::Launched {
                ticket,
                result: Err("no".into()),
            },
        );
        assert_eq!(m.launching.len(), 0);
        let effects = update(&mut m, key(KeyCode::Enter));
        assert!(
            matches!(effects.as_slice(), [Effect::Job(Job::Start { .. })]),
            "the idle worktree starts as usual"
        );
    }

    #[test]
    fn the_picker_offers_catalogue_repos_the_sidebar_lacks_after_the_listed_ones() {
        let mut m = model(&[]);
        m.catalogue = vec![root("/srv/code/tools"), root("/src/api")];
        assert_eq!(
            picker_matches(&m, ""),
            Picks::Rows(vec![
                Pick::Listed(RepoKey::Root(root("/src/api"))),
                Pick::Listed(RepoKey::Root(root("/src/web"))),
                Pick::Found(root("/srv/code/tools")),
            ]),
            "api isn't offered twice"
        );
        assert_eq!(
            picker_matches(&m, "to"),
            Picks::Rows(vec![Pick::Found(root("/srv/code/tools"))])
        );
        assert_eq!(picker_matches(&m, "zzz"), Picks::NoMatch);
        assert_eq!(
            picker_matches(&m, "~bob/x"),
            Picks::Unusable(Unusable::UserHome)
        );
    }

    #[test]
    fn the_picker_puts_an_exact_name_before_a_longer_one_within_its_group() {
        let mut m = model(&[]);
        m.catalogue = vec![root("/srv/apis"), root("/srv/api")];
        assert_eq!(
            picker_matches(&m, "api"),
            Picks::Rows(vec![
                Pick::Listed(RepoKey::Root(root("/src/api"))),
                Pick::Found(root("/srv/api")),
                Pick::Found(root("/srv/apis")),
            ])
        );
    }

    #[test]
    fn picking_a_catalogue_repo_lists_it_then_focuses_it_once_it_shows() {
        let mut m = model(&[]);
        m.catalogue = vec![root("/srv/code/tools")];
        update(&mut m, key(KeyCode::Char('p')));
        type_text(&mut m, "tools");
        let effects = update(&mut m, key(KeyCode::Enter));
        assert!(
            matches!(effects.as_slice(), [Effect::Job(Job::ListRepo(dir))] if dir == std::path::Path::new("/srv/code/tools"))
        );
        assert!(matches!(m.overlay, Overlay::None));

        update(
            &mut m,
            Msg::Done(Ok(Outcome::Listed(agent_core::ops::Listed {
                repo: root("/srv/code/tools"),
                listing: Listing::Added,
            }))),
        );
        let following = Some(Follow::Repo {
            root: root("/srv/code/tools"),
            until: m.snapshots + 2,
        });
        assert_eq!(m.follow, following);
        update(&mut m, Msg::Snapshot(Ok((snapshot_with(&[]), Vec::new()))));
        assert_eq!(
            m.follow, following,
            "a refresh from before the listing keeps the follow"
        );
        update(
            &mut m,
            Msg::Snapshot(Ok((snapshot_with(&["/srv/code/tools"]), Vec::new()))),
        );
        assert_eq!(m.follow, None);
        assert_eq!(selected(&m).as_deref(), Some("tools"));
    }

    #[test]
    fn a_typed_path_is_the_only_row_and_adds_the_repo_holding_it() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char('p')));
        type_text(&mut m, "../other");
        assert_eq!(
            picker_matches(&m, "../other"),
            Picks::Rows(vec![Pick::Path("/w/api/fix/src/../other".into())])
        );
        let effects = update(&mut m, key(KeyCode::Enter));
        assert!(
            matches!(effects.as_slice(), [Effect::Job(Job::ListRepo(dir))] if dir == std::path::Path::new("/w/api/fix/src/../other"))
        );
        assert!(matches!(m.overlay, Overlay::None));

        update(&mut m, key(KeyCode::Char('p')));
        type_text(&mut m, "zzz");
        assert!(update(&mut m, key(KeyCode::Enter)).is_empty());
        assert!(
            matches!(m.overlay, Overlay::Picker(_)),
            "⏎ with no row keeps the picker open"
        );
    }

    #[test]
    fn a_listed_repo_that_never_shows_stops_being_followed() {
        let mut m = model(&[]);
        update(
            &mut m,
            Msg::Done(Ok(Outcome::Listed(agent_core::ops::Listed {
                repo: root("/srv/code/tools"),
                listing: Listing::Added,
            }))),
        );
        update(&mut m, Msg::Snapshot(Ok((snapshot_with(&[]), Vec::new()))));
        assert!(m.follow.is_some());
        update(&mut m, Msg::Snapshot(Ok((snapshot_with(&[]), Vec::new()))));
        assert_eq!(
            m.follow, None,
            "the refresh the listing caused didn't show it, so later picks aren't blocked"
        );
    }

    #[test]
    fn listing_a_repo_doesnt_clobber_a_launch_being_followed() {
        let mut m = model(&[]);
        let ticket = m.ticket();
        m.follow = Some(Follow::Submitted {
            name: name("web"),
            ticket,
        });
        update(
            &mut m,
            Msg::Done(Ok(Outcome::Listed(agent_core::ops::Listed {
                repo: root("/srv/code/tools"),
                listing: Listing::AlreadyListed,
            }))),
        );
        assert_eq!(
            m.follow,
            Some(Follow::Submitted {
                name: name("web"),
                ticket,
            })
        );
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text == "tools is already listed")
        );
    }

    /// `api` (main exited, agent `fix` live, agent `idle` with no session) and `web` (main only).
    fn model(pins: &[&str]) -> Model {
        let sessions = vec![
            SessionInfo {
                name: name("fix"),
                status: SessionStatus::Live {
                    cwd: Some("/w/api/fix".into()),
                },
            },
            SessionInfo {
                name: name("api"),
                status: SessionStatus::Exited { cwd: None },
            },
        ];
        let mut m = Model::new(
            "/w/api/fix/src".into(),
            None,
            "claude".parse().expect("parses"),
            "claude".into(),
        );
        m.view = crate::model::View::Focus;
        m.visibility = Visibility::All;
        update(
            &mut m,
            Msg::Snapshot(Ok((snapshot(sessions, pins), Vec::new()))),
        );
        m.changes_view = ChangesView::Flat;
        m
    }

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn selected(m: &Model) -> Option<String> {
        m.cursor()
            .and_then(Cursor::entry_name)
            .map(ToString::to_string)
    }

    #[test]
    fn starts_on_the_agent_containing_the_cwd() {
        assert_eq!(selected(&model(&[])).as_deref(), Some("fix"));
    }

    #[test]
    fn focus_moves_within_the_repo_and_wraps() {
        let mut m = model(&[]);
        let order: Vec<_> = (0..3)
            .map(|_| {
                update(&mut m, key(KeyCode::Char('j')));
                selected(&m).expect("succeeds")
            })
            .collect();
        assert_eq!(order, ["idle", "api", "fix"]);
    }

    #[test]
    fn overview_expands_only_the_selected_repo_and_collapses_single_entry_repos() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char('[')));
        let rows = m.rows();
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert!(
            matches!(rows.last(), Some(Row::Entry { name, place: crate::model::Place::Solo }) if name.as_str() == "web")
        );
    }

    #[test]
    fn enter_on_a_repo_row_focuses_it() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char('[')));
        update(&mut m, key(KeyCode::Char('g')));
        assert!(matches!(m.cursor(), Some(Cursor::Repo(_))));
        update(&mut m, key(KeyCode::Enter));
        assert_eq!(m.view, View::Focus);
        assert_eq!(selected(&m).as_deref(), Some("api"));
    }

    #[test]
    fn cursor_falls_back_within_its_repo_when_its_entry_goes() {
        let mut m = model(&[]);
        let obs = Observation {
            repos: vec![(
                RepoRoot::new("/src/api".into()).expect("absolute"),
                RepoWorktrees {
                    main: wt("/src/api"),
                    linked: Vec::new(),
                },
            )],
            ..Observation::default()
        };
        let snap = assemble(&obs, &Layout::new("/w".into()).expect("absolute"), &[]).0;
        update(&mut m, Msg::Snapshot(Ok((snap, Vec::new()))));
        assert_eq!(selected(&m).as_deref(), Some("api"));
    }

    #[test]
    fn pins_come_early_in_the_switcher() {
        let mut m = model(&["idle"]);
        m.touch_mru(&name("fix"));
        m.touch_mru(&name("api"));
        let order: Vec<_> = m
            .switcher_items()
            .iter()
            .map(|(_, e)| e.name().to_string())
            .collect();
        assert_eq!(order, ["api", "idle", "fix", "web"]);
    }

    fn with_turn(m: &mut Model) {
        let mut all = m.activities.take().unwrap_or_default();
        for agent in ["fix", "api"] {
            all.insert(
                name(agent),
                reported(
                    agent_core::activity::State::Working { tools: Vec::new() },
                    5,
                ),
            );
        }
        m.activities = Some(all);
    }

    fn with_changes(m: &mut Model, files: usize) {
        let changed: Vec<agent_core::ports::Change> = (0..files)
            .map(|i| agent_core::ports::Change {
                path: format!("src/f{i}.rs").into(),
                lines: agent_core::ports::Lines::Untracked,
                state: agent_core::ports::FileState::Committed,
            })
            .collect();
        m.set_facts(
            &name("fix"),
            Ok(agent_core::ports::WorktreeFacts {
                pending: changed.clone(),
                changed,
                ..Default::default()
            }),
        );
    }

    fn with_paths(m: &mut Model, paths: &[&str]) {
        let changed: Vec<agent_core::ports::Change> = paths
            .iter()
            .map(|p| agent_core::ports::Change {
                path: (*p).into(),
                lines: agent_core::ports::Lines::Counted {
                    added: 2,
                    removed: 1,
                },
                state: agent_core::ports::FileState::Committed,
            })
            .collect();
        m.set_facts(
            &name("fix"),
            Ok(agent_core::ports::WorktreeFacts {
                pending: changed.clone(),
                changed,
                ..Default::default()
            }),
        );
    }

    fn tree_labels(m: &Model) -> Vec<String> {
        m.change_list().iter().map(|r| r.label.clone()).collect()
    }

    const PATHS: [&str; 5] = [
        "a/b/one.rs",
        "a/b/two.rs",
        "a/c/three.rs",
        "top.rs",
        "z/deep/er/four.rs",
    ];

    #[test]
    fn the_tree_is_the_default_and_a_backtick_switches_to_the_flat_list_and_back() {
        assert_eq!(
            Model::new(
                "/".into(),
                None,
                "claude".parse().expect("parses"),
                "claude".into()
            )
            .changes_view,
            ChangesView::Tree
        );
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &PATHS);
        m.panel = Panel::Changes { file: 0 };
        assert_eq!(tree_labels(&m)[0], "a");
        m.panel = Panel::Changes { file: 8 };
        assert_eq!(tree_labels(&m)[8], "top.rs");
        update(&mut m, key(KeyCode::Char('`')));
        assert_eq!(m.changes_view, ChangesView::Flat);
        assert_eq!(
            m.panel,
            Panel::Changes { file: 3 },
            "the cursor stays on top.rs"
        );
        update(&mut m, key(KeyCode::Char('`')));
        assert_eq!(m.changes_view, ChangesView::Tree);
        assert_eq!(m.panel, Panel::Changes { file: 8 });
    }

    fn with_scopes(m: &mut Model) {
        use agent_core::ports::{Change, Edit, FileState, Lines};
        let change = |path: &str, added, state| Change {
            path: path.into(),
            lines: Lines::Counted { added, removed: 0 },
            state,
        };
        let edited = FileState::Unstaged(Edit::Modified);
        m.set_facts(
            &name("fix"),
            Ok(agent_core::ports::WorktreeFacts {
                ahead: 3,
                dirty: 1,
                compared: agent_core::ports::Compared::Branch {
                    branch: "fix".into(),
                    base: "origin/main".into(),
                },
                changed: vec![
                    change("a/old.rs", 40, FileState::Committed),
                    change("a/hot.rs", 9, edited),
                ],
                pending: vec![change("a/hot.rs", 2, edited)],
                ..Default::default()
            }),
        );
    }

    #[test]
    fn changes_start_on_the_uncommitted_tab_and_brackets_switch_tabs_for_the_session() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Flat;
        with_scopes(&mut m);
        assert_eq!(m.changes_scope, crate::tree::ChangesScope::Uncommitted);
        assert_eq!(tree_labels(&m), ["a/hot.rs"], "only uncommitted files");
        assert_eq!(m.changes().len(), 1);
        m.panel = Panel::Changes { file: 1 };
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.changes_scope, crate::tree::ChangesScope::Since);
        assert_eq!(tree_labels(&m), ["a/old.rs", "a/hot.rs"]);
        assert_eq!(
            m.change_at(1).map(|c| c.state),
            Some(agent_core::ports::FileState::Committed),
            "committed files carry no uncommitted state"
        );
        assert_eq!(
            m.panel,
            Panel::Changes { file: 2 },
            "the cursor follows hot.rs"
        );
        update(&mut m, key(KeyCode::Char('[')));
        assert_eq!(m.changes_scope, crate::tree::ChangesScope::Uncommitted);
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        update(&mut m, key(KeyCode::Char(']')));
        update(&mut m, key(KeyCode::Char('1')));
        update(&mut m, key(KeyCode::Char('3')));
        assert_eq!(
            m.changes_scope,
            crate::tree::ChangesScope::Since,
            "refocusing keeps the tab"
        );
    }

    #[test]
    fn the_bracket_hint_names_the_second_tab_for_where_the_checkout_is() {
        use agent_core::ports::Compared;
        let mut m = model(&[]);
        with_scopes(&mut m);
        m.set_cursor(crate::model::Cursor::Entry(name("fix")));
        let hint = |m: &Model| crate::keymap::Situation::of(m).next_changes_tab;
        assert_eq!(hint(&m), "tab: branch");
        m.toggle_changes_scope();
        assert_eq!(hint(&m), "tab: uncommitted");
        m.toggle_changes_scope();
        let mut facts = match m.facts.get(&name("fix")) {
            Some(Ok(facts)) => facts.clone(),
            _ => panic!("facts"),
        };
        facts.compared = Compared::Trunk {
            branch: "main".into(),
            base: "origin/main".into(),
        };
        m.set_facts(&name("fix"), Ok(facts));
        assert_eq!(hint(&m), "tab: unpushed");
    }

    #[test]
    fn a_row_missing_from_the_other_tab_sends_the_cursor_to_the_first_row() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Flat;
        with_scopes(&mut m);
        update(&mut m, key(KeyCode::Char(']')));
        m.panel = Panel::Changes { file: 1 };
        update(&mut m, key(KeyCode::Char('[')));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
    }

    #[test]
    fn the_diff_under_the_cursor_is_read_against_the_tab_it_is_in() {
        use agent_core::ports::DiffBase;
        let mut m = model(&[]);
        m.changes_view = ChangesView::Flat;
        with_scopes(&mut m);
        let base = |m: &Model| match m.hovered_document() {
            Some((Key::Diff(key), _)) => key.base,
            other => panic!("a diff, got {other:?}"),
        };
        m.panel = Panel::Changes { file: 1 };
        assert_eq!(base(&m), DiffBase::Head);
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(base(&m), DiffBase::Fork);
    }

    #[test]
    fn enter_on_a_folder_folds_it_and_the_cursor_stays_on_it() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &PATHS);
        m.panel = Panel::Changes { file: 1 };
        assert_eq!(
            tree_labels(&m),
            [
                "a",
                "b",
                "one.rs",
                "two.rs",
                "c",
                "three.rs",
                "z/deep/er",
                "four.rs",
                "top.rs"
            ]
        );
        update(&mut m, key(KeyCode::Enter));
        assert_eq!(
            m.panel,
            Panel::Changes { file: 1 },
            "Enter folds instead of opening [0]"
        );
        assert_eq!(
            tree_labels(&m),
            ["a", "b", "c", "three.rs", "z/deep/er", "four.rs", "top.rs"]
        );
        update(&mut m, key(KeyCode::Enter));
        assert_eq!(tree_labels(&m).len(), 9);
        m.panel = Panel::Changes { file: 2 };
        update(&mut m, key(KeyCode::Enter));
        assert!(
            matches!(m.panel, Panel::Main(_)),
            "a file still opens its diff"
        );
    }

    #[test]
    fn dash_folds_everything_equals_opens_everything_and_the_state_is_per_agent() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &PATHS);
        m.panel = Panel::Changes { file: 2 };
        update(&mut m, key(KeyCode::Char('-')));
        assert_eq!(tree_labels(&m), ["a", "z/deep/er", "top.rs"]);
        assert_eq!(
            m.panel,
            Panel::Changes { file: 0 },
            "the cursor goes to the folder holding its row"
        );
        with_paths(&mut m, &["a/b/one.rs", "new/file.rs", "top.rs"]);
        assert_eq!(
            tree_labels(&m),
            ["a/b", "new", "file.rs", "top.rs"],
            "a closed folder stays closed while the files change and a new one starts open"
        );
        update(&mut m, key(KeyCode::Char('=')));
        assert_eq!(tree_labels(&m).len(), 5);
        let other = name("api");
        assert!(
            m.collapsed
                .get(&other)
                .is_none_or(std::collections::HashSet::is_empty)
        );
    }

    fn closed(m: &Model) -> Vec<String> {
        m.collapsed
            .get(&name("fix"))
            .into_iter()
            .flatten()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[test]
    fn a_closed_chain_stays_closed_when_it_splits_and_when_it_merges_back() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &["a/b/c/x.rs", "top.rs"]);
        m.panel = Panel::Changes { file: 0 };
        update(&mut m, key(KeyCode::Enter));
        assert_eq!(tree_labels(&m), ["a/b/c", "top.rs"]);
        assert_eq!(closed(&m), ["a/b/c"]);

        with_paths(&mut m, &["a/b/c/x.rs", "a/y.rs", "top.rs"]);
        assert_eq!(tree_labels(&m), ["a", "b/c", "y.rs", "top.rs"]);
        let rows = m.change_list();
        assert_eq!(rows[0].node, crate::tree::Node::Folder { open: true });
        assert_eq!(rows[1].node, crate::tree::Node::Folder { open: false });

        with_paths(&mut m, &["a/b/c/x.rs", "top.rs"]);
        assert_eq!(tree_labels(&m), ["a/b/c", "top.rs"], "merged and closed");
    }

    #[test]
    fn closing_an_outer_folder_keeps_a_chain_it_merges_into_closed_and_enter_opens_all_of_it() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &["a/b/c/x.rs", "a/y.rs"]);
        m.panel = Panel::Changes { file: 0 };
        update(&mut m, key(KeyCode::Enter));
        assert_eq!(closed(&m), ["a"]);
        with_paths(&mut m, &["a/b/c/x.rs"]);
        assert_eq!(tree_labels(&m), ["a/b/c"]);
        assert_eq!(
            m.change_list()[0].node,
            crate::tree::Node::Folder { open: false },
            "the outer folder was closed"
        );
        m.panel = Panel::Changes { file: 0 };
        update(&mut m, key(KeyCode::Enter));
        assert!(closed(&m).is_empty(), "{:?}", closed(&m));
        assert_eq!(tree_labels(&m), ["a/b/c", "x.rs"]);
    }

    #[test]
    fn dash_closes_the_intermediate_folders_of_a_chain_too() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &["a/b/c/x.rs", "top.rs"]);
        m.panel = Panel::Changes { file: 0 };
        update(&mut m, key(KeyCode::Char('-')));
        assert_eq!(closed(&m), ["a", "a/b", "a/b/c"]);
        with_paths(&mut m, &["a/b/c/x.rs", "a/b/y.rs", "top.rs"]);
        assert_eq!(
            tree_labels(&m),
            ["a/b", "top.rs"],
            "still closed after a split"
        );
    }

    #[test]
    fn closed_folders_that_no_longer_exist_are_forgotten() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &["a/b/x.rs", "d/y.rs"]);
        update(&mut m, key(KeyCode::Char('3')));
        update(&mut m, key(KeyCode::Char('-')));
        assert_eq!(closed(&m), ["a", "a/b", "d"]);
        with_paths(&mut m, &["d/y.rs"]);
        assert_eq!(closed(&m), ["d"]);
    }

    #[test]
    fn the_changes_cursor_stays_on_its_file_when_rows_appear_above_it() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &["b.rs", "c.rs"]);
        update(&mut m, key(KeyCode::Char('3')));
        update(&mut m, key(KeyCode::Char('j')));
        let on = |m: &Model| m.file_under_cursor().map(|(_, f)| f);
        assert_eq!(on(&m), Some("c.rs".into()));
        let refreshed = |m: &mut Model, paths: &[&str]| {
            let changed: Vec<agent_core::ports::Change> = paths
                .iter()
                .map(|p| agent_core::ports::Change {
                    path: (*p).into(),
                    lines: agent_core::ports::Lines::Untracked,
                    state: agent_core::ports::FileState::Committed,
                })
                .collect();
            update(
                m,
                Msg::Inspected {
                    name: name("fix"),
                    facts: Ok(agent_core::ports::WorktreeFacts {
                        pending: changed.clone(),
                        changed,
                        ..Default::default()
                    }),
                },
            );
        };
        m.inspections.insert(
            name("fix"),
            Inspection {
                events: None,
                at: Instant::now(),
            },
        );
        refreshed(&mut m, &["a.rs", "aa/z.rs", "b.rs", "c.rs"]);
        assert_eq!(on(&m), Some("c.rs".into()), "rows above came and went");
        refreshed(&mut m, &["a.rs", "b.rs"]);
        assert_eq!(
            on(&m),
            Some("b.rs".into()),
            "the file is gone, so the nearest row"
        );
    }

    #[test]
    fn the_changes_cursor_on_a_folder_follows_it_when_its_chain_changes() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &["a/b/x.rs", "top.rs"]);
        m.panel = Panel::Changes { file: 0 };
        with_paths(&mut m, &["a/b/x.rs", "a/y.rs", "top.rs"]);
        assert_eq!(
            m.change_row(match m.panel {
                Panel::Changes { file } => file,
                _ => unreachable!(),
            })
            .map(|r| r.label),
            Some("b".into()),
            "the chain split and the cursor stays on the same folder, a/b"
        );
    }

    #[test]
    fn the_tree_is_built_once_per_generation_view_and_fold_state() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &PATHS);
        let first = m.change_list();
        assert!(std::sync::Arc::ptr_eq(&first, &m.change_list()));
        m.panel = Panel::Changes { file: 0 };
        m.toggle_folder();
        let folded = m.change_list();
        assert!(
            !std::sync::Arc::ptr_eq(&first, &folded),
            "fold state changed"
        );
        assert!(std::sync::Arc::ptr_eq(&folded, &m.change_list()));
        m.toggle_changes_view();
        assert!(!std::sync::Arc::ptr_eq(&folded, &m.change_list()), "view");
        m.toggle_changes_view();
        let before = m.change_list();
        with_paths(&mut m, &PATHS);
        assert!(!std::sync::Arc::ptr_eq(&before, &m.change_list()), "facts");
    }

    #[test]
    fn dash_in_the_flat_list_says_there_are_no_folders() {
        let mut m = model(&[]);
        with_paths(&mut m, &PATHS);
        m.panel = Panel::Changes { file: 0 };
        update(&mut m, key(KeyCode::Char('-')));
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("no folders"))
        );
    }

    #[test]
    fn a_folder_row_is_not_a_file_for_e_and_reads_as_a_folder_in_main() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &PATHS);
        m.panel = Panel::Changes { file: 0 };
        assert!(m.file_under_cursor().is_none());
        assert!(
            matches!(m.hover(), Some(crate::document::Hover::Folder(k)) if k.path == std::path::Path::new("a"))
        );
        update(&mut m, key(KeyCode::Char('e')));
        assert!(m.status.is_some(), "a folder missing from disk says so");
        m.panel = Panel::Changes { file: 2 };
        assert!(m.file_under_cursor().is_some());
    }

    #[test]
    fn number_keys_focus_panels_and_esc_returns_to_the_list() {
        let mut m = model(&[]);
        with_changes(&mut m, 3);
        with_turn(&mut m);
        update(&mut m, key(KeyCode::Char('2')));
        assert_eq!(m.panel, Panel::Agent { offset: 0 });
        update(&mut m, key(KeyCode::Char('3')));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        assert!(update(&mut m, key(KeyCode::Esc)).is_empty());
        assert_eq!(
            m.panel,
            Panel::Agents,
            "Esc leaves [3] before it leaves NAV"
        );
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('1')));
        assert_eq!(m.panel, Panel::Agents);
        with_changes(&mut m, 0);
        update(&mut m, key(KeyCode::Char('3')));
        assert_eq!(m.panel, Panel::Agents, "nothing to show in [3]");
    }

    #[test]
    fn j_and_k_move_within_the_focused_panel() {
        let mut m = model(&[]);
        with_changes(&mut m, 3);
        with_turn(&mut m);
        update(&mut m, key(KeyCode::Char('3')));
        for _ in 0..5 {
            update(&mut m, key(KeyCode::Char('j')));
        }
        assert_eq!(
            m.panel,
            Panel::Changes { file: 2 },
            "the cursor stops at the last file"
        );
        assert_eq!(selected(&m).as_deref(), Some("fix"), "the selection stays");
        update(&mut m, key(KeyCode::Char('g')));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        update(&mut m, key(KeyCode::Char('G')));
        update(&mut m, key(KeyCode::Char('k')));
        assert_eq!(m.panel, Panel::Changes { file: 1 });
        let effects = update(&mut m, key(KeyCode::Char('e')));
        assert!(
            matches!(effects.as_slice(), [Effect::Edit(Open::File { root, file })] if root.ends_with("w/api/fix") && file == std::path::Path::new("src/f1.rs")),
            "e opens the file under the cursor"
        );
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(m.panel, Panel::Agent { offset: 0 });
        assert_eq!(m.row_at(), 0, "[2] has only its turn row");
        update(&mut m, key(KeyCode::Char('1')));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(
            selected(&m).as_deref(),
            Some("idle"),
            "[1] moves the selection"
        );
    }

    #[test]
    fn panels_fall_back_to_the_list() {
        let mut m = model(&[]);
        with_changes(&mut m, 3);
        with_turn(&mut m);
        update(&mut m, key(KeyCode::Char('3')));
        update(&mut m, key(KeyCode::Char('}')));
        assert_eq!(m.panel, Panel::Agents, "a new selection starts in [1]");
        update(&mut m, key(KeyCode::Char('{')));
        update(&mut m, key(KeyCode::Char('2')));
        m.revalidate();
        assert_eq!(
            m.panel,
            Panel::Agent { offset: 0 },
            "the same agent keeps [2]"
        );
        update(&mut m, key(KeyCode::Esc));
        update(&mut m, key(KeyCode::Char('[')));
        update(&mut m, key(KeyCode::Char('g')));
        assert!(matches!(m.cursor(), Some(Cursor::Repo(_))));
        update(&mut m, key(KeyCode::Char('2')));
        assert_eq!(m.panel, Panel::Agents, "a repo row has no [2]");
    }

    #[test]
    fn tab_cycles_the_panels_and_wraps() {
        let mut m = model(&[]);
        with_changes(&mut m, 3);
        with_turn(&mut m);
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Agent { offset: 0 });
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Agents);
        update(&mut m, key(KeyCode::BackTab));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        update(&mut m, key(KeyCode::BackTab));
        assert_eq!(m.panel, Panel::Agent { offset: 0 });
    }

    #[test]
    fn h_and_l_move_between_the_left_panels_like_shift_tab_and_tab() {
        let mut m = model(&[]);
        with_changes(&mut m, 3);
        with_turn(&mut m);
        update(&mut m, key(KeyCode::Char('l')));
        assert_eq!(m.panel, Panel::Agent { offset: 0 });
        update(&mut m, key(KeyCode::Char('l')));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        update(&mut m, key(KeyCode::Char('l')));
        assert_eq!(m.panel, Panel::Agents);
        update(&mut m, key(KeyCode::Char('h')));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        update(&mut m, key(KeyCode::Char('h')));
        assert_eq!(m.panel, Panel::Agent { offset: 0 });
        m.panel = Panel::Main(crate::model::Origin::Turn { offset: 0 });
        update(&mut m, key(KeyCode::Char('l')));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
    }

    #[test]
    fn l_no_longer_attaches_and_t_says_send_moved() {
        let mut m = model(&[]);
        let effects = update(&mut m, key(KeyCode::Char('l')));
        assert!(effects.is_empty());
        update(&mut m, key(KeyCode::Char('t')));
        assert_eq!(
            m.status.as_ref().map(|s| s.text.as_str()),
            Some("send is s now")
        );
    }

    #[test]
    fn tab_skips_panels_that_have_nothing_to_focus() {
        let mut m = model(&[]);
        with_changes(&mut m, 3);
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Changes { file: 0 }, "no turn recorded");
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Agents);
        with_changes(&mut m, 0);
        with_turn(&mut m);
        update(&mut m, key(KeyCode::BackTab));
        assert_eq!(m.panel, Panel::Agent { offset: 0 }, "no files");
        m.work_room = WorkRoom::TooShort;
        with_changes(&mut m, 3);
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Agents, "no room for [3]");
    }

    #[test]
    fn tab_from_a_document_goes_to_the_panel_after_its_origin() {
        let mut m = model(&[]);
        with_changes(&mut m, 3);
        with_turn(&mut m);
        m.panel = Panel::Main(crate::model::Origin::Turn { offset: 0 });
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        m.panel = Panel::Main(crate::model::Origin::Changes { file: 0 });
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Agents);
    }

    #[test]
    fn a_tab_switch_reschedules_the_preview_so_the_pane_never_sticks() {
        let mut m = model(&["fix"]);
        m.pane = Pane::Empty;
        m.preview_at = None;
        update(&mut m, key(KeyCode::Char(']')));
        assert!(
            m.preview_at.is_some(),
            "the selection may have changed under the tab"
        );
    }

    #[test]
    fn brackets_cycle_the_tabs_of_the_list_only() {
        let mut m = model(&["fix"]);
        with_changes(&mut m, 3);
        with_turn(&mut m);
        update(&mut m, key(KeyCode::Char('[')));
        assert_eq!(m.view, View::Overview);
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.view, View::Focus);
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.view, View::Focus, "[2] has no tabs");
        assert_eq!(m.panel, Panel::Agent { offset: 0 });
    }

    #[test]
    fn e_with_no_file_under_the_cursor_falls_through() {
        let mut m = model(&[]);
        with_changes(&mut m, 3);
        update(&mut m, key(KeyCode::Char('3')));
        update(&mut m, key(KeyCode::Char('G')));
        with_changes(&mut m, 1);
        let effects = update(&mut m, key(KeyCode::Char('e')));
        assert!(
            matches!(effects.as_slice(), [Effect::Edit(Open::File { file, .. })] if file == std::path::Path::new("src/f0.rs")),
            "the cursor is clamped to the files left"
        );
        with_changes(&mut m, 0);
        let effects = update(&mut m, key(KeyCode::Enter));
        assert!(
            !effects.iter().any(|e| matches!(e, Effect::Edit(_))),
            "with nothing under the cursor, ⏎ attaches as in [1]"
        );
    }

    #[test]
    fn e_opens_the_selected_checkout_from_the_list() {
        let mut m = model(&[]);
        let effects = update(&mut m, key(KeyCode::Char('e')));
        assert!(
            matches!(effects.as_slice(), [Effect::Edit(Open::Checkout(path))] if path.ends_with("w/api/fix"))
        );
        assert!(
            update(&mut m, key(KeyCode::Char('o'))).is_empty(),
            "o is free"
        );
    }

    #[test]
    fn idle_and_exited_checkouts_start_through_agent() {
        let mut m = model(&[]);
        assert!(update(&mut m, key(KeyCode::Char('j'))).is_empty());
        let effects = update(&mut m, key(KeyCode::Enter));
        assert!(matches!(
            effects.as_slice(),
            [Effect::Job(Job::Start { .. })]
        ));
        update(&mut m, key(KeyCode::Char('j')));
        let effects = update(&mut m, key(KeyCode::Enter));
        assert!(
            matches!(effects.as_slice(), [Effect::Job(Job::Start { at, .. })] if at.name().as_str() == "api"),
            "an exited main checkout restarts through agent, not zellij's cached command"
        );
    }

    #[test]
    fn a_failed_start_only_clears_its_own_pending_pane() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char('j')));
        let effects = update(&mut m, key(KeyCode::Enter));
        let [Effect::Job(Job::Start { ticket: stale, .. })] = effects.as_slice() else {
            panic!()
        };
        let stale = *stale;
        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Enter));
        update(
            &mut m,
            Msg::Launched {
                ticket: stale,
                result: Err("boom".into()),
            },
        );
        assert!(matches!(&m.pane, Pane::Connecting { name, .. } if name.as_str() == "api"));
    }

    #[test]
    fn kill_needs_confirmation() {
        let mut m = model(&[]);
        assert!(update(&mut m, key(KeyCode::Char('d'))).is_empty());
        assert!(matches!(m.overlay, Overlay::Confirm(Confirm::Kill(_))));
        assert!(update(&mut m, key(KeyCode::Char('x'))).is_empty());
        let effects = update(&mut m, key(KeyCode::Char('y')));
        assert!(
            matches!(effects.as_slice(), [Effect::Job(Job::Kill(r))] if r.name().as_str() == "fix")
        );
    }

    #[test]
    fn the_session_the_tui_runs_in_cant_be_killed_or_activated() {
        let mut m = model(&[]);
        m.own_session = Some(name("fix"));
        assert!(update(&mut m, key(KeyCode::Char('d'))).is_empty());
        assert!(matches!(m.overlay, Overlay::None));
        assert!(update(&mut m, key(KeyCode::Enter)).is_empty());
    }

    fn type_text(m: &mut Model, text: &str) {
        for c in text.chars() {
            update(m, key(KeyCode::Char(c)));
        }
    }

    fn form(m: &Model) -> &NewAgent {
        match &m.overlay {
            Overlay::New(form) => form,
            _ => panic!("the form isn't open"),
        }
    }

    fn derived(m: &Model) -> crate::model::Derived {
        m.derive(form(m))
    }

    fn open_form(m: &mut Model) {
        update(m, key(KeyCode::Char('n')));
    }

    #[test]
    fn c_opens_the_checkout_form_and_names_the_agent_from_the_branch_or_link() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char('c')));
        assert_eq!(form(&m).kind, FormKind::Checkout);
        assert_eq!(derived(&m).suggestion, None);
        assert!(matches!(
            derived(&m).submission,
            Submission::Blocked(Blocked::Target(_))
        ));

        type_text(&mut m, "axel/chargebee-webhook-handler-classes");
        let d = derived(&m);
        assert_eq!(d.name, "axel-chargebee");
        assert_eq!(
            d.suggestion.map(|s| s.source),
            Some(crate::model::NameSource::Branch)
        );
        assert!(matches!(
            d.submission,
            Submission::Ready(Launchable::Checkout {
                target: agent_core::names::Checkout::Branch(_),
                ..
            })
        ));

        update(&mut m, key(KeyCode::Esc));
        update(&mut m, key(KeyCode::Char('c')));
        update(
            &mut m,
            Msg::Paste("https://github.com/acme/api/pull/482".into()),
        );
        let d = derived(&m);
        assert_eq!(d.name, "pr-482");
        assert_eq!(
            d.suggestion.map(|s| s.source),
            Some(crate::model::NameSource::Link)
        );

        update(&mut m, key(KeyCode::Esc));
        update(&mut m, key(KeyCode::Char('c')));
        type_text(&mut m, "https://github.com/acme/api/issues/5");
        assert!(matches!(
            derived(&m).submission,
            Submission::Blocked(Blocked::Target(_))
        ));
        assert!(
            matches!(form(&m).naming.state, NamingState::Idle),
            "never asks the model"
        );
    }

    #[test]
    fn a_name_already_an_agent_attaches_on_its_branch_and_is_refused_on_another() {
        let on_branch = |path: &str, branch: &str| agent_core::model::Worktree {
            path: path.into(),
            head: agent_core::model::Head::Branch(branch.parse().expect("parses")),
        };
        let obs = Observation {
            sessions: vec![SessionInfo {
                name: name("fix"),
                status: SessionStatus::Live {
                    cwd: Some("/w/api/fix".into()),
                },
            }],
            repos: vec![(
                root("/src/api"),
                RepoWorktrees {
                    main: wt("/src/api"),
                    linked: vec![on_branch("/w/api/fix", "feature/x"), wt("/w/api/idle")],
                },
            )],
            warnings: Vec::new(),
        };
        let snapshot = assemble(&obs, &Layout::new("/w".into()).expect("absolute"), &[]).0;
        let mut m = Model::new(
            "/w/api/fix/src".into(),
            None,
            "claude".parse().expect("parses"),
            "claude".into(),
        );
        m.view = crate::model::View::Focus;
        m.visibility = Visibility::All;
        update(&mut m, Msg::Snapshot(Ok((snapshot, Vec::new()))));
        update(&mut m, key(KeyCode::Char('c')));
        type_text(&mut m, "feature/x");
        update(&mut m, key(KeyCode::Tab));
        type_text(&mut m, "fix");
        assert!(matches!(
            derived(&m).submission,
            Submission::Ready(Launchable::Checkout { .. })
        ));
        update(&mut m, key(KeyCode::BackTab));
        type_text(&mut m, "-old");
        assert!(matches!(
            derived(&m).submission,
            Submission::Blocked(Blocked::OnAnotherBranch(Some(on))) if on.as_str() == "feature/x"
        ));
        update(&mut m, key(KeyCode::Enter));
        assert!(
            matches!(form(&m).phase, Phase::Editing),
            "refused before the worker"
        );

        update(&mut m, key(KeyCode::Esc));
        update(&mut m, key(KeyCode::Char('c')));
        update(
            &mut m,
            Msg::Paste("https://github.com/acme/api/pull/9".into()),
        );
        update(&mut m, key(KeyCode::Tab));
        type_text(&mut m, "fix");
        assert!(
            matches!(
                derived(&m).submission,
                Submission::Ready(Launchable::Checkout { .. })
            ),
            "a pull request's branch isn't known until the worker asks"
        );
    }

    #[test]
    fn a_name_that_is_an_agent_in_another_repo_is_refused() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char('c')));
        type_text(&mut m, "fix-ci");
        update(&mut m, key(KeyCode::Tab));
        type_text(&mut m, "fix");
        update(&mut m, key(KeyCode::Tab));
        type_text(&mut m, "web");
        assert!(matches!(
            derived(&m).submission,
            Submission::Blocked(Blocked::InAnotherRepo(repo)) if repo == "api"
        ));
    }

    #[test]
    fn a_pull_request_link_picks_the_repo_it_names_while_nothing_is_typed() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char('c')));
        update(
            &mut m,
            Msg::Paste("https://github.com/acme/web/pull/9".into()),
        );
        assert_eq!(
            derived(&m).repo_input,
            crate::model::RepoInput::Default(Some(root("/src/web")))
        );
        update(&mut m, key(KeyCode::Tab));
        update(&mut m, key(KeyCode::Tab));
        type_text(&mut m, "api");
        assert!(matches!(
            derived(&m).repo_input,
            crate::model::RepoInput::Search(Some(found)) if found == root("/src/api")
        ));

        update(&mut m, key(KeyCode::Esc));
        update(&mut m, key(KeyCode::Char('c')));
        update(
            &mut m,
            Msg::Paste("https://github.com/acme/other/pull/9".into()),
        );
        assert_eq!(
            derived(&m).repo_input,
            crate::model::RepoInput::Default(Some(root("/src/api"))),
            "an unknown repo keeps the selected one"
        );
    }

    #[test]
    fn enter_in_the_checkout_form_submits_a_checkout_in_the_chosen_repo() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char('c')));
        type_text(&mut m, "fix-ci");
        let effects = update(&mut m, key(KeyCode::Enter));
        let Some(Effect::Job(Job::Launch { launch, ticket, .. })) = effects.first() else {
            panic!("launches");
        };
        assert_eq!(
            *launch,
            Launchable::Checkout {
                name: name("fix-ci").to_string().parse().expect("parses"),
                repo: crate::model::RepoChoice::Known(root("/src/api")),
                target: "fix-ci".parse().expect("parses"),
            }
        );
        assert!(matches!(form(&m).phase, Phase::Submitting { .. }));
        assert_eq!(m.launching, vec![(*ticket, name("fix-ci"))]);
        update(
            &mut m,
            Msg::Launched {
                ticket: *ticket,
                result: Err("branch fix-ci not found locally or on origin".into()),
            },
        );
        assert!(matches!(form(&m).phase, Phase::Editing));
        assert_eq!(
            form(&m).error.as_deref(),
            Some("branch fix-ci not found locally or on origin")
        );
    }

    #[test]
    fn a_checkout_that_reaches_another_worktree_is_followed_by_its_real_name() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char('c')));
        type_text(&mut m, "feature/x");
        let effects = update(&mut m, key(KeyCode::Enter));
        let Some(Effect::Job(Job::Launch { ticket, .. })) = effects.first() else {
            panic!("launches");
        };
        let ticket = *ticket;
        update(&mut m, key(KeyCode::Esc));
        assert!(matches!(m.follow, Some(Follow::Backgrounded { .. })));
        update(
            &mut m,
            Msg::Launched {
                ticket,
                result: Ok(Launch {
                    session: name("idle"),
                    spec: agent_core::process::LaunchSpec::new("zellij"),
                    delivery: agent_core::ops::Delivery::None,
                }),
            },
        );
        assert!(
            matches!(&m.follow, Some(Follow::Backgrounded { name: n, .. }) if *n == name("idle"))
        );
    }

    #[test]
    fn new_opens_on_the_selected_repo_and_derives_the_name() {
        let mut m = model(&[]);
        open_form(&mut m);
        assert_eq!(
            form(&m)
                .default_repo
                .as_ref()
                .map(|r| r.name().into_owned())
                .as_deref(),
            Some("api")
        );
        type_text(&mut m, "Fix the flaky login test");
        assert_eq!(
            form(&m).name,
            "",
            "the name is a suggestion, not typed text"
        );
        assert_eq!(derived(&m).name, "fix-flaky-login-test");
        assert!(matches!(&derived(&m).check, NameCheck::Free(_)));
    }

    #[test]
    fn derived_names_never_point_at_an_existing_agent() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "fix");
        assert_eq!(derived(&m).name, "fix-2");
        assert!(matches!(&derived(&m).check, NameCheck::Free(_)));
    }

    #[test]
    fn overlong_names_cant_be_submitted() {
        let mut m = model(&[]);
        open_form(&mut m);
        update(&mut m, key(KeyCode::Tab));
        type_text(&mut m, "a-name-that-is-far-too-long");
        assert!(matches!(
            &derived(&m).check,
            NameCheck::TooLong { bytes: 27 }
        ));
        assert!(update(&mut m, key(KeyCode::Enter)).is_empty());
        assert!(matches!(m.overlay, Overlay::New(_)));
    }

    #[test]
    fn typing_a_name_replaces_the_suggestion() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "add dark mode");
        update(&mut m, key(KeyCode::Tab));
        type_text(&mut m, "dm");
        assert_eq!(derived(&m).name, "dm");
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
        );
        assert_eq!(
            derived(&m).name,
            "add-dark-mode",
            "⌃R goes back to the suggestion"
        );
    }

    #[test]
    fn the_model_is_asked_once_typing_pauses_and_only_its_latest_answer_counts() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "why is the export slow");
        let Overlay::New(f) = &mut m.overlay else {
            panic!()
        };
        f.naming.state = NamingState::Due(Instant::now());
        let effects = tick(&mut m);
        let Some(generation) = effects.iter().find_map(|e| match e {
            Effect::Job(Job::SuggestName { generation, .. }) => Some(*generation),
            _ => None,
        }) else {
            panic!("no ask")
        };
        update(
            &mut m,
            Msg::NameSuggested {
                generation: generation - 1,
                name: "stale".parse().ok(),
            },
        );
        assert_eq!(
            derived(&m).suggestion.map(|s| s.source),
            Some(crate::model::NameSource::Words)
        );
        update(
            &mut m,
            Msg::NameSuggested {
                generation,
                name: "slow-export".parse().ok(),
            },
        );
        assert_eq!(derived(&m).name, "slow-export");
        type_text(&mut m, "!");
        assert_ne!(
            derived(&m).name,
            "slow-export",
            "a new prompt drops the old answer"
        );
    }

    #[test]
    fn linked_prompts_are_named_from_the_link_without_asking() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(
            &mut m,
            "look at https://linear.app/t/issue/ENG-7/broken-export please",
        );
        assert_eq!(derived(&m).name, "eng-7-broken-export");
        assert!(matches!(form(&m).naming.state, NamingState::Idle));
    }

    #[test]
    fn submitting_creates_in_the_chosen_repo_and_connects() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "add dark mode");
        let effects = update(&mut m, key(KeyCode::Enter));
        let [
            Effect::Job(Job::Launch {
                launch:
                    Launchable::Create {
                        name,
                        repo: crate::model::RepoChoice::Known(root),
                        task: Some(task),
                    },
                ticket,
                ..
            }),
        ] = effects.as_slice()
        else {
            panic!("expected a create job");
        };
        assert_eq!(name.as_str(), "add-dark-mode");
        assert_eq!(root.name(), "api");
        assert_eq!(task.as_str(), "add dark mode");
        assert!(
            matches!(&m.pane, Pane::Connecting { name, .. } if name.as_str() == "add-dark-mode")
        );
        let ticket = *ticket;
        update(
            &mut m,
            Msg::Launched {
                ticket,
                result: Err("boom".into()),
            },
        );
        assert_eq!(form(&m).error.as_deref(), Some("boom"));
        assert_eq!(
            form(&m).task,
            "add dark mode",
            "the text survives a failure"
        );
        assert!(matches!(form(&m).phase, Phase::Editing) && m.follow.is_none());
    }

    #[test]
    fn typed_names_attach_to_live_agents_with_the_task() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "rerun ci");
        update(&mut m, key(KeyCode::Tab));
        type_text(&mut m, "fix");
        assert!(matches!(&derived(&m).check, NameCheck::Live { .. }));
        let effects = update(&mut m, key(KeyCode::Enter));
        assert!(matches!(
            effects.as_slice(),
            [Effect::Job(Job::Launch {
                launch: Launchable::Attach { task: Some(_), .. },
                ..
            })]
        ));
    }

    #[test]
    fn idle_worktrees_start_with_the_task_and_main_checkouts_are_refused() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "go");
        update(&mut m, key(KeyCode::Tab));
        type_text(&mut m, "idle");
        assert!(matches!(&derived(&m).check, NameCheck::Idle { .. }));
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        );
        type_text(&mut m, "web");
        assert!(matches!(&derived(&m).check, NameCheck::MainCheckout { .. }));
        assert!(update(&mut m, key(KeyCode::Enter)).is_empty());
    }

    #[test]
    fn a_main_checkout_is_started_from_the_list_not_the_form() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "go");
        update(&mut m, key(KeyCode::Tab));
        type_text(&mut m, "api");
        assert!(matches!(derived(&m).check, NameCheck::MainCheckout { .. }));
        assert!(update(&mut m, key(KeyCode::Enter)).is_empty());
    }

    #[test]
    fn tab_completes_the_repo() {
        let mut m = model(&[]);
        open_form(&mut m);
        update(&mut m, key(KeyCode::BackTab));
        type_text(&mut m, "we");
        assert_eq!(derived(&m).completion.as_deref(), Some("b"));
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(form(&m).repo.text, "web");
        assert_eq!(
            form(&m).focus,
            Field::Repo,
            "completing doesn't leave the field"
        );
    }

    #[test]
    fn backgrounding_a_create_lets_the_pane_go_and_previews_it_once_it_shows() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "add dark mode");
        let effects = update(&mut m, key(KeyCode::Enter));
        let [Effect::Job(Job::Launch { ticket, .. })] = effects.as_slice() else {
            panic!()
        };
        let ticket = *ticket;
        update(&mut m, key(KeyCode::Esc));
        assert!(matches!(m.overlay, Overlay::None));
        assert_eq!(
            m.pane.awaiting(ticket),
            None,
            "navigating can't be undone by the late result"
        );
        assert!(
            matches!(&m.follow, Some(Follow::Backgrounded { name, .. }) if name.as_str() == "add-dark-mode")
        );
    }

    #[test]
    fn a_failed_create_keeps_other_follows() {
        let mut m = model(&[]);
        m.follow = Some(Follow::Restored(name("web")));
        let ticket = m.ticket();
        update(
            &mut m,
            Msg::Launched {
                ticket,
                result: Err("boom".into()),
            },
        );
        assert_eq!(m.follow, Some(Follow::Restored(name("web"))));
    }

    #[test]
    fn completion_survives_case_folding_that_changes_byte_lengths() {
        use crate::model::completes;
        assert_eq!(completes("kelvin", "\u{212A}E").as_deref(), Some("lvin"));
        assert_eq!(completes("api", "AP").as_deref(), Some("i"));
        assert_eq!(completes("api", "api"), None);
        assert_eq!(completes("api", "x"), None);
    }

    fn with_actions(m: &mut Model) {
        m.actions = r#"
            [[action]]
            key = "t"
            label = "tests"
            run = ["bin/test", "{name}", "{repo}"]

            [[action]]
            key = "s"
            label = "dev server"
            terminal_event = { name = "agent-spawn-dev", value = "{path}" }
        "#
        .parse::<agent_core::config::Config>()
        .expect("succeeds")
        .actions;
    }

    #[test]
    fn actions_run_on_the_selected_agent_with_its_details_as_arguments() {
        let mut m = model(&[]);
        with_actions(&mut m);
        update(&mut m, key(KeyCode::Char('x')));
        assert!(matches!(&m.overlay, Overlay::Actions(n) if n.as_str() == "fix"));
        let effects = update(&mut m, key(KeyCode::Char('t')));
        assert!(matches!(m.overlay, Overlay::None));
        let [Effect::Job(Job::Run { label, launch })] = effects.as_slice() else {
            panic!("expected a run")
        };
        assert_eq!(
            (label.as_str(), launch.program.to_str()),
            ("tests on fix", Some("bin/test"))
        );
        assert_eq!(launch.args, ["fix", "/src/api"]);
    }

    #[test]
    fn actions_needing_a_missing_checkout_are_refused() {
        let mut m = model(&[]);
        with_actions(&mut m);
        update(&mut m, key(KeyCode::Char('x')));
        assert!(update(&mut m, key(KeyCode::Char('s'))).is_empty());
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.kind == StatusKind::Error && s.text.contains("no {path}"))
        );
    }

    #[test]
    fn without_actions_x_says_where_to_add_them() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char('x')));
        assert!(matches!(m.overlay, Overlay::None));
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("config.toml"))
        );
    }

    #[test]
    fn kill_is_only_offered_for_running_sessions() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char('d')));
        assert!(
            matches!(&m.overlay, Overlay::Confirm(Confirm::Kill(r)) if r.name().as_str() == "fix")
        );
        update(&mut m, key(KeyCode::Esc));
        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(selected(&m).as_deref(), Some("api"));
        update(&mut m, key(KeyCode::Char('d')));
        assert!(matches!(m.overlay, Overlay::None));
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text == "api isn't running. D removes it")
        );
    }

    #[test]
    fn the_restored_selection_is_previewed_like_any_other() {
        let sessions = vec![
            SessionInfo {
                name: name("fix"),
                status: SessionStatus::Live {
                    cwd: Some("/w/api/fix".into()),
                },
            },
            SessionInfo {
                name: name("web"),
                status: SessionStatus::Live {
                    cwd: Some("/src/web".into()),
                },
            },
        ];
        let mut m = Model::new(
            "/elsewhere".into(),
            None,
            "claude".parse().expect("parses"),
            "claude".into(),
        );
        m.view = crate::model::View::Focus;
        m.visibility = Visibility::All;
        m.follow = Some(Follow::Restored(name("web")));
        update(
            &mut m,
            Msg::Snapshot(Ok((snapshot(sessions, &[]), Vec::new()))),
        );
        assert_eq!(selected(&m).as_deref(), Some("web"));
        m.preview_at = m.preview_at.map(|_| Instant::now());
        let effects = tick(&mut m);
        assert!(effects.iter().any(|e| matches!(e, Effect::Attach { name, intent: Intent::Preview, .. } if name.as_str() == "web")));
    }

    #[test]
    fn starting_inside_an_agent_beats_the_restored_selection() {
        let sessions = vec![
            SessionInfo {
                name: name("fix"),
                status: SessionStatus::Live {
                    cwd: Some("/w/api/fix".into()),
                },
            },
            SessionInfo {
                name: name("web"),
                status: SessionStatus::Live {
                    cwd: Some("/src/web".into()),
                },
            },
        ];
        let mut m = Model::new(
            "/w/api/fix/src".into(),
            None,
            "claude".parse().expect("parses"),
            "claude".into(),
        );
        m.view = crate::model::View::Focus;
        m.visibility = Visibility::All;
        m.follow = Some(Follow::Restored(name("web")));
        update(
            &mut m,
            Msg::Snapshot(Ok((snapshot(sessions, &[]), Vec::new()))),
        );
        assert_eq!(selected(&m).as_deref(), Some("fix"));
    }

    fn two_live() -> Model {
        let sessions = vec![
            SessionInfo {
                name: name("fix"),
                status: SessionStatus::Live {
                    cwd: Some("/w/api/fix".into()),
                },
            },
            SessionInfo {
                name: name("web"),
                status: SessionStatus::Live {
                    cwd: Some("/src/web".into()),
                },
            },
        ];
        let mut m = Model::new(
            "/elsewhere".into(),
            None,
            "claude".parse().expect("parses"),
            "claude".into(),
        );
        m.view = crate::model::View::Focus;
        m.visibility = Visibility::All;
        update(
            &mut m,
            Msg::Snapshot(Ok((snapshot(sessions, &[]), Vec::new()))),
        );
        m
    }

    #[test]
    fn every_checkout_is_inspected_and_again_soon_after_its_agent_reports() {
        let mut m = two_live();
        let inspected = |m: &mut Model| {
            let mut names: Vec<String> = tick(m)
                .into_iter()
                .filter_map(|e| match e {
                    Effect::Inspect { name, .. } => Some(name.to_string()),
                    _ => None,
                })
                .collect();
            names.sort();
            names
        };
        let earlier = |m: &mut Model, secs| {
            for last in m.inspections.values_mut() {
                last.at -= Duration::from_secs(secs);
            }
        };
        assert_eq!(inspected(&mut m), ["api", "fix", "idle", "web"]);
        assert_eq!(inspected(&mut m), [""; 0]);
        m.activities
            .get_or_insert_default()
            .insert(name("fix"), reported(agent_core::activity::State::Idle, 0));
        assert_eq!(
            inspected(&mut m),
            [""; 0],
            "not straight after the last read"
        );
        earlier(&mut m, 5);
        assert_eq!(inspected(&mut m), ["fix"]);
        earlier(&mut m, 31);
        assert_eq!(
            inspected(&mut m),
            ["api", "fix", "idle", "web"],
            "edits outside hooks are picked up"
        );
    }

    #[test]
    fn blocked_agents_come_before_finished_ones_and_lead_the_overview() {
        use agent_core::activity::State;
        let mut m = two_live();
        m.activities
            .get_or_insert_default()
            .insert(name("web"), reported(State::Done { summary: None }, 600));
        m.activities
            .get_or_insert_default()
            .insert(name("fix"), asking("rm?", 5));
        assert_eq!(
            m.needs_you(),
            [name("fix"), name("web")],
            "asking beats an older done"
        );
        assert_eq!(m.attention().title(), "◆1 ✓1 · agent");
        update(&mut m, key(KeyCode::Char('[')));
        let rows = m.rows();
        assert_eq!(rows[0], Row::Header(crate::model::Group::NeedsYou));
        assert_eq!(
            rows[1],
            Row::Grouped(crate::model::Group::NeedsYou, name("fix"))
        );
        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Char('j')));
        assert!(
            !m.rows()
                .iter()
                .any(|r| matches!(r, Row::Entry { name, .. } if name.as_str() == "fix")),
            "listed under Needs you, not again under its repo: {:?}",
            m.rows()
        );
    }

    #[test]
    fn the_selection_follows_an_agent_into_and_out_of_needs_you() {
        use agent_core::activity::State;
        let mut m = two_live();
        update(&mut m, key(KeyCode::Char('[')));
        m.set_cursor(Cursor::Entry(name("fix")));
        update(
            &mut m,
            Msg::Activities(HashMap::from([(name("fix"), asking("rm?", 5))])),
        );
        assert_eq!(
            m.cursor(),
            Some(&Cursor::Grouped(crate::model::Group::NeedsYou, name("fix")))
        );
        update(
            &mut m,
            Msg::Activities(HashMap::from([(name("fix"), reported(State::Idle, 0))])),
        );
        assert_eq!(m.cursor(), Some(&Cursor::Entry(name("fix"))));
    }

    #[test]
    fn tab_cycles_agents_repo_pinned_and_back() {
        let mut m = model(&["fix"]);
        m.view = View::Overview;
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.view, View::Focus);
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.view, View::Pinned);
        assert_eq!(
            m.cursor(),
            Some(&Cursor::Entry(name("fix"))),
            "the selected agent is pinned, so it stays selected"
        );
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.view, View::Overview);
        update(&mut m, key(KeyCode::Char('[')));
        assert_eq!(m.view, View::Pinned);
        update(&mut m, key(KeyCode::Char('[')));
        assert_eq!(m.view, View::Focus);
        update(&mut m, key(KeyCode::Char('[')));
        assert_eq!(m.view, View::Overview);
    }

    #[test]
    fn a_new_model_starts_on_the_agents_tab() {
        let m = Model::new(
            "/".into(),
            None,
            "claude".parse().expect("parses"),
            "claude".into(),
        );
        assert_eq!(m.view, View::Overview);
    }

    #[test]
    fn the_pinned_tab_lists_pins_then_the_other_agents_that_need_you() {
        let mut m = model(&["api"]);
        update(&mut m, key(KeyCode::Char(']')));
        let pin = Row::Entry {
            name: name("api"),
            place: crate::model::Place::Pinned,
        };
        assert_eq!(m.rows(), vec![pin.clone()]);
        assert_eq!(
            m.cursor(),
            Some(&Cursor::Entry(name("api"))),
            "fix isn't listed here, so the first pin is selected"
        );
        update(
            &mut m,
            Msg::Activities(HashMap::from([(name("fix"), asking("rm?", 5))])),
        );
        assert_eq!(
            m.rows(),
            vec![
                pin,
                Row::Header(crate::model::Group::NeedsYou),
                Row::Grouped(crate::model::Group::NeedsYou, name("fix")),
            ]
        );
        assert_eq!(
            m.extent().rows,
            5,
            "the Needs you header and all four entries, pinned or not"
        );
    }

    #[test]
    fn a_waiting_pin_stays_in_its_place_on_the_pinned_tab() {
        let mut m = model(&["fix"]);
        update(&mut m, key(KeyCode::Char(']')));
        update(
            &mut m,
            Msg::Activities(HashMap::from([(name("fix"), asking("rm?", 5))])),
        );
        assert_eq!(
            m.rows(),
            vec![Row::Entry {
                name: name("fix"),
                place: crate::model::Place::Pinned,
            }]
        );
        assert_eq!(m.cursor(), Some(&Cursor::Entry(name("fix"))));
    }

    #[test]
    fn an_empty_pinned_tab_selects_nothing_and_a_repo_pick_leaves_it() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.rows(), Vec::new());
        assert_eq!(m.cursor(), None, "nothing to select");
        update(&mut m, key(KeyCode::Char('p')));
        assert!(matches!(m.overlay, Overlay::Picker(_)));
        for c in "api".chars() {
            update(&mut m, key(KeyCode::Char(c)));
        }
        update(&mut m, key(KeyCode::Enter));
        assert_eq!(m.view, View::Focus, "picking a repo leaves the pinned tab");
        assert_eq!(m.cursor(), Some(&Cursor::Entry(name("api"))));
    }

    #[test]
    fn the_pinned_tab_falls_back_to_needs_you_when_nothing_is_pinned() {
        let mut m = model(&[]);
        update(
            &mut m,
            Msg::Activities(HashMap::from([(name("fix"), asking("rm?", 5))])),
        );
        m.set_cursor(Cursor::Entry(name("api")));
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.view, View::Pinned);
        assert_eq!(
            m.cursor(),
            Some(&Cursor::Grouped(crate::model::Group::NeedsYou, name("fix")))
        );
        assert!(m.items().contains(m.cursor().expect("selected")));
    }

    #[test]
    fn selecting_an_agent_the_pinned_tab_lacks_moves_to_the_agents_tab() {
        let mut m = model(&["api"]);
        update(&mut m, key(KeyCode::Char(']')));
        m.set_cursor(Cursor::Entry(name("fix")));
        assert_eq!(m.view, View::Overview);
        assert_eq!(m.cursor(), Some(&Cursor::Entry(name("fix"))));
        assert!(m.items().contains(m.cursor().expect("selected")));
        let snapshot = m.snapshot().expect("loaded").clone();
        update(&mut m, Msg::Snapshot(Ok((snapshot, Vec::new()))));
        assert_eq!(
            m.cursor(),
            Some(&Cursor::Entry(name("fix"))),
            "survives a refresh"
        );
    }

    #[test]
    fn a_full_tab_cycle_comes_back_to_the_agent_the_pinned_tab_lacked() {
        let mut m = model(&["api"]);
        assert_eq!(m.cursor(), Some(&Cursor::Entry(name("fix"))));
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.cursor(), Some(&Cursor::Entry(name("api"))));
        update(&mut m, key(KeyCode::Char(']')));
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.view, View::Focus);
        assert_eq!(m.cursor(), Some(&Cursor::Entry(name("fix"))));

        update(&mut m, key(KeyCode::Char('[')));
        m.set_cursor(Cursor::Entry(name("api")));
        update(&mut m, key(KeyCode::Char(']')));
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(
            m.cursor(),
            Some(&Cursor::Entry(name("api"))),
            "moving on the pinned tab is kept"
        );
    }

    #[test]
    fn the_pinned_tab_opens_on_the_first_pin_not_a_needs_you_row() {
        let mut m = model(&["web"]);
        update(
            &mut m,
            Msg::Activities(HashMap::from([(name("fix"), asking("rm?", 5))])),
        );
        m.set_cursor(Cursor::Entry(name("idle")));
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.view, View::Pinned);
        assert_eq!(m.cursor(), Some(&Cursor::Entry(name("web"))));
    }

    fn snapshot_with_tools(with_tools: bool) -> agent_core::model::Snapshot {
        let mut repos = vec![
            (
                root("/src/api"),
                RepoWorktrees {
                    main: wt("/src/api"),
                    linked: vec![wt("/w/api/fix"), wt("/w/api/idle")],
                },
            ),
            (
                root("/src/web"),
                RepoWorktrees {
                    main: wt("/src/web"),
                    linked: Vec::new(),
                },
            ),
        ];
        if with_tools {
            repos.push((
                root("/src/tools"),
                RepoWorktrees {
                    main: wt("/src/tools"),
                    linked: Vec::new(),
                },
            ));
        }
        let obs = Observation {
            sessions: vec![SessionInfo {
                name: name("fix"),
                status: SessionStatus::Live {
                    cwd: Some("/w/api/fix".into()),
                },
            }],
            repos,
            warnings: Vec::new(),
        };
        let pins = [name("api")];
        assemble(&obs, &Layout::new("/w".into()).expect("absolute"), &pins).0
    }

    #[test]
    fn a_displaced_agent_that_has_gone_does_not_drag_the_selection() {
        let mut m = Model::new(
            "/w/api/fix/src".into(),
            None,
            "claude".parse().expect("parses"),
            "claude".into(),
        );
        m.view = crate::model::View::Focus;
        m.visibility = Visibility::All;
        update(
            &mut m,
            Msg::Snapshot(Ok((snapshot_with_tools(true), Vec::new()))),
        );
        update(
            &mut m,
            Msg::Activities(HashMap::from([(name("fix"), asking("rm?", 5))])),
        );
        m.set_cursor(Cursor::Entry(name("tools")));
        update(&mut m, key(KeyCode::Char(']')));
        let pin = Cursor::Entry(name("api"));
        assert_eq!(
            m.cursor(),
            Some(&pin),
            "tools isn't pinned, so the first pin"
        );
        update(
            &mut m,
            Msg::Snapshot(Ok((snapshot_with_tools(false), Vec::new()))),
        );
        assert_eq!(m.cursor(), Some(&pin));
        update(&mut m, key(KeyCode::Char(']')));
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.view, View::Focus);
        assert_eq!(m.cursor(), Some(&pin), "the gone agent is forgotten");
    }

    #[test]
    fn a_goes_to_the_most_urgent_agent_and_says_when_nothing_waits() {
        use agent_core::activity::State;
        let mut m = two_live();
        update(&mut m, key(KeyCode::Char('a')));
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text == "Nothing needs you")
        );
        m.activities.get_or_insert_default().insert(
            name("web"),
            reported(
                State::Failed {
                    reason: "rate limit".into(),
                },
                30,
            ),
        );
        let effects = update(&mut m, key(KeyCode::Char('a')));
        assert!(
            matches!(effects.as_slice(), [Effect::Attach { name, intent: Intent::Activate, .. }] if name.as_str() == "web")
        );
    }

    #[test]
    fn a_long_turn_finishing_elsewhere_rings_once() {
        use agent_core::activity::State;
        let mut m = two_live();
        m.activities.get_or_insert_default().insert(
            name("fix"),
            reported(State::Working { tools: Vec::new() }, 120),
        );
        let mut next = m.activities.clone().unwrap_or_default();
        next.insert(name("fix"), reported(State::Done { summary: None }, 0));
        update(&mut m, Msg::Activities(next.clone()));
        assert!(tick(&mut m).iter().any(|e| matches!(e, Effect::Bell)));
        update(&mut m, Msg::Activities(next));
        assert!(
            !tick(&mut m).iter().any(|e| matches!(e, Effect::Bell)),
            "already done"
        );
    }

    fn filtered(pins: &[&str]) -> Model {
        let mut m = model(pins);
        m.visibility = Visibility::Active;
        m.revalidate();
        m
    }

    fn listed(m: &Model) -> Vec<String> {
        m.rows()
            .iter()
            .filter_map(|row| match row {
                Row::Entry { name, .. } => Some(name.to_string()),
                Row::Repo { key, .. } => Some(format!(
                    "repo {}",
                    key.root()
                        .map_or(String::new(), |r| r.path().display().to_string())
                )),
                Row::Grouped(..) | Row::Header(_) | Row::Elsewhere(_) => None,
            })
            .collect()
    }

    #[test]
    fn only_checkouts_with_a_session_are_listed_until_dot_shows_the_rest() {
        let mut m = filtered(&[]);
        assert_eq!(
            listed(&m),
            ["fix"],
            "the exited main and the idle worktree are hidden"
        );
        assert_eq!(m.hidden(), 2, "api and idle: what `.` adds to this tab");
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(
            selected(&m).as_deref(),
            Some("fix"),
            "nothing else to move to"
        );

        update(&mut m, key(KeyCode::Char('.')));
        assert_eq!(listed(&m), ["api", "fix", "idle"]);
        assert_eq!(m.hidden(), 0);
        update(&mut m, key(KeyCode::Char('.')));
        assert_eq!(listed(&m), ["fix"]);
    }

    #[test]
    fn the_selection_stays_listed_until_it_moves_on() {
        let mut m = filtered(&[]);
        m.set_cursor(Cursor::Entry(name("idle")));
        assert_eq!(listed(&m), ["fix", "idle"]);
        assert_eq!(m.hidden(), 1, "api; the selected idle is listed");
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(selected(&m).as_deref(), Some("fix"));
        assert_eq!(
            listed(&m),
            ["fix"],
            "it goes once something else is selected"
        );
    }

    #[test]
    fn overview_drops_repos_with_nothing_listed_and_the_pinned_tab_keeps_every_pin() {
        let mut m = filtered(&["web"]);
        update(&mut m, key(KeyCode::Char('[')));
        assert_eq!(m.view, View::Overview);
        assert_eq!(listed(&m), ["repo /src/api", "fix"], "web has no session");
        update(&mut m, key(KeyCode::Char('[')));
        assert_eq!(m.view, View::Pinned);
        assert_eq!(listed(&m), ["web"], "pins show whatever their state");
        assert_eq!(m.hidden(), 0, "the filter doesn't apply here");
    }

    #[test]
    fn repo_jumps_skip_repos_with_nothing_running() {
        let mut m = filtered(&[]);
        update(&mut m, key(KeyCode::Char('}')));
        assert_eq!(selected(&m).as_deref(), Some("fix"));
        update(&mut m, key(KeyCode::Char('.')));
        update(&mut m, key(KeyCode::Char('}')));
        assert_eq!(selected(&m).as_deref(), Some("web"));
    }

    #[test]
    fn the_switcher_still_finds_hidden_checkouts_and_selecting_one_lists_it() {
        let mut m = filtered(&[]);
        let found = switcher_matches(&m, "web");
        assert_eq!(found, [name("web")]);
        assert!(switcher_matches(&m, "idle").contains(&name("idle")));
        m.set_cursor(Cursor::Entry(name("web")));
        assert_eq!(selected(&m).as_deref(), Some("web"));
        assert_eq!(listed(&m), ["web"]);
    }

    fn delegating() -> Model {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m
    }

    fn reads(effects: Vec<Effect>) -> Vec<String> {
        effects
            .into_iter()
            .filter_map(|e| match e {
                Effect::ReadSubagent { id, workdir, .. } => {
                    Some(format!("{id:?} {}", workdir.display()))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn focusing_the_turn_puts_a_cursor_on_its_rows_and_reads_the_subagent_it_is_on() {
        let mut m = delegating();
        assert_eq!(m.subagent_rows().len(), 3);
        assert!(
            reads(tick(&mut m)).is_empty(),
            "nothing is read while [2] is unfocused"
        );

        update(&mut m, key(KeyCode::Char('2')));
        assert_eq!(m.row_at(), 0, "the turn row first");
        assert!(
            reads(tick(&mut m)).is_empty(),
            "the turn needs no transcript"
        );
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(
            reads(tick(&mut m)),
            ["AgentId(\"s2\") /w/heucristic0/obs-layout-4"],
            "the oldest first, as the tree lists them"
        );
        assert!(reads(tick(&mut m)).is_empty(), "once");

        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(m.row_at(), 2);
        assert_eq!(
            reads(tick(&mut m)).len(),
            1,
            "the newly selected one is read at once"
        );
        update(&mut m, key(KeyCode::Char('G')));
        assert_eq!(m.row_at(), 3);
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(m.row_at(), 3, "stops at the end");
        update(&mut m, key(KeyCode::Char('g')));
        assert_eq!(m.row_at(), 0);
        update(&mut m, key(KeyCode::Char('k')));
        assert_eq!(m.row_at(), 0);
        assert_eq!(
            m.panel,
            Panel::Agent { offset: 0 },
            "j moves the cursor, not a scroll"
        );
    }

    #[test]
    fn a_subagent_under_the_cursor_stays_listed_after_a_prompt_clears_it_until_the_cursor_leaves() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        let (agent, id) = m.subagent_selected().expect("a subagent is selected");
        assert_eq!(reads(tick(&mut m)).len(), 1);

        let mut cleared = m.activities.clone().expect("reported");
        cleared
            .get_mut(&agent)
            .expect("has activity")
            .subagents
            .remove(&id);
        update(&mut m, Msg::Activities(cleared));
        assert_eq!(m.subagent_rows().len(), 3, "the hovered row stays");
        assert_eq!(m.subagent_selected(), Some((agent.clone(), id.clone())));
        assert!(m.subagent_reads.contains_key(&(agent.clone(), id.clone())));
        assert!(matches!(
            m.hover(),
            Some(crate::document::Hover::Subagent(_, ref held)) if *held == id
        ));

        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(m.subagent_rows().len(), 2, "gone once the cursor moved on");
        assert!(m.held_subagent.is_some_and(|held| held.id != id));
        assert!(!m.subagent_reads.contains_key(&(agent, id)));
    }

    #[test]
    fn a_subagent_is_read_again_soon_after_its_agent_reports_but_not_every_tick() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(reads(tick(&mut m)).len(), 1);
        let (name, id) = m.subagent_selected().expect("a subagent is selected");

        let mut more = m.activities.clone().expect("reported");
        more.get_mut(&name).expect("has activity").events += 1;
        update(&mut m, Msg::Activities(more));
        assert!(
            reads(tick(&mut m)).is_empty(),
            "within the refresh interval"
        );
        for last in m.subagent_reads.values_mut() {
            last.at -= SUBAGENT_REFRESH;
        }
        assert_eq!(reads(tick(&mut m)).len(), 1, "after it");

        let detail = agent_core::ports::SubagentDetail {
            prompt: "do it".into(),
            ..Default::default()
        };
        update(
            &mut m,
            Msg::Subagent {
                name: name.clone(),
                id: id.clone(),
                detail: Ok(Some(detail.clone())),
            },
        );
        assert_eq!(
            m.subagent_logs.get(&(name.clone(), id.clone())),
            Some(&Ok(Some(detail.clone())))
        );

        let mut gone = m.activities.clone().expect("reported");
        gone.get_mut(&name).expect("has activity").subagents.clear();
        update(&mut m, Msg::Activities(gone));
        update(&mut m, key(KeyCode::Char('g')));
        assert!(m.subagent_logs.is_empty() && m.subagent_reads.is_empty());
        update(
            &mut m,
            Msg::Subagent {
                name,
                id,
                detail: Ok(Some(detail)),
            },
        );
        assert!(
            m.subagent_logs.is_empty(),
            "a late answer for an unwatched subagent is dropped"
        );
    }

    #[test]
    fn with_only_a_turn_there_is_one_row_and_zero_or_enter_open_its_document() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("rate-limit-1")));
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(m.row_at(), 0, "nowhere to go");
        assert!(matches!(m.hover(), Some(crate::document::Hover::Turn(_))));
        assert_eq!(reads(tick(&mut m)), [""; 0]);
        update(&mut m, key(KeyCode::Enter));
        assert!(m.main_focused());
    }

    #[test]
    fn j_and_k_in_the_turn_pick_what_zero_opens() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        assert!(matches!(m.hover(), Some(crate::document::Hover::Turn(_))));
        update(&mut m, key(KeyCode::Char('j')));
        assert!(matches!(
            m.hover(),
            Some(crate::document::Hover::Subagent(..))
        ));
        update(&mut m, key(KeyCode::Char('J')));
        assert_eq!(m.docs.scroll, 2, "J scrolls [0] from [2] too");
        update(&mut m, key(KeyCode::Char('0')));
        assert!(m.main_focused());
        update(&mut m, key(KeyCode::Esc));
        assert!(matches!(m.panel, Panel::Agent { .. }), "back to [2]");
    }

    #[test]
    fn an_agent_with_no_report_has_no_turn_to_focus() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("payroll-fix-11")));
        let live_without_report = m
            .selected_entry()
            .is_some_and(|e| m.activity(e.name()).is_none());
        if live_without_report {
            update(&mut m, key(KeyCode::Char('2')));
            assert_eq!(m.panel, Panel::Agents);
            assert!(
                m.status
                    .as_ref()
                    .is_some_and(|s| s.text.contains("no turn recorded"))
            );
        }
    }

    fn diff_reads(effects: Vec<Effect>) -> Vec<String> {
        effects
            .into_iter()
            .filter_map(|e| match e {
                Effect::ReadDocument {
                    key: Key::Diff(key),
                    worktree,
                    ..
                } => Some(format!("{} {}", key.path.display(), worktree.display())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_diff_under_the_changes_cursor_is_read_once_the_cursor_rests_and_scrolls_with_j_k_and_ctrl_d()
     {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        assert_eq!(diff_reads(tick(&mut m)), [""; 0], "[3] isn't focused");
        update(&mut m, key(KeyCode::Char('3')));
        assert!(matches!(m.panel, Panel::Changes { file: 0 }));
        assert_eq!(
            diff_reads(tick(&mut m)),
            [""; 0],
            "the cursor hasn't rested"
        );
        std::thread::sleep(crate::document::REST + Duration::from_millis(20));
        assert_eq!(
            diff_reads(tick(&mut m)),
            ["src/triage/flow.ts /w/heucristic0/obs-layout-4"]
        );
        assert_eq!(diff_reads(tick(&mut m)), [""; 0], "asked once");

        m.docs.page = 10;
        update(&mut m, key(KeyCode::Char('J')));
        assert_eq!(m.docs.scroll, 2);
        update(&mut m, key(KeyCode::Char('K')));
        update(&mut m, key(KeyCode::Char('K')));
        assert_eq!(m.docs.scroll, 0);
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
        );
        assert_eq!(m.docs.scroll, 5, "half a page of [0], not of the list");
        assert!(matches!(m.panel, Panel::Changes { file: 0 }));
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        );
        assert_eq!(m.docs.scroll, 0);

        update(&mut m, key(KeyCode::Char('j')));
        m.settle_main();
        assert_eq!(m.docs.scroll, 0, "another file starts at the top");
    }

    #[test]
    fn j_and_k_keep_moving_the_list_and_scroll_nothing_from_the_list() {
        let mut m = crate::fixtures::fleet(10);
        m.docs.page = 10;
        update(&mut m, key(KeyCode::Char('J')));
        assert_eq!(m.docs.scroll, 0, "the terminal doesn't scroll from NAV");
    }

    fn on_changes(name_of: &str) -> Model {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name(name_of)));
        update(&mut m, key(KeyCode::Char('3')));
        m.docs.page = 10;
        m
    }

    fn edits(effects: &[Effect]) -> usize {
        effects
            .iter()
            .filter(|e| matches!(e, Effect::Edit(_)))
            .count()
    }

    #[test]
    fn enter_in_changes_focuses_the_document_and_escape_goes_back_one_level_at_a_time() {
        let mut m = on_changes("obs-layout-4");
        assert!(!m.main_focused());
        let effects = update(&mut m, key(KeyCode::Enter));
        assert_eq!(edits(&effects), 0, "enter no longer opens the editor");
        assert!(m.main_focused());

        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(
            m.docs.scroll, 2,
            "j scrolls the document, not the file list"
        );
        assert_eq!(m.panel.listed(), Panel::Changes { file: 0 });
        update(&mut m, key(KeyCode::Char('G')));
        assert_eq!(m.docs.scroll, usize::MAX);
        update(&mut m, key(KeyCode::Char('g')));
        assert_eq!(m.docs.scroll, 0);
        update(&mut m, key(KeyCode::PageDown));
        assert_eq!(m.docs.scroll, 10, "a page of [0]");

        update(&mut m, key(KeyCode::Esc));
        assert!(!m.main_focused());
        assert!(matches!(m.panel, Panel::Changes { file: 0 }), "back to [3]");
        update(&mut m, key(KeyCode::Char('j')));
        assert!(
            matches!(m.panel, Panel::Changes { file: 1 }),
            "j moves the files again"
        );
        update(&mut m, key(KeyCode::Esc));
        assert_eq!(m.panel, Panel::Agents, "then to [1]");
    }

    #[test]
    fn zero_focuses_the_document_from_changes_and_one_gives_the_terminal_back() {
        let mut m = on_changes("obs-layout-4");
        update(&mut m, key(KeyCode::Char('0')));
        assert!(m.main_focused());
        update(&mut m, key(KeyCode::Char('0')));
        assert!(m.main_focused(), "nothing more to focus");
        update(&mut m, key(KeyCode::Char('1')));
        assert!(!m.main_focused());
        assert_eq!(m.panel, Panel::Agents);
        assert!(m.terminal_visible());

        let mut m = on_changes("obs-layout-4");
        update(&mut m, key(KeyCode::Enter));
        update(&mut m, key(KeyCode::Char('3')));
        assert!(!m.main_focused(), "choosing a panel leaves the document");
        assert!(matches!(m.panel, Panel::Changes { .. }));
    }

    #[test]
    fn e_still_opens_the_file_from_changes_and_from_its_document() {
        let mut m = on_changes("obs-layout-4");
        assert_eq!(edits(&update(&mut m, key(KeyCode::Char('e')))), 1);
        update(&mut m, key(KeyCode::Enter));
        assert_eq!(edits(&update(&mut m, key(KeyCode::Char('e')))), 1);
    }

    #[test]
    fn i_types_to_the_selected_agent_from_anywhere_and_keeps_the_place_it_left() {
        let mut m = on_changes("obs-layout-4");
        update(&mut m, key(KeyCode::Enter));
        let effects = update(&mut m, key(KeyCode::Char('i')));
        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::Attach {
                    intent: Intent::Activate,
                    ..
                }
            )),
            "i goes to AGENT on the selected agent"
        );
        assert!(
            m.main_focused(),
            "and the diff is where escape from AGENT returns to"
        );

        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        let effects = update(&mut m, key(KeyCode::Char('0')));
        assert!(
            effects.iter().any(|e| matches!(e, Effect::Attach { .. })),
            "0 in [1] is AGENT"
        );
    }

    #[test]
    fn an_agents_terminal_counts_as_in_the_pane_only_while_it_shows() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        m.pane = Pane::Connecting {
            name: name("obs-layout-4"),
            ticket: m.ticket(),
        };
        assert_eq!(m.visible_session(), Some(&name("obs-layout-4")));
        update(&mut m, key(KeyCode::Char('3')));
        assert_eq!(m.visible_session(), None, "its diff is on screen instead");
    }

    #[test]
    fn the_readiness_row_is_the_first_row_of_changes_and_reads_the_branch() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        assert!(!m.branch_row(), "Uncommitted lists only its files");
        update(&mut m, key(KeyCode::Char('3')));
        assert!(matches!(m.hover(), Some(crate::document::Hover::Diff(_))));
        update(&mut m, key(KeyCode::Char(']')));
        assert!(m.branch_row(), "obs-layout-4 is ahead in the fixture");
        update(&mut m, key(KeyCode::Char('g')));
        assert!(matches!(m.hover(), Some(crate::document::Hover::Branch(_))));
        assert_eq!(m.change_rows(), m.changes().len() + 1);
        assert_eq!(
            tick(&mut m)
                .iter()
                .filter(|e| matches!(e, Effect::ReadDocument { .. }))
                .count(),
            0
        );
        std::thread::sleep(crate::document::REST + Duration::from_millis(20));
        let asked: Vec<_> = tick(&mut m)
            .into_iter()
            .filter(|e| {
                matches!(
                    e,
                    Effect::ReadDocument {
                        key: Key::Branch(_),
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(asked.len(), 1, "the branch log is read like a diff");

        let effects = update(&mut m, key(KeyCode::Char('e')));
        assert!(
            matches!(effects.as_slice(), [Effect::Edit(Open::Checkout(_))]),
            "with no file under the cursor e opens the worktree"
        );
        update(&mut m, key(KeyCode::Char('j')));
        assert!(matches!(m.hover(), Some(crate::document::Hover::Diff(_))));
        update(&mut m, key(KeyCode::Char('G')));
        assert!(matches!(m.panel, Panel::Changes { file } if file == m.change_rows() - 1));
    }

    #[test]
    fn plus_and_underscore_switch_the_screen_mode_and_the_panels_keep_the_keys() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        update(&mut m, key(KeyCode::Char('3')));
        update(&mut m, key(KeyCode::Char('+')));
        assert_eq!(m.screen, Screen::Full);
        assert!(matches!(m.panel, Panel::Changes { .. }), "the focus stays");
        update(&mut m, key(KeyCode::Char('j')));
        assert!(matches!(m.panel, Panel::Changes { file: 1 }));
        update(&mut m, key(KeyCode::Char('_')));
        assert_eq!(m.screen, Screen::Split);
    }

    fn set_state(m: &mut Model, agent: &str, state: agent_core::activity::State, events: u64) {
        let mut activity = reported(state, 5);
        activity.events = events;
        m.activities
            .get_or_insert_default()
            .insert(name(agent), activity);
    }

    fn working() -> agent_core::activity::State {
        agent_core::activity::State::Working { tools: Vec::new() }
    }

    fn done() -> agent_core::activity::State {
        agent_core::activity::State::Done { summary: None }
    }

    fn replies(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Job(Job::Reply { prompt, .. }) => Some(prompt.as_str().to_string()),
                _ => None,
            })
            .collect()
    }

    fn talk_to(m: &mut Model, agent: &str, text: &str) -> Vec<Effect> {
        m.set_cursor(Cursor::Entry(name(agent)));
        update(m, key(KeyCode::Char('s')));
        assert!(matches!(m.overlay, Overlay::Talk { .. }), "the box opens");
        type_text(m, text);
        update(m, key(KeyCode::Enter))
    }

    fn sent(m: &mut Model, agent: &str) -> Vec<Effect> {
        update(
            m,
            Msg::Sent {
                to: name(agent),
                result: Ok(()),
            },
        )
    }

    #[test]
    fn t_opens_the_message_box_from_every_panel_whatever_the_agent_is_doing() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
        set_state(&mut m, "auth-refresh-2", working(), 1);
        for panel in [None, Some('2'), Some('3')] {
            if let Some(panel) = panel {
                update(&mut m, key(KeyCode::Char(panel)));
            }
            update(&mut m, key(KeyCode::Char('s')));
            assert!(
                matches!(&m.overlay, Overlay::Talk { to, .. } if to.as_str() == "auth-refresh-2"),
                "{:?}",
                m.panel
            );
            update(&mut m, key(KeyCode::Esc));
        }
        update(&mut m, key(KeyCode::Char('r')));
        assert!(matches!(m.overlay, Overlay::None), "r is free");
    }

    #[test]
    fn t_refuses_agents_that_report_nothing_or_are_not_running() {
        let mut m = two_live();
        m.set_cursor(Cursor::Entry(name("fix")));
        update(&mut m, key(KeyCode::Char('s')));
        assert!(matches!(m.overlay, Overlay::None));
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("reports no activity")),
            "says why"
        );
        let mut m = model(&[]);
        m.set_cursor(Cursor::Entry(name("api")));
        update(&mut m, key(KeyCode::Char('s')));
        assert!(matches!(m.overlay, Overlay::None), "exited");
    }

    #[test]
    fn a_message_to_an_agent_at_rest_is_sent_now() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, "auth-refresh-2", done(), 1);
        let effects = talk_to(&mut m, "auth-refresh-2", "rebase");
        assert_eq!(replies(&effects), ["rebase"]);
        assert!(matches!(m.overlay, Overlay::None));
    }

    #[test]
    fn a_message_to_a_working_agent_is_queued_and_sent_only_once_it_is_at_rest() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, "auth-refresh-2", working(), 1);
        let effects = talk_to(&mut m, "auth-refresh-2", "rebase");
        assert!(effects.is_empty(), "nothing is typed while it works");
        assert_eq!(m.outboxes[&name("auth-refresh-2")].len(), 1);
        assert_eq!(replies(&tick(&mut m)).len(), 0);

        let mut asking_now = crate::fixtures::asking("rm?", 1);
        asking_now.events = 2;
        let effects = update(
            &mut m,
            Msg::Activities(HashMap::from([(name("auth-refresh-2"), asking_now)])),
        );
        assert!(replies(&effects).is_empty(), "a prompt is open");

        let mut finished = reported(done(), 0);
        finished.events = 3;
        let effects = update(
            &mut m,
            Msg::Activities(HashMap::from([(name("auth-refresh-2"), finished)])),
        );
        assert_eq!(replies(&effects), ["rebase"], "the first rest sends it");
        assert!(
            replies(&tick(&mut m)).is_empty(),
            "not again while it is being typed"
        );
    }

    #[test]
    fn queued_messages_go_in_order_one_per_rest_and_busy_keeps_them() {
        let mut m = crate::fixtures::fleet(10);
        let agent = "auth-refresh-2";
        set_state(&mut m, agent, working(), 1);
        talk_to(&mut m, agent, "first");
        talk_to(&mut m, agent, "second");
        let queue = |m: &Model| -> Vec<String> {
            m.outboxes[&name(agent)]
                .iter()
                .map(|p| p.as_str().to_string())
                .collect()
        };
        assert_eq!(queue(&m), ["first", "second"]);

        set_state(&mut m, agent, done(), 2);
        let all = m.activities.clone().expect("reported");
        assert_eq!(replies(&update(&mut m, Msg::Activities(all))), ["first"]);

        let effects = update(
            &mut m,
            Msg::Sent {
                to: name(agent),
                result: Err(crate::worker::SendError::Busy),
            },
        );
        assert!(
            replies(&effects).is_empty(),
            "held until the record changes"
        );
        assert_eq!(queue(&m), ["first", "second"]);
        set_state(&mut m, agent, done(), 5);
        let all = m.activities.clone().expect("reported");
        assert_eq!(replies(&update(&mut m, Msg::Activities(all))), ["first"]);

        let effects = sent(&mut m, agent);
        assert!(
            replies(&effects).is_empty(),
            "the next waits for the agent to report again"
        );
        assert_eq!(queue(&m), ["second"]);

        set_state(&mut m, agent, working(), 3);
        set_state(&mut m, agent, done(), 4);
        let all = m.activities.clone().expect("reported");
        assert_eq!(replies(&update(&mut m, Msg::Activities(all))), ["second"]);
        sent(&mut m, agent);
        assert!(m.outboxes.is_empty());
    }

    fn status(m: &Model) -> String {
        m.status
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default()
    }

    #[test]
    fn enter_in_the_box_always_says_what_happened() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        talk_to(&mut m, AGENT, "rebase");
        assert_eq!(status(&m), format!("sent to {AGENT}"));

        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, working(), 1);
        talk_to(&mut m, AGENT, "rebase");
        assert_eq!(
            status(&m),
            format!("queued for {AGENT} · sends when it's at rest")
        );

        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        let effects = talk_to(&mut m, AGENT, "");
        assert!(effects.is_empty());
        assert!(matches!(m.overlay, Overlay::Talk { .. }), "the box stays");
        assert!(status(&m).contains("type a message"));
    }

    #[test]
    fn pasted_text_lands_in_the_message_box() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        m.set_cursor(Cursor::Entry(name(AGENT)));
        update(&mut m, key(KeyCode::Char('s')));
        update(&mut m, Msg::Paste("rebase\r\nonto main".into()));
        let effects = update(&mut m, key(KeyCode::Enter));
        assert_eq!(replies(&effects), ["rebase\nonto main"]);
    }

    #[test]
    fn a_launchless_record_stays_queued_until_a_new_hook_with_no_lapse() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        live_since(&mut m, 2);
        assert_eq!(
            replies(&talk_to(&mut m, AGENT, "rebase")),
            Vec::<String>::new()
        );
        assert!(m.awaiting_report(&name(AGENT)));
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("waiting for"))
        );
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(replies(&tick(&mut m)), Vec::<String>::new());
        assert_eq!(replies(&report(&mut m, reported(done(), 0))), ["rebase"]);
    }

    #[test]
    fn a_quiet_agent_launched_before_the_tui_saw_it_live_sends_at_rest() {
        let mut m = crate::fixtures::fleet(10);
        let mut a = reported(done(), 20);
        a.session = crate::fixtures::launch_reported(Duration::from_secs(25));
        report(&mut m, a);
        live_since(&mut m, 20);
        assert_eq!(replies(&talk_to(&mut m, AGENT, "rebase")), ["rebase"]);
    }

    #[test]
    fn a_booting_record_never_sends_within_the_quiet_time() {
        let mut m = crate::fixtures::fleet(10);
        let mut a = reported(done(), 1);
        a.session = crate::fixtures::launched(Duration::from_secs(1));
        report(&mut m, a);
        live_since(&mut m, 30);
        assert_eq!(
            replies(&talk_to(&mut m, AGENT, "rebase")),
            Vec::<String>::new()
        );
        assert_eq!(replies(&tick(&mut m)), Vec::<String>::new());
    }

    #[test]
    fn a_failed_send_drops_that_message_and_says_which() {
        let mut m = crate::fixtures::fleet(10);
        let agent = "auth-refresh-2";
        set_state(&mut m, agent, done(), 1);
        talk_to(&mut m, agent, "rebase onto main");
        update(
            &mut m,
            Msg::Sent {
                to: name(agent),
                result: Err(crate::worker::SendError::Failed("no pty".into())),
            },
        );
        assert!(m.outboxes.is_empty());
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("no pty") && s.text.contains("rebase onto main"))
        );
    }

    fn drafted(path: &str, text: &str) -> crate::review::Comment {
        let line = crate::fixtures::line_info(path, text, 1);
        crate::review::Comment::on(&[&line], None, "Fix.").expect("a comment")
    }

    #[test]
    fn a_review_of_a_crlf_file_queues_and_a_control_character_gets_its_own_message() {
        let mut m = crate::fixtures::fleet(10);
        let agent = "auth-refresh-2";
        set_state(&mut m, agent, done(), 1);
        m.drafts
            .insert(name(agent), vec![drafted("a.rs", "+let x = 1;\r")]);
        talk_to(&mut m, agent, "");
        assert_eq!(m.outboxes[&name(agent)].len(), 1);
        assert!(
            !m.outboxes[&name(agent)]
                .front()
                .expect("queued")
                .as_str()
                .contains('\r')
        );

        talk_to(&mut m, "obs-layout-4", "a\x1b[201~b");
        assert!(matches!(m.overlay, Overlay::Talk { .. }));
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("control character")),
            "{:?}",
            m.status.as_ref().map(|s| &s.text)
        );
        update(&mut m, key(KeyCode::Esc));
        talk_to(&mut m, "obs-layout-4", "  ");
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("type a message first"))
        );
    }

    #[test]
    fn a_dismissed_empty_comment_leaves_no_draft_behind() {
        let mut m = crate::fixtures::fleet(10);
        let agent = name("auth-refresh-2");
        let line = crate::fixtures::line_info("a.rs", "+x", 1);
        add_comment(&mut m, &agent, &[line], "", None);
        assert!(m.drafts.is_empty());
    }

    #[test]
    fn a_comment_with_a_control_character_is_refused_and_keeps_its_box() {
        let mut m = crate::fixtures::fleet(10);
        let agent = name("auth-refresh-2");
        let line = crate::fixtures::line_info("a.rs", "+x", 1);
        add_comment(&mut m, &agent, &[line], "a\x1bb", Some("abc1234".into()));
        assert!(m.drafts.is_empty());
        assert!(
            matches!(&m.overlay, Overlay::Comment { text, rev, .. }
                if text == "a\x1bb" && rev.as_deref() == Some("abc1234")),
            "the box stays with its commit"
        );
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("control character"))
        );
    }

    #[test]
    fn restored_comments_keep_a_newer_one_on_the_same_line() {
        let mut m = crate::fixtures::fleet(10);
        let agent = name("auth-refresh-2");
        m.drafts.insert(agent.clone(), vec![drafted("a.rs", "+x")]);
        assert_eq!(
            restore_comments(&mut m, &agent, vec![drafted("a.rs", "+x")]),
            1
        );
        assert_eq!(m.drafts[&agent].len(), 2);
    }

    #[test]
    fn drafts_go_with_their_agent() {
        let mut m = crate::fixtures::fleet(10);
        m.drafts
            .insert(name("auth-refresh-2"), vec![drafted("a.rs", "+x")]);
        m.drafts
            .insert(name("removed-agent"), vec![drafted("a.rs", "+x")]);
        forget_gone(&mut m);
        assert!(m.drafts.contains_key(&name("auth-refresh-2")));
        assert!(!m.drafts.contains_key(&name("removed-agent")));
    }

    #[test]
    fn a_review_that_fails_to_send_gives_its_comments_back_and_one_that_sends_does_not() {
        let mut m = crate::fixtures::fleet(10);
        let agent = "auth-refresh-2";
        set_state(&mut m, agent, done(), 1);
        m.drafts.insert(name(agent), vec![drafted("a.rs", "+x")]);
        talk_to(&mut m, agent, "");
        assert!(m.drafts.is_empty(), "queued, so not in the draft");
        m.drafts.insert(name(agent), vec![drafted("b.rs", "+y")]);
        update(
            &mut m,
            Msg::Sent {
                to: name(agent),
                result: Err(crate::worker::SendError::Failed("no pty".into())),
            },
        );
        let paths: Vec<_> = m.drafts[&name(agent)]
            .iter()
            .map(|c| c.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, ["a.rs", "b.rs"], "back ahead of newer comments");
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("back in the draft"))
        );

        m.drafts.clear();
        m.drafts.insert(name(agent), vec![drafted("a.rs", "+x")]);
        talk_to(&mut m, agent, "");
        sent(&mut m, agent);
        assert!(m.drafts.is_empty(), "delivered comments stay gone");
    }

    const AGENT: &str = "auth-refresh-2";

    fn report(m: &mut Model, mut activity: Activity) -> Vec<Effect> {
        activity.events = m.activity(&name(AGENT)).map_or(0, |a| a.events) + 1;
        update(m, Msg::Activities(HashMap::from([(name(AGENT), activity)])))
    }

    fn live_since(m: &mut Model, secs_ago: u64) {
        m.live_since.insert(
            name(AGENT),
            Some(SystemTime::now() - Duration::from_secs(secs_ago)),
        );
    }

    #[test]
    fn a_record_from_before_a_restart_never_gets_typed_into() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        live_since(&mut m, 2);
        let effects = talk_to(&mut m, AGENT, "rebase");
        assert!(
            replies(&effects).is_empty(),
            "the record is 5s old, the session 2s"
        );
        assert_eq!(replies(&tick(&mut m)), Vec::<String>::new());

        let effects = report(&mut m, reported(done(), 0));
        let [Effect::Job(Job::Reply { not_before, .. })] = effects.as_slice() else {
            panic!("a report after the start sends it");
        };
        assert_eq!(
            *not_before,
            m.not_before(&name(AGENT)),
            "the worker gets the bound"
        );
        assert!(not_before.is_some());
    }

    #[test]
    fn a_session_resurrected_by_this_tui_waits_for_a_report_after_it() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        m.launched_at.insert(name(AGENT), SystemTime::now());
        assert_eq!(
            replies(&talk_to(&mut m, AGENT, "rebase")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_fresh_session_start_gets_quiet_time_for_a_startup_dialog() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, working(), 1);
        live_since(&mut m, 30);
        talk_to(&mut m, AGENT, "rebase");
        let mut booted = reported(done(), 1);
        booted.session = crate::fixtures::booting();
        assert!(
            replies(&report(&mut m, booted)).is_empty(),
            "1s after SessionStart"
        );
        let mut quiet = reported(done(), 6);
        quiet.session = crate::fixtures::booting();
        assert_eq!(replies(&report(&mut m, quiet)), ["rebase"]);
    }

    #[test]
    fn busy_holds_the_queue_until_the_record_changes() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        talk_to(&mut m, AGENT, "rebase");
        let busy = || Msg::Sent {
            to: name(AGENT),
            result: Err(SendError::Busy),
        };
        assert!(replies(&update(&mut m, busy())).is_empty(), "no hot loop");
        assert_eq!(replies(&tick(&mut m)), Vec::<String>::new());
        let same = m.activities.clone().expect("reported");
        assert!(
            replies(&update(&mut m, Msg::Activities(same))).is_empty(),
            "unchanged"
        );
        let effects = report(&mut m, reported(done(), 0));
        assert_eq!(replies(&effects), ["rebase"], "a changed record retries");
    }

    #[test]
    fn an_unreadable_store_keeps_the_message_and_retries() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        talk_to(&mut m, AGENT, "rebase");
        let effects = update(
            &mut m,
            Msg::Sent {
                to: name(AGENT),
                result: Err(SendError::Unreadable("io".into())),
            },
        );
        assert_eq!(replies(&effects), Vec::<String>::new());
        assert_eq!(m.outboxes[&name(AGENT)].len(), 1, "still queued");
        let same = m.activities.clone().expect("reported");
        assert_eq!(replies(&update(&mut m, Msg::Activities(same))), ["rebase"]);
    }

    #[test]
    fn nothing_is_typed_while_the_user_types_into_that_agent() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, working(), 1);
        talk_to(&mut m, AGENT, "rebase");
        let term = agent_term::Terminal::spawn(
            &agent_core::process::LaunchSpec::new("cat"),
            agent_term::TermSize::of(ratatui::layout::Rect::new(0, 0, 80, 24)).expect("size"),
            Box::new(agent_term::Vt100::new(
                agent_term::TermSize::of(ratatui::layout::Rect::new(0, 0, 80, 24)).expect("size"),
            )) as Box<dyn agent_term::Emulator>,
        )
        .expect("a pty");
        m.pane = Pane::Connected(crate::model::Connection {
            name: name(AGENT),
            term,
            others: None,
            mode: Mode::Agent,
            opened: Instant::now(),
        });
        assert_eq!(
            replies(&report(&mut m, reported(done(), 0))),
            Vec::<String>::new()
        );
        if let Some(c) = m.pane.connection_mut() {
            c.mode = Mode::Nav;
        }
        assert_eq!(replies(&tick(&mut m)), ["rebase"], "resumes in NAV");
    }

    #[test]
    fn a_queue_the_agent_never_reports_on_says_it_is_waiting() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        talk_to(&mut m, AGENT, "/compact");
        talk_to(&mut m, AGENT, "next");
        sent(&mut m, AGENT);
        let outbox = &m.outboxes[&name(AGENT)];
        assert!(!outbox.stalled(Some(1)), "just sent");
        let mut outbox = crate::model::Outbox::default();
        outbox.push("next".parse().expect("a prompt"), Vec::new());
        outbox.sent_instant = Instant::now().checked_sub(crate::model::STALL_AFTER * 2);
        outbox.sent_at = Some(1);
        assert!(outbox.stalled(Some(1)));
        assert!(!outbox.stalled(Some(2)), "it reported");
        assert!(replies(&tick(&mut m)).is_empty(), "never sent blindly");
    }

    #[test]
    fn q_with_queued_messages_asks_before_quitting() {
        let mut m = crate::fixtures::fleet(10);
        assert!(matches!(
            update(&mut m, key(KeyCode::Char('q'))).as_slice(),
            [Effect::Quit]
        ));
        set_state(&mut m, AGENT, working(), 1);
        talk_to(&mut m, AGENT, "rebase");
        assert!(update(&mut m, key(KeyCode::Char('q'))).is_empty());
        assert!(matches!(m.overlay, Overlay::Confirm(Confirm::Quit(1))));
        update(&mut m, key(KeyCode::Esc));
        assert!(matches!(m.overlay, Overlay::None));
        update(&mut m, key(KeyCode::Char('q')));
        assert!(matches!(
            update(&mut m, key(KeyCode::Char('y'))).as_slice(),
            [Effect::Quit]
        ));
    }

    #[test]
    fn the_queue_survives_selection_changes_and_ctrl_x_in_the_box_clears_it() {
        let mut m = crate::fixtures::fleet(10);
        let agent = "auth-refresh-2";
        set_state(&mut m, agent, working(), 1);
        talk_to(&mut m, agent, "one");
        talk_to(&mut m, agent, "two");
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        assert_eq!(m.outboxes[&name(agent)].len(), 2);

        m.set_cursor(Cursor::Entry(name(agent)));
        update(&mut m, key(KeyCode::Char('s')));
        update(
            &mut m,
            Msg::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
        );
        assert!(!m.outboxes.contains_key(&name(agent)));
        assert!(matches!(m.overlay, Overlay::Talk { .. }), "the box stays");
    }

    #[test]
    fn v_opens_and_closes_the_peek_and_it_counts_as_the_terminal_being_visible() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        update(&mut m, key(KeyCode::Char('2')));
        assert!(!m.terminal_visible(), "[2] shows a document");
        update(&mut m, key(KeyCode::Char('v')));
        assert!(m.peeking());
        assert!(m.terminal_visible());
        update(&mut m, key(KeyCode::Char('v')));
        assert!(!m.peeking());
        update(&mut m, key(KeyCode::Char('v')));
        update(&mut m, key(KeyCode::Esc));
        assert!(!m.peeking());
        assert_eq!(
            m.panel,
            Panel::Agent { offset: 0 },
            "Esc closed only the peek"
        );
    }

    #[test]
    fn keys_do_not_reach_the_terminal_while_peeking() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        update(&mut m, key(KeyCode::Char('v')));
        let before = selected(&m);
        for c in ['j', 'k', 'x', 'q', 'd', 'n', 'G', 'p'] {
            let effects = update(&mut m, key(KeyCode::Char(c)));
            assert!(effects.is_empty(), "{c}");
        }
        assert!(m.peeking());
        assert!(matches!(m.focus(), Focus::Nav(_)));
        assert!(matches!(m.overlay, Overlay::None));
        assert_eq!(selected(&m), before);
    }

    #[test]
    fn t_in_the_peek_opens_the_box_and_the_peek_stays_behind_it() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, "obs-layout-4", done(), 1);
        m.set_cursor(Cursor::Entry(name("obs-layout-4")));
        update(&mut m, key(KeyCode::Char('v')));
        update(&mut m, key(KeyCode::Char('s')));
        assert!(matches!(m.overlay, Overlay::Talk { .. }));
        type_text(&mut m, "hi");
        let effects = update(&mut m, key(KeyCode::Enter));
        assert_eq!(replies(&effects), ["hi"]);
        assert!(m.peeking(), "back to the peek");
    }

    #[test]
    fn i_and_enter_in_the_peek_enter_the_agent() {
        for code in [KeyCode::Char('i'), KeyCode::Enter] {
            let mut m = crate::fixtures::fleet(10);
            m.set_cursor(Cursor::Entry(name("obs-layout-4")));
            update(&mut m, key(KeyCode::Char('v')));
            let effects = update(&mut m, key(code));
            assert!(
                effects.iter().any(|e| matches!(
                    e,
                    Effect::Attach {
                        intent: Intent::Activate,
                        ..
                    }
                )),
                "{code:?}"
            );
            assert!(!m.peeking());
        }
    }

    #[test]
    fn e_on_a_subagent_row_opens_its_transcript_and_on_the_turn_row_the_checkout() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        let effects = update(&mut m, key(KeyCode::Char('e')));
        assert!(
            matches!(effects.as_slice(), [Effect::Edit(Open::Checkout(_))]),
            "the turn row"
        );

        update(&mut m, key(KeyCode::Char('j')));
        let key_of = m.subagent_selected().expect("a subagent is selected");
        let effects = update(&mut m, key(KeyCode::Char('e')));
        assert!(edits(&effects) == 0, "not read yet");

        m.subagent_logs.insert(
            key_of,
            Ok(Some(agent_core::ports::SubagentDetail {
                transcript: "/p/s/subagents/agent-a1.jsonl".into(),
                ..Default::default()
            })),
        );
        let effects = update(&mut m, key(KeyCode::Char('e')));
        assert!(
            matches!(effects.as_slice(), [Effect::Edit(Open::File { root, file })]
                if root.ends_with("obs-layout-4")
                    && file == std::path::Path::new("/p/s/subagents/agent-a1.jsonl")),
        );
    }

    fn with_log(m: &mut Model, finished: bool) -> (SessionName, AgentId) {
        use crate::log::fixtures::{bash, detail, said};
        use agent_core::ports::{CallState, Summary};
        let (name, id) = m.subagent_selected().expect("a subagent is selected");
        let ok = || CallState::Done(Summary::NoOutput);
        let mut d = detail(vec![
            said("one"),
            bash("ls", ok()),
            said("two"),
            bash("pwd", ok()),
        ]);
        d.finished = finished;
        m.subagent_logs
            .insert((name.clone(), id.clone()), Ok(Some(d)));
        (name, id)
    }

    #[test]
    fn the_log_cursor_moves_by_entry_folds_and_opens_the_transcript_at_it() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        let key_ = with_log(&mut m, true);
        update(&mut m, key(KeyCode::Char('0')));
        let nav = |m: &Model| m.log_view.navs.get(&key_).cloned().unwrap_or_default();

        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(
            nav(&m).cursor,
            Some(1),
            "a finished log starts at the prompt"
        );
        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Enter));
        assert!(nav(&m).is_open(2));
        update(&mut m, key(KeyCode::Enter));
        assert!(!nav(&m).is_open(2));
        update(&mut m, key(KeyCode::Char('=')));
        assert!(nav(&m).is_open(4));
        update(&mut m, key(KeyCode::Char('-')));
        assert!(!nav(&m).is_open(4));
        update(&mut m, key(KeyCode::Char('G')));
        assert_eq!(nav(&m).cursor, Some(4));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(nav(&m).cursor, Some(4), "stops at the last entry");
        update(&mut m, key(KeyCode::Char('g')));
        assert_eq!(nav(&m).cursor, Some(0));

        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Char('j')));
        let effects = update(&mut m, key(KeyCode::Char('e')));
        assert!(
            matches!(effects.as_slice(), [Effect::Edit(Open::Transcript { text, line, .. })]
                if text.lines().nth(line.get() - 1) == Some("### Bash ls")),
            "e opens the rendered transcript at the cursor's entry"
        );
        update(&mut m, key(KeyCode::Esc));
        assert!(matches!(m.panel, Panel::Agent { .. }));
    }

    #[test]
    fn a_running_log_follows_on_its_last_entry_and_pauses_when_the_cursor_moves_up() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        with_log(&mut m, false);
        update(&mut m, key(KeyCode::Char('0')));
        update(&mut m, key(KeyCode::Char('k')));
        assert!(m.docs.paused() || !m.docs.following());
        update(&mut m, key(KeyCode::Char('G')));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(
            m.log_view.navs.values().next().and_then(|n| n.cursor),
            Some(4)
        );
    }

    #[test]
    fn comments_on_diff_lines_make_a_draft_that_s_sends_as_one_review() {
        use crate::review::Comment;
        let mut m = delegating();
        let name = m.selected_entry().expect("selected").name().clone();
        let line = |text: &str, new: usize| crate::fixtures::line_info("a.rs", text, new);
        let lines = vec![line("+let x = 3;", 4)];
        add_comment(&mut m, &name, &lines, "Use the limit.", None);
        add_comment(&mut m, &name, &[line("+more", 9)], "Why?", None);
        assert_eq!(m.drafts[&name].len(), 2);

        add_comment(&mut m, &name, &lines, "Use the configured limit.", None);
        assert_eq!(
            m.drafts[&name].len(),
            2,
            "the same line replaces its comment"
        );
        assert_eq!(m.drafts[&name][0].text, "Use the configured limit.");

        add_comment(&mut m, &name, &[line("+more", 9)], "", None);
        assert_eq!(
            m.drafts[&name].len(),
            1,
            "empty text on a commented line deletes it"
        );

        update(&mut m, key(KeyCode::Char('s')));
        assert!(matches!(m.overlay, Overlay::Talk { .. }));
        update(&mut m, key(KeyCode::Char('o')));
        update(&mut m, key(KeyCode::Enter));
        assert!(m.drafts.is_empty(), "the draft clears once it is queued");
        let sent: &str = m.outboxes[&name].iter().last().expect("queued").as_ref();
        assert!(sent.starts_with("Review comments."), "{sent}");
        assert!(
            sent.contains("\n\no\n\na.rs:4\n> +let x = 3;\nUse the configured limit."),
            "{sent}"
        );
        let _ = Comment::on(&[&line("+x", 1)], None, "");
    }

    #[test]
    fn y_in_a_subagent_log_copies_the_entry_under_the_cursor() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        with_log(&mut m, true);
        update(&mut m, key(KeyCode::Char('0')));
        update(&mut m, key(KeyCode::Char('j')));
        let effects = update(&mut m, key(KeyCode::Char('y')));
        assert!(matches!(effects.as_slice(), [Effect::Copy(text)] if text == "one"));
    }

    #[test]
    fn y_on_the_turn_copies_the_agents_message() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('0')));
        let effects = update(&mut m, key(KeyCode::Char('y')));
        assert!(
            matches!(effects.as_slice(), [Effect::Copy(text)] if text.contains("token refresh")),
            "the message is copied"
        );
    }
}
