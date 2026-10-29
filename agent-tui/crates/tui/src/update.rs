use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use agent_core::actions::{Action, CommandLine, Context, EventName, Invocation};
use agent_core::activity::{Activity, State as ActivityState};
use agent_core::config::ControlKey;
use agent_core::model::{RepoKey, Slot};
use agent_core::names::{Prompt, SessionName};
use agent_core::ops::{Approved, Launch};
use agent_core::ports::ClientId;
use agent_term::{CellPos, encode_key, encode_mouse};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::model::{
    Answer, Chord, Confirm, Cursor, Edge, Enter, Field, Focus, Follow, INFO_TTL, Inspection,
    Intent, Mode, Model, NamingState, NewAgent, Overlay, Pane, Phase, Query, Reach, StatusKind,
    Submission, TerminalFocus, Ticket, View, fuzzy_match, repo_matches,
};
use crate::worker::{Job, Msg, Outcome};

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

pub enum Effect {
    Job(Job),
    /// Read git facts about an agent's worktree.
    Inspect {
        name: SessionName,
        worktree: PathBuf,
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
    /// Suspend the TUI and open `$EDITOR` here.
    Edit(PathBuf),
    /// Suspend the TUI and edit the new-agent task in `$EDITOR`.
    EditTask(String),
    Quit,
    /// Ring the terminal's bell.
    Bell,
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
        Msg::Activities(activities) => on_activities(m, activities),
        Msg::Focus(focus) => {
            m.terminal = focus;
            Vec::new()
        }
        Msg::Inspected { name, facts } => {
            if m.inspections.contains_key(&name) {
                m.facts.insert(name, facts);
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
    effects.extend(acknowledge(m, SEEN_AFTER).map(Effect::Job));
    effects.extend(preview(m));
    effects.extend(inspect(m));
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
    match result {
        Ok(launch) => {
            if matches!(&m.overlay, Overlay::New(form) if matches!(form.phase, Phase::Submitting { ticket: t, .. } if t == ticket))
            {
                m.overlay = Overlay::None;
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
        Outcome::Replied(name) => m.info(format!("sent to {name}")),
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
    m.revalidate();
    Vec::new()
}

/// The agent in the pane has finished a turn and been in view for `after`
/// with the terminal focused: mark it seen.
fn acknowledge(m: &mut Model, after: Duration) -> Option<Job> {
    let c = m
        .pane
        .connection()
        .filter(|c| c.opened.elapsed() >= after)?;
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
    m.facts.retain(|name, _| known.contains(name));
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
    if !matches!(m.cursor(), Some(Cursor::Grouped(_, n)) if n == name) {
        m.set_cursor(Cursor::Entry(name.clone()));
    }
    m.preview_at = None;
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
    on_nav_key(m, key)
}

/// Control characters other than newlines and tabs could end a bracketed
/// paste early and run the rest as keystrokes.
fn sanitize_paste(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect()
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
    match key.code {
        KeyCode::Char('q') => return vec![Effect::Quit],
        KeyCode::Esc | KeyCode::Char('i') => return enter_agent(m),
        KeyCode::Enter | KeyCode::Char('l') => match (m.cursor().cloned(), selected) {
            (Some(Cursor::Repo(key)), _) => {
                m.view = View::Focus;
                m.select_repo(&key);
            }
            (_, Some(entry)) => return activate(m, entry.name()),
            _ => {}
        },
        KeyCode::Char('j') | KeyCode::Down => m.move_cursor(1),
        KeyCode::Char('k') | KeyCode::Up => m.move_cursor(-1),
        KeyCode::Char('g') | KeyCode::Home => m.move_to_edge(Edge::First),
        KeyCode::Char('G') | KeyCode::End => m.move_to_edge(Edge::Last),
        KeyCode::Char(']') => m.jump_repo(1),
        KeyCode::Char('[') => m.jump_repo(-1),
        KeyCode::Tab => m.toggle_view(),
        KeyCode::Char('p') => m.overlay = Overlay::Picker(Query::default()),
        KeyCode::Char(' ' | '/') => m.overlay = Overlay::Switcher(Query::default()),
        KeyCode::Char('?') => m.overlay = Overlay::Help,
        KeyCode::Char(c @ '1'..='9') => {
            let slot = c
                .to_digit(10)
                .and_then(|d| u8::try_from(d).ok())
                .and_then(Slot::new);
            match slot
                .and_then(|s| m.snapshot().and_then(|snap| snap.slot(s)))
                .map(|(_, e)| e.name().clone())
            {
                Some(name) => return activate(m, &name),
                None => m.info(format!("nothing pinned to {c} · m pins the selected agent")),
            }
        }
        KeyCode::Char('m') => {
            if let Some(entry) = selected {
                let name = entry.name().clone();
                return vec![Effect::Job(match entry.pin() {
                    Some(_) => Job::Unpin(name),
                    None => Job::Pin(name),
                })];
            }
        }
        KeyCode::Char('n') => open_new_form(m),
        KeyCode::Char('r') => {
            if let Some(entry) = selected {
                match m.replyable(&entry).cloned() {
                    Some(to) => {
                        m.overlay = Overlay::Reply {
                            to,
                            text: String::new(),
                        }
                    }
                    None => m.info(format!(
                        "{} can't take a reply from here · ⏎ attaches",
                        entry.name()
                    )),
                }
            }
        }
        KeyCode::Char('o') => {
            let path = m.selected().and_then(|(repo, e)| {
                e.and_then(|e| e.path())
                    .or_else(|| repo.key().root().map(agent_core::paths::RepoRoot::path))
                    .map(PathBuf::from)
            });
            if let Some(path) = path {
                return vec![Effect::Edit(path)];
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
            let here = m.pane.session().cloned();
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

fn open_new_form(m: &mut Model) {
    let default_repo = m
        .selected()
        .and_then(|(repo, _)| repo.key().root().cloned())
        .or_else(|| {
            m.snapshot()
                .and_then(|s| s.containing(&m.start_dir))
                .and_then(|(r, _)| r.key().root().cloned())
        });
    m.overlay = Overlay::New(Box::new(NewAgent::new(default_repo)));
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

/// Sends a reply, unless the agent started asking since: the text would
/// then land in its prompt, so the reply stays open with an error.
fn reply(m: &mut Model, to: SessionName, text: String) -> Vec<Effect> {
    let Ok(prompt) = text.parse::<Prompt>() else {
        m.overlay = Overlay::Reply { to, text };
        return Vec::new();
    };
    let open = m
        .snapshot()
        .and_then(|s| s.entry(&to))
        .and_then(|(_, entry)| m.replyable(entry))
        .is_some();
    if !open {
        m.error(format!("{to} is asking now, so the reply wasn't sent"));
        m.overlay = Overlay::Reply { to, text };
        return Vec::new();
    }
    vec![Effect::Job(Job::Reply { to, prompt })]
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

pub fn picker_matches(m: &Model, query: &str) -> Vec<RepoKey> {
    m.snapshot()
        .map(|s| {
            s.repos()
                .iter()
                .filter(|r| fuzzy_match(query, &r.name()).is_some())
                .map(|r| r.key().clone())
                .collect()
        })
        .unwrap_or_default()
}

fn on_overlay_key(m: &mut Model, key: KeyEvent) -> Vec<Effect> {
    match std::mem::replace(&mut m.overlay, Overlay::None) {
        Overlay::None | Overlay::Help => Vec::new(),
        Overlay::New(form) => {
            m.overlay = Overlay::New(form);
            on_new_key(m, &key)
        }
        Overlay::Reply { to, mut text } => match key.code {
            KeyCode::Esc => Vec::new(),
            KeyCode::Enter => reply(m, to, text),
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
                m.overlay = Overlay::Reply { to, text };
                Vec::new()
            }
        },
        Overlay::Switcher(mut query) => {
            let matches = switcher_matches(m, &query.text);
            match key.code {
                KeyCode::Esc | KeyCode::Tab => Vec::new(),
                KeyCode::Enter => query
                    .pick(&matches)
                    .cloned()
                    .map(|name| activate(m, &name))
                    .unwrap_or_default(),
                _ => {
                    edit_query(&mut query, &key, matches.len(), m.keys.nav());
                    m.overlay = Overlay::Switcher(query);
                    Vec::new()
                }
            }
        }
        Overlay::Picker(mut query) => {
            let matches = picker_matches(m, &query.text);
            match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if let Some(key) = query.pick(&matches).cloned() {
                        m.select_repo(&key);
                    }
                }
                _ => {
                    edit_query(&mut query, &key, matches.len(), m.keys.nav());
                    m.overlay = Overlay::Picker(query);
                }
            }
            Vec::new()
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
    let repos = repo_matches(m.snapshot(), &form.repo.text).len();
    match key.code {
        KeyCode::Esc => m.overlay = Overlay::None,
        KeyCode::Enter if !alt => return submit(m),
        _ if ctrl(key, 'e') => {
            let Overlay::New(form) = &mut m.overlay else {
                return Vec::new();
            };
            form.focus = Field::Task;
            return vec![Effect::EditTask(form.task.clone())];
        }
        _ => edit_form(m, |form| match key.code {
            KeyCode::Enter if form.focus == Field::Task => form.task.push('\n'),
            _ if ctrl(key, 'j') && form.focus == Field::Task => form.task.push('\n'),
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

fn on_mouse(m: &mut Model, mouse: MouseEvent) -> Vec<Effect> {
    let at = Position::new(mouse.column, mouse.row);
    let clicked = matches!(mouse.kind, MouseEventKind::Down(_));
    if let Focus::Overlay(_) = m.focus() {
        return Vec::new();
    }
    if let Some(cell) = CellPos::within(m.areas.terminal, at) {
        let scrolling = matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        );
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
    use crate::model::{Launchable, NameCheck, Row};

    fn snapshot(sessions: Vec<SessionInfo>, pins: &[&str]) -> agent_core::model::Snapshot {
        let obs = Observation {
            sessions,
            repos: vec![
                (
                    RepoRoot::new("/src/api".into()).unwrap(),
                    RepoWorktrees {
                        main: wt("/src/api"),
                        linked: vec![wt("/w/api/fix"), wt("/w/api/idle")],
                    },
                ),
                (
                    RepoRoot::new("/src/web".into()).unwrap(),
                    RepoWorktrees {
                        main: wt("/src/web"),
                        linked: Vec::new(),
                    },
                ),
            ],
            warnings: Vec::new(),
        };
        let pins: Vec<SessionName> = pins.iter().map(|p| name(p)).collect();
        assemble(&obs, &Layout::new("/w".into()).unwrap(), &pins).0
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
            "claude".parse().unwrap(),
            "claude".into(),
        );
        update(
            &mut m,
            Msg::Snapshot(Ok((snapshot(sessions, pins), Vec::new()))),
        );
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
                selected(&m).unwrap()
            })
            .collect();
        assert_eq!(order, ["idle", "api", "fix"]);
    }

    #[test]
    fn overview_expands_only_the_selected_repo_and_collapses_single_entry_repos() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Tab));
        let rows = m.rows();
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert!(
            matches!(rows.last(), Some(Row::Entry { name, place: crate::model::Place::Solo }) if name.as_str() == "web")
        );
    }

    #[test]
    fn enter_on_a_repo_row_focuses_it() {
        let mut m = model(&[]);
        update(&mut m, key(KeyCode::Tab));
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
                RepoRoot::new("/src/api".into()).unwrap(),
                RepoWorktrees {
                    main: wt("/src/api"),
                    linked: Vec::new(),
                },
            )],
            ..Observation::default()
        };
        let snap = assemble(&obs, &Layout::new("/w".into()).unwrap(), &[]).0;
        update(&mut m, Msg::Snapshot(Ok((snap, Vec::new()))));
        assert_eq!(selected(&m).as_deref(), Some("api"));
    }

    #[test]
    fn pinned_slots_and_switcher_order() {
        let mut m = model(&["idle"]);
        m.touch_mru(&name("fix"));
        m.touch_mru(&name("api"));
        let order: Vec<_> = m
            .switcher_items()
            .iter()
            .map(|(_, e)| e.name().to_string())
            .collect();
        assert_eq!(order, ["api", "idle", "fix", "web"]);
        let effects = update(&mut m, key(KeyCode::Char('1')));
        assert!(
            matches!(effects.as_slice(), [Effect::Job(Job::Start { at, .. })] if at.name().as_str() == "idle")
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
        .unwrap()
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
            "claude".parse().unwrap(),
            "claude".into(),
        );
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
            "claude".parse().unwrap(),
            "claude".into(),
        );
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
            "claude".parse().unwrap(),
            "claude".into(),
        );
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
        update(&mut m, key(KeyCode::Tab));
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
    fn a_reply_goes_to_the_selected_agent_while_it_shows_no_prompt() {
        use agent_core::activity::State;
        let mut m = two_live();
        m.set_cursor(Cursor::Entry(name("fix")));
        m.activities = Some(HashMap::from([(
            name("fix"),
            reported(State::Done { summary: None }, 5),
        )]));
        update(&mut m, key(KeyCode::Char('r')));
        type_text(&mut m, "rebase");
        let effects = update(&mut m, key(KeyCode::Enter));
        assert!(
            matches!(effects.as_slice(), [Effect::Job(Job::Reply { to, prompt })]
                if to.as_str() == "fix" && prompt.as_str() == "rebase"),
            "{}",
            effects.len()
        );
        assert!(matches!(m.overlay, Overlay::None));
    }

    #[test]
    fn no_reply_is_typed_into_a_prompt() {
        use agent_core::activity::State;
        let mut m = two_live();
        m.set_cursor(Cursor::Entry(name("fix")));
        m.activities = Some(HashMap::from([(name("fix"), asking("rm?", 5))]));
        update(&mut m, key(KeyCode::Char('r')));
        assert!(
            matches!(m.overlay, Overlay::None),
            "asking agents get no reply box"
        );
        m.activities = Some(HashMap::from([(name("fix"), reported(State::Idle, 5))]));
        update(&mut m, key(KeyCode::Char('r')));
        type_text(&mut m, "go");
        m.activities = Some(HashMap::from([(name("fix"), asking("rm?", 0))]));
        assert!(update(&mut m, key(KeyCode::Enter)).is_empty());
        assert!(
            matches!(&m.overlay, Overlay::Reply { text, .. } if text == "go"),
            "the text is kept for when it's answered"
        );
    }

    #[test]
    fn the_selection_follows_an_agent_into_and_out_of_needs_you() {
        use agent_core::activity::State;
        let mut m = two_live();
        update(&mut m, key(KeyCode::Tab));
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
    fn a_pinned_selection_follows_its_agent_into_needs_you() {
        let mut m = model(&["fix"]);
        update(&mut m, key(KeyCode::Tab));
        m.set_cursor(Cursor::Grouped(crate::model::Group::Pinned, name("fix")));
        update(
            &mut m,
            Msg::Activities(HashMap::from([(name("fix"), asking("rm?", 5))])),
        );
        assert_eq!(
            m.cursor(),
            Some(&Cursor::Grouped(crate::model::Group::NeedsYou, name("fix")))
        );
        assert!(m.items().contains(m.cursor().unwrap()));
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
}
