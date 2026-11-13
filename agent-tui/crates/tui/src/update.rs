use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_core::actions::{Action, CommandLine, Context, EventName, Invocation};
use agent_core::activity::{
    Activity, AgentId, Hold, InterruptSignal, Standing, State, TranscriptRef,
};
use agent_core::config::ControlKey;
use agent_core::model::{Entry, EntryState, RepoKey, Snapshot};
use agent_core::names::{CarriageReturn, InvalidPrompt, Prompt, SessionName, clean};
use agent_core::ops::{Approved, Launch, RemovalPlan};
use agent_core::paths::{RepoRoot, Typed, typed_path};
use agent_core::ports::{ActivityDetail, ClientId, Listing, Observation, Probe, TranscriptStamp};
use agent_term::{CellPos, encode_key, encode_mouse};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::cause::Cause;
use crate::document::{Documents, Key};
use crate::keymap::Situation;
use crate::log::{EntryKey, Nav};
use crate::model::{
    ActivityRow, AfterCheck, Answer, Areas, CheckHealth, Chord, CommentDraft, Company, Confirm,
    Cursor, DirCheck, Direction, Drawn, Edge, Enter, Field, FirstLine, Focus, Follow, FormKind,
    HelpState, INFO_TTL, Inspected, Inspection, Intent, Listed, LogKey, Look, Missing, Mode, Model,
    Moment, NamingState, Need, NewAgent, Overlay, Pane, Panel, Peek, Phase, Planning, PromptEdit,
    Query, Reach, Reply, ReplyTo, RepoInput, Screen, StatusKind, Submission, TerminalFocus, Ticket,
    Unreachable, View, Watch, WatchView, WorkRoom, elsewhere_banner, fuzzy_match, match_rank,
    repo_matches, unlisted,
};
use crate::touched::Baseline;
use crate::track::{Compose, Found, Probed, Seen};
use crate::tree::ChangesView;
use crate::typed::{Columns, Message};
use crate::worker::{Job, Msg, Outcome, OutputRead, OutputRequest, SendError};

mod restart;

/// How long the pane shows a finished agent, terminal focused, before it
/// counts as seen.
const SEEN_AFTER: Duration = Duration::from_secs(3);
/// At most one bell this often.
const BELL_EVERY: Duration = Duration::from_secs(10);
/// An activity this long rings when it finishes. Shorter ones you were probably
/// watching.
const LONG_TURN: Duration = Duration::from_secs(30);
/// An agent's git facts refresh on its activity, at most this often.
const REINSPECT_AFTER: Duration = Duration::from_secs(3);
/// And this often whatever it does.
const SLOW_REFRESH: Duration = Duration::from_secs(30);
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

impl Open {
    /// The directory the editor runs in.
    pub fn dir(&self) -> &Path {
        match self {
            Self::Checkout(dir) => dir,
            Self::File { root, .. } | Self::At { root, .. } | Self::Transcript { root, .. } => root,
        }
    }
}

/// What a new connection starts that needs no terminal.
pub enum Background {
    Job(Job),
    Look(Look),
}

pub enum Effect {
    Job(Job),
    /// Hand restarts to the restart thread, which runs them in order.
    Restart(Vec<agent_core::ops::Restart>),
    /// Read git facts about an agent's worktree.
    Inspect {
        name: SessionName,
        worktree: PathBuf,
        /// How many events the agent has reported.
        events: Option<u64>,
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
    /// Read the current activity of an agent working in `workdir`.
    ReadActivity {
        name: SessionName,
        workdir: PathBuf,
        /// Where its hooks last said the transcript is.
        at: Option<TranscriptRef>,
    },
    /// Read what the background command `id` of an agent working in `workdir`
    /// has printed. The answer carries `ticket`.
    ReadCommandOutput(OutputRequest),
    /// Read the transcript at `at` for what it says of the activity
    /// `standing` proves.
    ProbeTranscript {
        name: SessionName,
        at: TranscriptRef,
        standing: Standing,
        /// The file as the last read found it, so nothing is read while it is
        /// unchanged.
        seen: Option<TranscriptStamp>,
    },
    /// Attach to an existing session (resurrecting it if it exited).
    Attach {
        name: SessionName,
        intent: Intent,
        ticket: Ticket,
    },
    /// Look for the windows showing any session, off the UI thread.
    CheckElsewhere(Look),
    /// Show a session read-only at the size it was found to have.
    Observe {
        observation: Observation,
        ticket: Ticket,
    },
    Connect {
        launch: Launch,
        intent: Intent,
        ticket: Ticket,
    },
    /// Ask the harness's model to name the form's prompt.
    SuggestName {
        prompt: Prompt,
        harness: agent_core::ports::HarnessId,
        edit: PromptEdit,
    },
    /// Suspend the TUI and open `$EDITOR`.
    Edit(Open),
    /// Suspend the TUI and edit the new-agent task in `$EDITOR`.
    EditTask(String),
    /// Suspend the TUI and edit the message written for `to` in `$EDITOR`.
    EditMessage {
        to: SessionName,
        text: String,
    },
    Quit,
    /// Type bytes into `to`'s terminal, if it is still the connected one.
    Pty {
        to: SessionName,
        bytes: Vec<u8>,
    },
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

/// The selected agent's checkout.
fn selected_root(m: &Model) -> Option<PathBuf> {
    m.selected_entry()?.path().map(Path::to_path_buf)
}

fn own_client(m: &Model) -> Option<ClientId> {
    m.pane.connection().map(|c| c.id)
}

pub fn update(m: &mut Model, msg: Msg) -> Vec<Effect> {
    let effects = step(m, msg);
    forget_unlisted(m);
    settle(m);
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
        Msg::Paste(text) => on_paste(m, &text),
        Msg::Resize => Vec::new(),
        Msg::Snapshot(Ok((snapshot, warnings))) => {
            on_snapshot(m, snapshot, warnings);
            Vec::new()
        }
        Msg::Snapshot(Err(e)) | Msg::Done(Err(e)) => {
            m.error(e);
            Vec::new()
        }
        Msg::RemovalPlanned { ticket, result } => {
            on_removal_planned(m, ticket, result);
            Vec::new()
        }
        Msg::RestartPlanned { ticket, result } => restart::on_restart_planned(m, ticket, result),
        Msg::FleetPlanned { ticket, result } => restart::on_fleet_planned(m, ticket, result),
        Msg::Restarted { name, finish } => restart::on_restarted(m, &name, finish),
        Msg::Launched { ticket, result } => on_launched(m, ticket, result),
        Msg::Observed { ticket, result } => on_observed(m, ticket, result),
        Msg::Done(Ok(outcome)) => {
            report(m, outcome);
            Vec::new()
        }
        Msg::Sent { to, ticket, result } => {
            on_sent(m, &to, ticket, result);
            Vec::new()
        }
        Msg::Activities(activities) => on_activities(m, activities),
        Msg::Catalogue { repos, warnings } => {
            on_catalogue(m, repos, warnings);
            Vec::new()
        }
        Msg::ClosedElsewhere { name, closed } => on_closed_elsewhere(m, &name, closed),
        Msg::DirChecked { path, found } => {
            if let Overlay::New(form) = &mut m.overlay
                && form.dir.awaits(&path)
            {
                form.dir = DirCheck::Answered { path, found };
            }
            Vec::new()
        }
        Msg::NameSuggested { edit, name } => {
            if let Overlay::New(form) = &mut m.overlay {
                form.model_named(edit, name);
            }
            Vec::new()
        }
        Msg::Elsewhere { ticket, found } => on_elsewhere(m, ticket, found),
        Msg::Focus(focus) => {
            m.terminal = focus;
            Vec::new()
        }
        Msg::Inspected { name, read } => on_inspected(m, &name, read),
        Msg::Document { key, result } => {
            m.docs.loaded(&key, result, m.clock.mono);
            Vec::new()
        }
        Msg::Activity { name, detail } => {
            on_activity(m, &name, detail);
            Vec::new()
        }
        Msg::CommandOutput(read) => {
            on_command_output(m, read);
            Vec::new()
        }
        Msg::Probed {
            name,
            standing,
            result,
        } => {
            on_probed(m, &name, standing, result);
            Vec::new()
        }
        Msg::Subagent { name, id, detail } => {
            if let Some(watched) = m
                .agents
                .get_mut(&name)
                .and_then(|t| t.subagents.get_mut(&id))
            {
                watched.watch.log = Some(detail);
            }
            Vec::new()
        }
    }
}

/// Git facts about an agent's worktree were read, unless it is no longer
/// asked about.
fn on_inspected(m: &mut Model, name: &SessionName, read: Result<Inspected, Cause>) -> Vec<Effect> {
    if m.tracked(name).is_some_and(|t| t.inspection.is_some()) {
        m.set_facts(name, read);
    }
    Vec::new()
}

/// An agent's activity was read. A new prompt starts the cursor over.
fn on_activity(m: &mut Model, name: &SessionName, detail: Result<Option<ActivityDetail>, Cause>) {
    let Some(tracked) = m.agents.get_mut(name) else {
        return;
    };
    let watch = &mut tracked.activity_view;
    if let (Some(Ok(Some(old))), Ok(Some(new))) = (&watch.watch.log, &detail)
        && old.mark != new.mark
    {
        watch.watch.nav = Nav::default();
    }
    watch.watch.log = Some(detail);
}

/// What a background command printed was read. A failure is told once, until
/// a read succeeds again, and a paused document keeps its place in the text.
fn on_command_output(m: &mut Model, read: OutputRead) {
    let OutputRead {
        name,
        id,
        ticket,
        result,
    } = read;
    let moment = m.clock;
    let Some(heard) = m
        .agents
        .get_mut(&name)
        .and_then(|tracked| tracked.command_outputs.get_mut(&id))
        .and_then(|watch| watch.hear(ticket, result, moment))
    else {
        return;
    };
    let shown = matches!(m.hover(), Some(crate::document::Hover::Command(on, task)) if on == name && task == id);
    if shown && m.docs.paused() {
        m.docs.scroll = heard.moved.scroll(m.docs.scroll);
    }
    if let Some(cause) = heard.new_failure {
        m.error(format!(
            "couldn't read a command's output · {}",
            cause.first_line()
        ));
    }
}

/// A read of `name`'s transcript was answered. What it found is already
/// recorded by the worker, and arrives as the activity changing. The file's
/// stamp is kept for the proof it was read under, and a failure is told once
/// and then read again whole.
fn on_probed(m: &mut Model, name: &SessionName, standing: Standing, result: Result<Probe, Cause>) {
    let Some(probed) = m
        .agents
        .get_mut(name)
        .and_then(|t| t.probed.as_mut())
        .filter(|p| p.standing == standing)
    else {
        return;
    };
    let told = probed.found == Found::Failed;
    let found = match result {
        Ok(Probe::Going(stamp) | Probe::Interrupted(stamp, _) | Probe::Older(stamp)) => {
            Found::Stamp(stamp)
        }
        Ok(Probe::Unchanged) => return,
        Ok(Probe::NoTranscript) => Found::NoTranscript,
        Err(why) => {
            if !told {
                m.error(format!(
                    "couldn't read {name}'s transcript for an interrupt: {why}"
                ));
            }
            Found::Failed
        }
    };
    if let Some(probed) = m.agents.get_mut(name).and_then(|t| t.probed.as_mut()) {
        probed.found = found;
    }
}

/// Asks for a read of the transcript of every agent that is going, or was
/// interrupted, for whether the user stopped it or it carried on: its
/// harness sends no event for that, so the transcript is the only place it
/// shows. An agent is read again every [`PROBE_EVERY`], and nothing is read
/// while its file is unchanged.
fn probe_interrupts(m: &mut Model) -> Vec<Effect> {
    let Some(snapshot) = m.snapshot() else {
        return Vec::new();
    };
    let now = m.clock.mono;
    let due: Vec<(SessionName, TranscriptRef, Standing)> = snapshot
        .entries()
        .filter(|(_, e)| matches!(e.state(), EntryState::Live))
        .filter_map(|(_, entry)| {
            let name = entry.name();
            let activity = m
                .activity(name)
                .filter(|a| a.interrupts == InterruptSignal::Transcript)?;
            let standing = activity.standing()?;
            let transcript = activity.transcript.clone()?;
            let last = m.tracked(name).and_then(|t| t.probed.as_ref());
            Probed::due(last, standing, now).then(|| (name.clone(), transcript, standing))
        })
        .collect();
    due.into_iter()
        .map(|(name, at, standing)| {
            let tracked = m.track(&name);
            let (probed, seen) = Probed::asked(tracked.probed.take(), standing, now);
            tracked.probed = Some(probed);
            Effect::ProbeTranscript {
                name,
                at,
                standing,
                seen,
            }
        })
        .collect()
}

fn on_paste(m: &mut Model, text: &str) -> Vec<Effect> {
    match &mut m.overlay {
        Overlay::New(_) => {
            edit_form(m, |form| {
                for c in sanitize_paste(text).chars() {
                    type_into(form, c);
                }
            });
        }
        Overlay::Talk(to) => {
            let to = to.clone();
            let room = crate::view::talk_room(m.size);
            if let Some(Compose::Editing { text: typed, .. }) =
                m.tracked_mut(&to).and_then(|t| t.compose.as_mut())
            {
                typed.insert_str(&clean(text, CarriageReturn::Newline, None));
                if let Some(room) = room {
                    typed.reveal(room);
                }
            }
        }
        Overlay::Comment(CommentDraft { text: typed, .. }) => {
            typed.insert_str(&clean(text, CarriageReturn::Newline, None));
        }
        Overlay::None
        | Overlay::Switcher(_)
        | Overlay::Picker(_)
        | Overlay::Confirm(_)
        | Overlay::Help(_)
        | Overlay::Actions(_)
        | Overlay::Planning { .. } => {
            if let Some(c) = m.pane.connection().filter(|c| c.mode == Mode::Agent) {
                let text = sanitize_paste(text);
                let bytes = if c.term.modes().bracketed_paste {
                    format!("\x1b[200~{text}\x1b[201~")
                } else {
                    text
                };
                return vec![Effect::Pty {
                    to: c.name.clone(),
                    bytes: bytes.into_bytes(),
                }];
            }
        }
    }
    Vec::new()
}

/// "first (+n more{hint})": a warning list as one status line.
fn first_warning(warnings: &[String], hint: &str) -> String {
    match warnings {
        [] => String::new(),
        [only] => only.clone(),
        [first, rest @ ..] => format!("{first} (+{} more{hint})", rest.len()),
    }
}

/// Whether the overlay is still waiting on the plan `ticket` asked for, of
/// the kind `wanted` picks.
fn waiting_on(m: &Model, ticket: Ticket, wanted: fn(&Planning) -> bool) -> bool {
    matches!(&m.overlay, Overlay::Planning { ticket: t, planning } if *t == ticket && wanted(planning))
}

fn on_removal_planned(m: &mut Model, ticket: Ticket, result: Result<RemovalPlan, Cause>) {
    if !waiting_on(m, ticket, |p| matches!(p, Planning::Removal(_))) {
        return;
    }
    m.overlay = Overlay::None;
    match result {
        Ok(plan) => m.overlay = Overlay::Confirm(Confirm::Remove(plan.into())),
        Err(e) => m.error(e),
    }
}

fn on_snapshot(m: &mut Model, snapshot: Snapshot, warnings: Vec<String>) {
    m.load(snapshot);
    m.hold_activity_row();
    forget_gone(m);
    if !warnings.is_empty() && warnings != m.last_warnings {
        let line = first_warning(&warnings, " in `agent ls`");
        m.error(format!("discovery: {line}"));
    }
    m.last_warnings = warnings;
}

fn on_catalogue(m: &mut Model, repos: Vec<RepoRoot>, warnings: Vec<String>) {
    m.catalogue = repos;
    if !warnings.is_empty() && warnings != m.catalogue_warnings {
        m.error(first_warning(&warnings, ""));
    }
    m.catalogue_warnings = warnings;
}

/// What closing the other windows came to. A close that worked leaves the
/// session alone, and a close that failed leaves what was known. Either way
/// a new look follows at once, which drops any answer from before the close,
/// and a pane waiting on an older look takes this one's answer instead.
fn on_closed_elsewhere(
    m: &mut Model,
    name: &SessionName,
    closed: Result<usize, Cause>,
) -> Vec<Effect> {
    match closed {
        Ok(n) => {
            m.record_company(name, Company::Alone);
            if n > 0 {
                m.info(format!("closed {n} other window(s) on {name}"));
            }
        }
        Err(e) => m.error(e),
    }
    vec![Effect::CheckElsewhere(looking(m, Need::Decision))]
}

/// Asks the harness to name the form's prompt once typing has paused.
fn ask_for_name(m: &mut Model) -> Option<Effect> {
    let Overlay::New(form) = &mut m.overlay else {
        return None;
    };
    let NamingState::Due(at) = form.naming.state else {
        return None;
    };
    if m.clock.mono < at {
        return None;
    }
    form.naming.state = NamingState::Idle;
    let prompt = form.task.parse::<agent_core::names::Prompt>().ok()?;
    form.naming.state = NamingState::Asking;
    let edit = form.naming.edit;
    Some(Effect::SuggestName {
        prompt,
        harness: m.harness.clone(),
        edit,
    })
}

/// Asks the worker whether the repo field's typed path is a directory, once
/// per path, since the form can't look at the filesystem itself.
fn check_dir(m: &mut Model) -> Option<Effect> {
    let Overlay::New(form) = &m.overlay else {
        return None;
    };
    let RepoInput::Checking(path) = m.derive(form).repo_input else {
        return None;
    };
    if form.dir.awaits(&path) {
        return None;
    }
    let Overlay::New(form) = &mut m.overlay else {
        return None;
    };
    form.dir = DirCheck::Asked(path.clone());
    Some(Effect::Job(Job::CheckDir(path)))
}

/// [`tick_at`] at the real time.
pub fn tick(m: &mut Model) -> Vec<Effect> {
    tick_at(m, Moment::now())
}

/// Brings everything a draw would otherwise have to decide up to date: the
/// sidebar's room, what [0] points at and [3]'s tree. Runs after each
/// message and tick, so a draw only reads. It can also change the pane: a
/// connection another window shares, with the keys back in NAV and no peek
/// open, drops its client so that window gets the full size.
fn settle(m: &mut Model) {
    pause_if_elsewhere(m);
    let geometry = crate::view::geometry(m.size, m.screen);
    m.areas.terminal = geometry.terminal;
    m.fit_sidebar(geometry.sidebar);
    m.settle();
    fit_command_output(m);
}

/// Wraps what the command [0] shows has printed to [0]'s width.
fn fit_command_output(m: &mut Model) {
    let Some(crate::document::Hover::Command(name, id)) = m.hover() else {
        return;
    };
    let width = usize::from(m.areas.terminal.width);
    let Some(heading) = crate::view::command_heading_row(m, &id, width) else {
        return;
    };
    if let Some(watch) = m
        .agents
        .get_mut(&name)
        .and_then(|tracked| tracked.command_outputs.get_mut(&id))
    {
        watch.fit(width, heading, crate::view::wrap);
    }
}

/// The terminal is `size` big now.
pub fn resized(m: &mut Model, size: ratatui::layout::Rect) {
    m.size = size;
    settle(m);
}

/// Takes in what a draw laid out, the only thing drawing leaves behind.
/// Scroll and following are placed here, from the room the draw measured,
/// so the keys that move over a document can rely on them.
pub fn drawn(m: &mut Model, drawn: Drawn) {
    let Drawn {
        areas,
        work_room,
        scroll,
        overlay_offset,
        rows,
    } = drawn;
    m.areas = areas;
    if let Some(room) = work_room {
        m.work_room = room;
    }
    if let Some(scroll) = scroll {
        m.scroll = scroll;
        clamp_panel(m);
    }
    match (&mut m.overlay, overlay_offset) {
        (Overlay::Switcher(query) | Overlay::Picker(query), Some(offset)) => {
            query.offset = offset;
        }
        (Overlay::Help(help), Some(offset)) => help.offset = offset,
        _ => {}
    }
    if let Some(rows) = rows {
        m.docs.keep(rows);
    }
    if let Some(main) = &m.areas.main {
        if let Some(slot) = &main.diff {
            m.diff_view = m.diff_view.settled(slot, m.docs.lines());
        }
        m.docs.place(main.total, main.height, main.tails);
    }
}

/// Hands the focus back to [1] when the panel it was in wasn't laid out,
/// and keeps what the layout scrolled and clamped in the panel that was.
fn clamp_panel(m: &mut Model) {
    let list = match m.panel.list() {
        Listed::Agents => return,
        Listed::Agent { row, .. } => match m.areas.agent_first {
            Some(FirstLine(first)) => Panel::Agent {
                offset: first,
                row: row.clone(),
            },
            None => Panel::Agents,
        },
        Listed::Changes { file } if !m.areas.changes_panel.is_empty() => Panel::Changes {
            file: file.min(m.change_rows().saturating_sub(1)),
        },
        Listed::Changes { .. } => Panel::Agents,
    };
    m.panel = std::mem::replace(&mut m.panel, Panel::Agents).with_list(list);
}

/// Periodic work: debounced preview attach, naming, and status expiry. The
/// clock is read by the caller, so everything one tick decides agrees on it.
pub fn tick_at(m: &mut Model, now: Moment) -> Vec<Effect> {
    m.clock = now;
    // What the cursor is on must be judged at this moment before deciding
    // what to read. What the tick then asks for changes nothing settling
    // reads, so once is enough.
    settle(m);
    tick_effects(m)
}

fn tick_effects(m: &mut Model) -> Vec<Effect> {
    if m.status.as_ref().is_some_and(|s| {
        s.kind == StatusKind::Info && m.clock.mono.saturating_duration_since(s.at) > INFO_TTL
    }) {
        m.status = None;
    }
    let mut effects = check_timed_out(m);
    effects.extend(ask_for_name(m));
    effects.extend(bell(m));
    effects.extend(recheck_elsewhere_when_due(m));
    effects.extend(look(m, SEEN_AFTER).map(Effect::Job));
    effects.extend(preview(m));
    effects.extend(inspect(m));
    effects.extend(read_subagent(m));
    effects.extend(read_activity(m));
    effects.extend(read_command_output(m));
    effects.extend(probe_interrupts(m));
    effects.extend(read_document(m));
    effects.extend(check_dir(m));
    effects
}

/// How long after an action the TUI looks for the window it may have opened,
/// which gives the terminal time to start the client.
const ELSEWHERE_RECHECK_AFTER: Duration = Duration::from_secs(1);

/// How long a check may take before the pane attaches without it.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// Asks whether another window shows `name` before attaching to it.
fn check_first(m: &mut Model, name: &SessionName, then: AfterCheck) -> Effect {
    m.set_pane(Pane::Checking {
        name: name.clone(),
        then,
        until: m.clock.mono + CHECK_TIMEOUT,
    });
    Effect::CheckElsewhere(looking(m, Need::Decision))
}

/// The one place the pane starts waiting on a client.
fn connecting(m: &mut Model, name: &SessionName) -> Ticket {
    let ticket = m.ticket();
    if matches!(&m.peek, Some(Peek::Watch(w)) if &w.name == name) {
        m.peek = Some(Peek::Pane);
    }
    m.set_pane(Pane::Connecting {
        name: name.clone(),
        ticket,
    });
    ticket
}

/// Like [`connecting`], for a session that isn't running yet.
fn starting(m: &mut Model, name: &SessionName) -> Ticket {
    let ticket = connecting(m, name);
    let now = m.clock.wall;
    m.track(name).launched(now);
    ticket
}

/// Turns a float over the pane's paused session into a watch of it, since
/// another window shows it and a client of ours would squish that window.
fn start_watching(m: &mut Model) -> Vec<Effect> {
    let (Some(Peek::Pane), Some(name)) = (&m.peek, m.pane.held_elsewhere().cloned()) else {
        return Vec::new();
    };
    let ticket = m.ticket();
    m.peek = Some(Peek::Watch(Watch::new(name.clone(), ticket)));
    vec![Effect::Job(Job::Observe { name, ticket })]
}

/// What the look for other windows means to a float watching a session:
/// ask for its size again while the other window is there, and go back to
/// the pane with a client once it has gone.
fn watch_looked(m: &mut Model) -> Vec<Effect> {
    let ticket = m.ticket();
    let Some(Peek::Watch(watch)) = &mut m.peek else {
        return Vec::new();
    };
    let name = watch.name.clone();
    if !m.company(&name).is_elsewhere() {
        return reattach_preview(m, name);
    }
    match m.watch_mut().and_then(|watch| watch.measure(ticket)) {
        Some(name) => vec![Effect::Job(Job::Observe {
            name: name.clone(),
            ticket,
        })],
        None => Vec::new(),
    }
}

/// What the size found for a watch means: start or restart the watcher,
/// close the float when it can't start, or nothing when the float moved on.
pub fn on_observed(
    m: &mut Model,
    ticket: Ticket,
    result: Result<Observation, Cause>,
) -> Vec<Effect> {
    let Some(watch) = m.watch_mut() else {
        return Vec::new();
    };
    match watch.reply(ticket, result) {
        Reply::Spawn(observation) => vec![Effect::Observe {
            observation,
            ticket,
        }],
        Reply::Failed(why) => {
            if let Some(name) = m.peek_name().cloned() {
                m.error(format!("couldn't peek at {name}: {why}"));
            }
            m.peek = None;
            Vec::new()
        }
        Reply::Ignore => Vec::new(),
    }
}

/// The watcher couldn't be started: answers the size request with a failure.
pub fn observe_failed(m: &mut Model, ticket: Ticket, why: Cause) {
    on_observed(m, ticket, Err(why));
}

/// The float's watcher ended on its own, so the float closes with it.
pub fn watcher_ended(m: &mut Model) {
    let Some((name, mut watcher)) = m.take_watcher() else {
        return;
    };
    m.info(match watcher.term.exit_reason() {
        Some(reason) => format!("peek at {name} ended: {reason}"),
        None => format!("peek at {name} ended"),
    });
}

/// The pane's connection ended on its own, so a float showing it closes
/// with it.
pub fn connection_ended(m: &mut Model) {
    let Some(name) = m.pane.connection().map(|c| c.name.clone()) else {
        return;
    };
    let peeked = matches!(m.peek, Some(Peek::Pane));
    m.set_pane(Pane::Empty);
    m.info(if peeked {
        format!("peek at {name} ended")
    } else {
        format!("detached from {name}")
    });
}

fn attach(m: &mut Model, name: SessionName, intent: Intent) -> Vec<Effect> {
    let ticket = connecting(m, &name);
    vec![Effect::Attach {
        name,
        intent,
        ticket,
    }]
}

/// Whether `name` exited meanwhile, which attaching would resurrect, and
/// the pane has nothing to show then.
fn empty_if_exited(m: &mut Model, name: &SessionName) -> bool {
    let exited = !m.attachable(name);
    if exited {
        m.set_pane(Pane::Empty);
        if m.peek_over(name) {
            m.peek = None;
        }
    }
    exited
}

fn reattach_preview(m: &mut Model, name: SessionName) -> Vec<Effect> {
    if empty_if_exited(m, &name) {
        Vec::new()
    } else {
        attach(m, name, Intent::Preview)
    }
}

/// Notes what the newest look found of every agent, then lets the pane
/// act on what that says of the session it is on.
fn on_elsewhere(
    m: &mut Model,
    ticket: Ticket,
    found: Result<HashMap<SessionName, Company>, Cause>,
) -> Vec<Effect> {
    if !m.is_latest_look(ticket) {
        return Vec::new();
    }
    let sightings = match found {
        Ok(sightings) => {
            m.check_health = CheckHealth::Working;
            sightings
        }
        Err(why) => return check_failed(m, &why),
    };
    let listed: Vec<SessionName> = m
        .snapshot()
        .into_iter()
        .flat_map(|s| s.entries().map(|(_, e)| e.name().clone()))
        .chain(m.pane.session().cloned())
        .collect();
    for name in listed {
        let company = sightings.get(&name).cloned().unwrap_or_default();
        m.record_company(&name, company);
    }
    let mut effects = watch_looked(m);
    effects.extend(pane_looked(m));
    effects
}

fn pane_looked(m: &mut Model) -> Vec<Effect> {
    match &m.pane {
        Pane::Checking { name, then, .. } => {
            let (name, then) = (name.clone(), *then);
            checked(m, name, then)
        }
        Pane::Elsewhere { name } if !m.company(name).is_elsewhere() => {
            let name = name.clone();
            reattach_preview(m, name)
        }
        Pane::Connected(c) if m.peek_client_squishes() => {
            let name = c.name.clone();
            m.set_pane(Pane::Elsewhere { name });
            start_watching(m)
        }
        Pane::Empty | Pane::Connecting { .. } | Pane::Connected(_) | Pane::Elsewhere { .. } => {
            Vec::new()
        }
    }
}

/// Keeps the pane as it is, says so once, and lets a pane still waiting on
/// the answer attach without it.
fn check_failed(m: &mut Model, why: &Cause) -> Vec<Effect> {
    if m.check_health == CheckHealth::Working {
        m.error(format!("couldn't look for other windows: {why}"));
    }
    m.check_health = CheckHealth::Failing;
    match &m.pane {
        Pane::Checking { name, then, .. } => {
            let (name, then) = (name.clone(), *then);
            checked(m, name, then)
        }
        Pane::Elsewhere { name } if !m.company(name).is_elsewhere() => {
            let name = name.clone();
            reattach_preview(m, name)
        }
        Pane::Empty | Pane::Connecting { .. } | Pane::Connected(_) | Pane::Elsewhere { .. } => {
            Vec::new()
        }
    }
}

/// A check that has had its time attaches without it, quietly: slow isn't
/// broken.
fn check_timed_out(m: &mut Model) -> Vec<Effect> {
    let late = match &m.pane {
        Pane::Checking {
            name, then, until, ..
        } if m.clock.mono >= *until => Some((name.clone(), *then)),
        _ => None,
    };
    late.map_or_else(Vec::new, |(name, then)| checked(m, name, then))
}

/// Acts on what the newest look, or the lack of one, left known of `name`.
fn checked(m: &mut Model, name: SessionName, then: AfterCheck) -> Vec<Effect> {
    if empty_if_exited(m, &name) {
        return Vec::new();
    }
    match (m.company(&name).clone(), then) {
        (Company::Alone, _) => attach(m, name, then.intent()),
        (Company::Elsewhere(_), AfterCheck::Preview) => {
            m.set_pane(Pane::Elsewhere { name });
            start_watching(m)
        }
        (Company::Elsewhere(_), AfterCheck::Activate) => stay_elsewhere(m, &name),
    }
}

/// Drops the client of a connection another window shares, once the keys are
/// back in NAV and no peek needs it, so that window gets the full size.
fn pause_if_elsewhere(m: &mut Model) {
    if let Some(name) = m.client_to_release() {
        m.set_pane(Pane::Elsewhere { name });
    }
}

/// Leaves `name`, selected, to its other window and says how to take it.
fn stay_elsewhere(m: &mut Model, name: &SessionName) -> Vec<Effect> {
    m.set_pane(Pane::Elsewhere { name: name.clone() });
    if m.cursor().and_then(Cursor::entry_name) != Some(name) {
        m.set_cursor(Cursor::Entry(name.clone()));
    }
    m.preview_at = None;
    m.peek = None;
    if let Some(windows) = m.company(name).windows() {
        let said = format!("{name}: {}", elsewhere_banner(windows));
        m.info(said);
    }
    Vec::new()
}

/// The look the periodic refresh repeats, for every agent's card, for a
/// newcomer beside a connection, and for the other window going while the
/// pane is left alone. A pane waiting on its own look or a client doesn't
/// ask again, which would drop that look's answer.
pub fn recheck_elsewhere(m: &mut Model) -> Option<Look> {
    match &m.pane {
        Pane::Checking { .. } | Pane::Connecting { .. } => None,
        Pane::Empty | Pane::Connected(_) | Pane::Elsewhere { .. } => {
            Some(looking(m, Need::Refresh))
        }
    }
}

/// Looks for other windows once an action has had time to open one. A pane
/// waiting on its own look or client keeps the ask for the tick after.
fn recheck_elsewhere_when_due(m: &mut Model) -> Option<Effect> {
    m.elsewhere_due.filter(|due| m.clock.mono >= *due)?;
    let look = recheck_elsewhere(m)?;
    m.elsewhere_due = None;
    Some(Effect::CheckElsewhere(look))
}

/// A new look, which makes every look before it stale.
fn looking(m: &mut Model, need: Need) -> Look {
    Look {
        ticket: m.start_look(),
        own: own_client(m),
        need,
    }
}

/// What `⏎`, `0` and `i` do on `name`: move here only from the row whose
/// banner says so.
fn enter_intent(m: &Model, name: &SessionName) -> Intent {
    if m.moves_here().is_some_and(|(held, _)| held == name) {
        Intent::MoveHere
    } else {
        Intent::Activate
    }
}

/// Attaches the selected live agent once the cursor has rested on it.
fn preview(m: &mut Model) -> Option<Effect> {
    let due = m.preview_at.is_some_and(|t| m.clock.mono >= t);
    if !due || matches!(m.focus(), Focus::Agent(_)) {
        return None;
    }
    m.preview_at = None;
    let target = m
        .selected_entry()
        .filter(|e| m.reach(e).is_attach())
        .map(|e| e.name().clone());
    match target {
        Some(name) if m.pane.session() == Some(&name) => None,
        Some(name) => Some(check_first(m, &name, AfterCheck::Preview)),
        None => {
            m.set_pane(Pane::Empty);
            None
        }
    }
}

/// A start or create finished: connect, or say why it failed where the
/// user is looking (the form that asked, or the status bar).
fn on_launched(m: &mut Model, ticket: Ticket, result: Result<Launch, Cause>) -> Vec<Effect> {
    m.launching.retain(|(t, _)| *t != ticket);
    match result {
        Ok(launch) => {
            let now = m.clock.wall;
            m.track(&launch.session).launched(now);
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
            // this connects nothing. Its follow previews it instead.
            vec![Effect::Connect {
                launch,
                intent: Intent::Activate,
                ticket,
            }]
        }
        Err(e) => {
            m.abandon(ticket);
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
        let allowed = m.last_bell.map_or(m.clock.mono, |at| at + BELL_EVERY);
        m.bell_due = Some(m.bell_due.map_or(allowed, |due| due.min(allowed)));
    }
    note_baselines(m, &activities);
    m.activities = Some(activities);
    m.hold_activity_row();
    m.revalidate();
    Vec::new()
}

/// Notes the changed files of every agent whose prompt began an activity, to
/// tell later what the activity touched. An agent first seen working counts.
/// The facts are only a snapshot when they were asked for after the agent's
/// last report before the prompt (or they lack what it did then) and no later
/// than the event that carried the prompt (or they include what this
/// activity did, which can be reported in the same poll).
/// Otherwise nothing is noted.
fn note_baselines(m: &mut Model, next: &HashMap<SessionName, Activity>) {
    let Some(previous) = &m.activities else {
        return;
    };
    let began: Vec<(SessionName, Baseline)> = next
        .iter()
        .filter_map(|(name, now)| {
            let before = previous.get(name);
            let began = match before {
                Some(before) => now.has_new_prompt_since(before),
                None => now.has_taken_a_prompt(),
            };
            began.then(|| (name.clone(), baseline_since(m, name, before, now)))
        })
        .collect();
    for (name, baseline) in began {
        m.track(&name).baseline = baseline;
    }
}

fn baseline_since(
    m: &Model,
    name: &SessionName,
    before: Option<&Activity>,
    now: &Activity,
) -> Baseline {
    let read = m
        .tracked(name)
        .and_then(|t| t.facts.as_ref())
        .and_then(|r| r.result.as_ref().ok());
    let current = read.is_some_and(|r| {
        r.asked_events.is_some_and(|asked| {
            before.is_none_or(|before| asked >= before.events) && asked <= now.prompt_event
        })
    });
    Baseline::of(read.filter(|_| current).map(|r| &r.facts))
}

/// Drops what's known about the subagents and commands their agents no longer
/// list, unless the cursor is still on one.
fn forget_unlisted(m: &mut Model) {
    let Some(activities) = &m.activities else {
        return;
    };
    let (subagent, command) = match m.panel.activity_row() {
        Some(ActivityRow::Subagent { by, id }) => (Some((by, id)), None),
        Some(ActivityRow::Command { by, id }) => (None, Some((by, id))),
        _ => (None, None),
    };
    for (name, tracked) in &mut m.agents {
        tracked.subagents.retain(|id, _| {
            subagent == Some((name, id))
                || activities
                    .get(name)
                    .is_some_and(|a| a.subagents.contains_key(id))
        });
        tracked.command_outputs.retain(|id, _| {
            command == Some((name, id))
                || activities
                    .get(name)
                    .and_then(Activity::commands)
                    .is_some_and(|commands| commands.get(id).is_some())
        });
    }
}

/// Asks for what [3]'s cursor is on once it has rested there.
fn read_document(m: &mut Model) -> Option<Effect> {
    let (key, worktree) = m.hovered_document()?;
    let due = m.docs.due(m.clock.mono)?;
    debug_assert_eq!(due, key);
    let reuse = match &due {
        Key::Folder(folder) => {
            m.docs
                .cached_diffs(&folder.name, folder.base, folder.epoch, &folder.files)
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
/// shown, soon after its agent reports something, and every few seconds
/// while it runs.
fn read_subagent(m: &mut Model) -> Option<Effect> {
    let (name, id) = m.subagent_selected()?;
    let workdir = selected_root(m)?;
    let events = m.activity(&name)?.events;
    let now = m.clock.mono;
    let watched = m.watched(&(name.clone(), id.clone()));
    if matches!(watched.and_then(|w| w.watch.log.as_ref()), Some(Ok(Some(detail))) if matches!(detail.outcome, agent_core::ports::Outcome::Finished { .. }))
    {
        return None;
    }
    if !watched.is_none_or(|w| w.watch.due(events, true, now)) {
        return None;
    }
    m.track(&name)
        .subagents
        .entry(id.clone())
        .or_default()
        .watch
        .asked(events, now);
    Some(Effect::ReadSubagent { name, id, workdir })
}

/// Asks for what the background command [0] shows has printed: when it is
/// first shown, and every few seconds while it stays shown.
fn read_command_output(m: &mut Model) -> Option<Effect> {
    let Some(crate::document::Hover::Command(name, id)) = m.hover() else {
        return None;
    };
    let workdir = selected_root(m)?;
    let at = m.activity(&name)?.transcript.clone();
    let presence = m.activity_list().command(&id)?.presence;
    let now = m.clock.mono;
    if !m
        .tracked(&name)
        .and_then(|t| t.command_outputs.get(&id))
        .is_none_or(|watch| watch.due(presence, now))
    {
        return None;
    }
    let ticket = m.ticket();
    m.track(&name)
        .command_outputs
        .entry(id.clone())
        .or_default()
        .ask(ticket, presence, now);
    Some(Effect::ReadCommandOutput(OutputRequest {
        name,
        id,
        ticket,
        workdir,
        at,
    }))
}

/// Asks for the current activity of the agent whose activity [0] shows: when it is
/// first shown, soon after the agent reports something, and every few
/// seconds while it works.
fn read_activity(m: &mut Model) -> Option<Effect> {
    let Some(crate::document::Hover::Activity(name)) = m.hover() else {
        return None;
    };
    let workdir = selected_root(m)?;
    let activity = m.activity(&name)?;
    let (events, at) = (activity.events, activity.transcript.clone());
    let now = m.clock.mono;
    let growing = m.activity_running(&name);
    if !m
        .tracked(&name)
        .is_none_or(|t| t.activity_view.watch.due(events, growing, now))
    {
        return None;
    }
    m.track(&name).activity_view.watch.asked(events, now);
    Some(Effect::ReadActivity { name, workdir, at })
}

/// Notes the agent in front of the user as seen once it has been there for
/// `after`, with the terminal focused: its activity in [0], or its terminal in
/// the pane. Only the peek float shows a terminal over an activity, and what it
/// covers isn't in front of them, so the pane wins. What the agent did since
/// is new to them, and a finished activity they haven't looked at is
/// acknowledged, so the `✓` and the `since you looked` marker clear together.
fn look(m: &mut Model, after: Duration) -> Option<Job> {
    let now = m.clock.mono;
    let focused = m.terminal != TerminalFocus::Unfocused;
    let viewing = match m.hover() {
        Some(crate::document::Hover::Activity(name)) if focused => Some(name),
        _ => None,
    };
    for (name, tracked) in &mut m.agents {
        let looked = &mut tracked.activity_view.looked;
        if viewing.as_ref() == Some(name) {
            looked.since.get_or_insert(now);
        } else {
            looked.since = None;
        }
    }
    let steady = viewing.filter(|name| {
        m.tracked(name)
            .and_then(|t| t.activity_view.looked.since)
            .is_some_and(|since| now.saturating_duration_since(since) >= after)
    });
    let shown = match &m.peek {
        Some(Peek::Watch(Watch {
            name,
            view: WatchView::Watched(w),
        })) => Some((name, w.opened)),
        Some(
            Peek::Pane
            | Peek::Watch(Watch {
                view: WatchView::Watching(_),
                ..
            }),
        )
        | None => m.pane.connection().map(|c| (&c.name, c.opened)),
    };
    let watching = shown
        .filter(|(_, opened)| now.saturating_duration_since(*opened) >= after)
        .filter(|_| m.terminal_visible())
        .map(|(name, _)| name.clone());
    let front = watching.or(steady).filter(|_| focused);
    if let Some(name) = &front {
        let wall = m.clock.wall;
        m.track(name).activity_view.looked.seen = Some(wall);
    }
    let name = front?;
    let activity = m.activity(&name)?;
    let State::Done { .. } = activity.state else {
        return None;
    };
    let mark = (name.clone(), activity.events);
    if m.acknowledged.as_ref() == Some(&mark) {
        return None;
    }
    m.acknowledged = Some(mark);
    Some(Job::Acknowledge(name))
}

/// Drops what's known about entries that are no longer in the snapshot.
fn forget_gone(m: &mut Model) {
    let Some(snapshot) = m.snapshot() else {
        return;
    };
    let known: HashSet<SessionName> = snapshot.entries().map(|(_, e)| e.name().clone()).collect();
    let now = m.clock.wall;
    for (name, tracked) in &mut m.agents {
        if known.contains(name) {
            tracked.listed();
        }
    }
    let gone: Vec<SessionName> = m
        .agents
        .iter()
        .filter(|(name, t)| !known.contains(*name) && !t.seen.pending(now))
        .map(|(name, _)| name.clone())
        .collect();
    for name in gone {
        let Some(tracked) = m.agents.remove(&name) else {
            continue;
        };
        let drafted = tracked.draft.len();
        if matches!(tracked.seen, Seen::Pending { .. }) && drafted > 0 {
            m.error(format!(
                "{name} never started · dropped {drafted} drafted for it"
            ));
        }
        if matches!(&m.overlay, Overlay::Talk(open) if *open == name) {
            m.overlay = Overlay::None;
        }
        if tracked.compose.as_ref().is_some_and(Compose::holds_work) {
            m.error(format!("{name} is gone · dropped the message for it"));
        }
    }
    if m.peek_name().is_some_and(|name| !known.contains(name)) {
        m.peek = None;
    }
    // A watcher exiting with its session was only verified on one session host
    // version, so the snapshot closes the float too.
    let ended = match &m.peek {
        Some(Peek::Watch(watch)) if !m.attachable(&watch.name) => Some(watch.name.clone()),
        _ => None,
    };
    if let Some(name) = ended {
        m.peek = None;
        m.info(format!("peek at {name} ended"));
    }
    if m.pane
        .held_elsewhere()
        .is_some_and(|name| !known.contains(name))
    {
        m.set_pane(Pane::Empty);
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
    let now = m.clock.mono;
    let due: Vec<_> = snapshot
        .entries()
        .filter(|(_, e)| e.head().is_some())
        .filter_map(|(_, entry)| {
            let events = m.activity(entry.name()).map(|a| a.events);
            let failing = matches!(m.facts(entry.name()), Some(Err(_)));
            let due = m
                .tracked(entry.name())
                .and_then(|t| t.inspection)
                .is_none_or(|last| {
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
            m.track(&name).inspection = Some(Inspection { events, at: now });
            Effect::Inspect {
                name,
                worktree,
                events,
            }
        })
        .collect()
}

/// Rings the queued bell once the rate limit allows.
fn bell(m: &mut Model) -> Option<Effect> {
    m.bell_due.filter(|due| m.clock.mono >= *due)?;
    m.bell_due = None;
    m.last_bell = Some(m.clock.mono);
    Some(Effect::Bell)
}

/// Whether an agent newly needs the user: asking, failed, or finished a
/// activity that took a while. Not the agent being typed into or the TUI's own
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
                || match &activity.state {
                    State::Failed { .. } => {
                        !matches!(before.map(|b| &b.state), Some(State::Failed { .. }))
                    }
                    State::Done { .. } => match before.map(|b| (&b.state, b.since)) {
                        Some((State::Working { .. } | State::Delegating, started)) => {
                            activity.since.duration_since(started).unwrap_or_default() >= LONG_TURN
                        }
                        _ => false,
                    },
                    State::Idle
                    | State::Working { .. }
                    | State::Delegating
                    | State::Interrupted
                    | State::Exited => false,
                }
        })
}

/// What arriving in AGENT does to the session's other windows.
#[derive(Clone, Copy)]
enum Others {
    Keep,
    Close,
}

/// Keys now go to the connected agent.
fn enter_agent(m: &mut Model) -> Vec<Effect> {
    enter(m, Others::Keep)
        .into_iter()
        .map(Effect::Job)
        .collect()
}

/// Closing the other windows moves the session here, so it takes this
/// pane's size.
fn enter(m: &mut Model, others: Others) -> Vec<Job> {
    let own = own_client(m);
    let Some(c) = m.pane.connection_mut() else {
        return Vec::new();
    };
    c.mode = Mode::Agent;
    let name = c.name.clone();
    m.peek = None;
    m.touch_mru(&name);
    m.preview_at = None;
    let mut jobs = Vec::new();
    if let Others::Close = others {
        jobs.push(Job::CloseElsewhere { name, own });
    }
    jobs.extend(look(m, Duration::ZERO));
    jobs
}

/// What a new connection starts: the AGENT mode the user asked for, the
/// closing of other windows once it exists, and the look for newcomers.
pub fn connected(m: &mut Model, intent: Intent) -> Vec<Background> {
    let (jobs, looks) = match intent {
        Intent::Preview => (Vec::new(), true),
        Intent::Activate => (enter(m, Others::Keep), true),
        Intent::MoveHere => {
            let jobs = enter(m, Others::Close);
            moved_here(m);
            (jobs, false)
        }
    };
    let look = looks.then(|| recheck_elsewhere(m)).flatten();
    jobs.into_iter()
        .map(Background::Job)
        .chain(look.map(Background::Look))
        .collect()
}

/// The other windows are being closed, so the session is alone now. Looks
/// asked before this may still see them, and are dropped.
fn moved_here(m: &mut Model) {
    if let Some(name) = m.pane.connection().map(|c| c.name.clone()) {
        m.record_company(&name, Company::Alone);
        m.start_look();
    }
}

/// Types into `name`, or moves here when `intent` is `MoveHere`.
fn activate(m: &mut Model, name: &SessionName, intent: Intent) -> Vec<Effect> {
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
    let enter = match reach {
        Reach::Own => {
            m.error(format!("this TUI is running inside {name}"));
            return Vec::new();
        }
        Reach::Restarting => {
            m.info(format!("{name} is restarting"));
            return Vec::new();
        }
        Reach::Other { enter, .. } => enter,
    };
    if m.elsewhere(name) {
        if matches!(enter, Enter::Attach) {
            return match intent {
                Intent::MoveHere => attach(m, name.clone(), Intent::MoveHere),
                Intent::Activate | Intent::Preview => stay_elsewhere(m, name),
            };
        }
        m.set_pane(Pane::Empty);
    }
    if m.pane.connection().is_some_and(|c| &c.name == name) {
        return enter_agent(m);
    }
    match enter {
        Enter::Attach => vec![check_first(m, name, AfterCheck::Activate)],
        Enter::Resurrect => {
            let ticket = starting(m, name);
            vec![Effect::Job(Job::Resurrect {
                name: name.clone(),
                ticket,
            })]
        }
        Enter::Start(at) => {
            let ticket = starting(m, name);
            m.info(format!("starting {name}…"));
            vec![Effect::Job(Job::Start {
                at,
                harness: m.harness.clone(),
                ticket,
            })]
        }
        Enter::Refused(why) => {
            m.error(why);
            Vec::new()
        }
        Enter::Restart(at) => {
            let ticket = starting(m, name);
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
    if let Some(c) = m.pane.connection().filter(|c| c.mode == Mode::Agent) {
        if pressed(&key, nav) {
            let name = c.name.clone();
            m.pane.set_nav();
            m.chord = Chord::Armed;
            if m.selected_entry().is_none_or(|e| e.name() != &name) {
                m.set_cursor(Cursor::Entry(name));
                m.preview_at = None;
            }
        } else {
            return vec![Effect::Pty {
                to: c.name.clone(),
                bytes: encode_key(key, c.term.modes()),
            }];
        }
        return Vec::new();
    }
    if m.peek.is_none() {
        let armed = std::mem::replace(&mut m.chord, Chord::Idle) == Chord::Armed;
        if pressed(&key, m.keys.nav()) {
            return leave_pane(m, armed);
        }
    }
    nav_or_peek_command(m, &key).unwrap_or_default()
}

const SEND_MOVED: &str = "send is s now";

/// The peek float reads the terminal and passes nothing to it.
/// What `code` does in the peek, or `None` when nothing is bound to it.
fn peek_command(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    let name = m.peek_name()?.clone();
    match code {
        KeyCode::Esc | KeyCode::Char('v') => m.peek = None,
        KeyCode::Char('s') => open_talk(m, &name, None),
        KeyCode::Char('t') => m.info(SEND_MOVED),
        KeyCode::Char('i') | KeyCode::Enter => return Some(activate(m, &name, Intent::Activate)),
        _ => return None,
    }
    Some(Vec::new())
}

/// `e` on a subagent row: its whole transcript, in the agent's checkout.
fn edit_transcript(m: &mut Model) -> Option<Vec<Effect>> {
    let key = m.subagent_selected()?;
    let root = selected_root(m)?;
    match m.watched(&key).and_then(|w| w.watch.log.as_ref()) {
        Some(Ok(Some(detail))) if !detail.transcript.as_str().is_empty() => {
            Some(vec![Effect::Edit(Open::File {
                root,
                file: detail.transcript.path().to_path_buf(),
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
pub fn commit_rev(m: &Model) -> Option<crate::review::Rev> {
    match m.hover() {
        Some(crate::document::Hover::Commit(key)) => Some(crate::review::Rev::new(key.sha)),
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
    rev: Option<crate::review::Rev>,
) {
    let refs: Vec<&crate::review::LineInfo> = lines.iter().collect();
    let Some(comment) = crate::review::Comment::on(&refs, rev.as_ref(), text.trim()) else {
        return;
    };
    if let Some(c) = agent_core::names::first_control(text) {
        m.info(format!(
            "the comment has a control character ({c:?}) · Esc closes"
        ));
        m.overlay = Overlay::Comment(CommentDraft {
            to: to.clone(),
            lines: lines.to_vec(),
            text: Message::from(text),
            rev,
        });
        return;
    }
    let at = m.draft(to).iter().position(|c| c.same_place(&comment));
    match (at, text.trim().is_empty()) {
        (None, true) => {
            m.info("type a comment first · Esc closes");
            m.overlay = Overlay::Comment(CommentDraft {
                to: to.clone(),
                lines: lines.to_vec(),
                text: Message::default(),
                rev,
            });
            return;
        }
        (Some(i), true) => {
            m.track(to).draft.remove(i);
        }
        (Some(i), false) => m.track(to).draft[i] = comment,
        (None, false) => m.track(to).draft.push(comment),
    }
    m.diff_view.anchor = None;
    let n = m.draft(to).len();
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
        .draft(&name)
        .iter()
        .find(|c| c.covers(first, rev.as_ref()));
    m.overlay = Overlay::Comment(CommentDraft {
        to: name,
        text: existing.map_or_else(Message::default, |c| Message::from(c.text.as_str())),
        lines,
        rev,
    });
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
                crate::review::LineKind::Entry(state) => crate::model::Opened::File {
                    path: line.path.clone(),
                    state,
                },
                crate::review::LineKind::Header
                | crate::review::LineKind::Added
                | crate::review::LineKind::Removed
                | crate::review::LineKind::Context => return Some(effects),
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
                crate::review::LineKind::Header
                | crate::review::LineKind::Entry(_)
                | crate::review::LineKind::Added
                | crate::review::LineKind::Removed
                | crate::review::LineKind::Context => line.path.to_string_lossy().into_owned(),
            };
            return Some(copy(m, text));
        }
        KeyCode::Char('e') => {
            let line = lines.get(at)?;
            if !matches!(line.kind, crate::review::LineKind::Entry(_)) {
                return None;
            }
            let root = selected_root(m)?;
            effects.push(Effect::Edit(Open::File {
                root,
                file: line.path.clone(),
            }));
            return Some(effects);
        }
        KeyCode::PageDown | KeyCode::PageUp => {
            let half = m.doc_half_page() * 2;
            m.docs.scroll_by(if code == KeyCode::PageDown {
                half
            } else {
                -half
            });
            m.follow_page();
            return Some(effects);
        }
        _ => return None,
    };
    if let Some(to) = to {
        m.diff_view.cursor = to;
        m.reveal_line();
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
    let root = selected_root(m)?;
    let line = m.docs.lines().get(m.diff_view.cursor)?;
    Some(vec![Effect::Edit(Open::At {
        root,
        file: line.path.clone(),
        line: line.edit,
    })])
}

/// `J` and `K` in a diff: the next or previous file, in [3] when [0] shows
/// one file and in the diff itself when it shows several.
fn jump_file(m: &mut Model, direction: Direction) -> Vec<Effect> {
    let none = match direction {
        Direction::Forward => "last file",
        Direction::Back => "first file",
    };
    if matches!(m.hover(), Some(crate::document::Hover::Diff(_))) {
        if !m.step_file(direction) {
            m.info(none);
        }
        return Vec::new();
    }
    let at = m.diff_view.cursor;
    match crate::review::file(m.docs.lines(), at, direction) {
        Some(to) => {
            m.diff_view.cursor = to;
            m.diff_view.anchor = None;
            m.reveal_line();
        }
        None => m.info(none),
    }
    Vec::new()
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
        KeyCode::Char(']') => crate::review::hunk(lines, at, Direction::Forward),
        KeyCode::Char('[') => crate::review::hunk(lines, at, Direction::Back),
        KeyCode::PageDown | KeyCode::PageUp => {
            let half = m.doc_half_page() * 2;
            m.docs.scroll_by(if code == KeyCode::PageDown {
                half
            } else {
                -half
            });
            m.follow_page();
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
                crate::review::label(&line.path, commit_rev(m).as_ref(), &[line])
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
        KeyCode::Char('J') => return Some(jump_file(m, Direction::Forward)),
        KeyCode::Char('K') => return Some(jump_file(m, Direction::Back)),
        KeyCode::Char('e') => return edit_line(m),
        _ => return None,
    };
    if let Some(to) = to {
        m.diff_view.cursor = to;
        m.reveal_line();
    }
    Some(Vec::new())
}

/// Scrolls [0] so the log entry `entry` of `key` shows, as the last draw
/// laid it out.
fn reveal_entry(areas: &Areas, docs: &mut Documents, key: &LogKey, entry: &EntryKey) {
    if let Some(main) = areas.main.as_ref().filter(|a| a.nav.as_ref() == Some(key)) {
        main.reveal_entry(docs, entry);
    }
}

/// The checkout, title and file name for writing the log `key` names out
/// for `$EDITOR`.
fn transcript_target(m: &Model, key: &LogKey) -> Option<(PathBuf, String, String)> {
    let root = selected_root(m)?;
    Some(match key {
        LogKey::Subagent((name, id)) => {
            let title = m.subagent_label(id).unwrap_or("subagent").to_string();
            (root, title, format!("{}-{}", name.as_str(), id.as_str()))
        }
        LogKey::Activity(name) => (
            root,
            format!("Activity · {name}"),
            format!("{}-activity", name.as_str()),
        ),
    })
}

/// Takes [0] where the entry `⏎` was pressed on points, and says why when
/// it can't.
fn follow(m: &mut Model, jump: crate::log::Jump) -> Vec<Effect> {
    let Some(spot) = m.activity_spot() else {
        return Vec::new();
    };
    let result = match jump {
        crate::log::Jump::Subagent(id) => m.open_subagent(spot, &id),
        crate::log::Jump::File(path) => m.open_diff(spot, &path),
    };
    if let Err(missing) = result {
        m.info(missing.to_string());
    }
    Vec::new()
}

/// How a key that moves the cursor over a log moves the document with it.
enum Motion {
    Down { follow: bool },
    Up,
    Start,
    End,
}

/// What a key does to an open log, worked out while the log is borrowed.
enum Step {
    Moved { to: EntryKey, motion: Motion },
    Stayed,
    Page,
    Follow(crate::log::Jump),
    Folded,
    Copy(String),
    Edit(Vec<Effect>),
}

/// Keys for [0] while it reads a log, a subagent's or the current activity: the
/// entry cursor, folds, the transcript in the editor and, in an activity, `⏎`
/// on a call or file that leads somewhere. `None` leaves the key to the
/// plain document keys.
fn read_log(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    let key = m.open_log()?;
    let editing = if code == KeyCode::Char('e') {
        transcript_target(m, &key)
    } else {
        None
    };
    let mut nav = m.nav(&key)?.clone();
    let step = {
        let contents = m.contents(&key)?;
        let at = contents.at(&nav)?;
        key_step(code, &contents, at, &mut nav, editing)?
    };
    *m.nav_mut(&key)? = nav;
    Some(match step {
        Step::Moved { to, motion } => {
            match motion {
                Motion::Down { follow } => {
                    if follow {
                        m.docs.scroll_to_end();
                    }
                    reveal_entry(&m.areas, &mut m.docs, &key, &to);
                }
                Motion::Up => {
                    reveal_entry(&m.areas, &mut m.docs, &key, &to);
                    m.docs.pause();
                }
                Motion::Start => m.docs.scroll_to_start(),
                Motion::End => m.docs.scroll_to_end(),
            }
            Vec::new()
        }
        Step::Stayed | Step::Folded => Vec::new(),
        Step::Page => {
            let half = m.doc_half_page() * 2;
            m.docs.scroll_by(if code == KeyCode::PageDown {
                half
            } else {
                -half
            });
            m.follow_page();
            Vec::new()
        }
        Step::Follow(jump) => follow(m, jump),
        Step::Copy(text) => copy(m, text),
        Step::Edit(effects) => effects,
    })
}

fn key_step(
    code: KeyCode,
    contents: &crate::log::Contents<'_>,
    at: usize,
    nav: &mut Nav,
    editing: Option<(PathBuf, String, String)>,
) -> Option<Step> {
    let last = contents.last()?;
    let entries = &contents.entries;
    let down = |to: usize| Motion::Down {
        follow: contents.running && to >= last,
    };
    let (to, motion) = match code {
        KeyCode::Char('j') | KeyCode::Down => {
            let to = (at + 1).min(last);
            (to, down(to))
        }
        KeyCode::Char('k') | KeyCode::Up => (at.saturating_sub(1), Motion::Up),
        KeyCode::Char('g') | KeyCode::Home => (0, Motion::Start),
        KeyCode::Char('G') | KeyCode::End => (last, Motion::End),
        KeyCode::Char(']') => match Direction::Forward.next_start(&contents.phases(), at) {
            Some(to) => (to, down(to)),
            None => return Some(Step::Stayed),
        },
        KeyCode::Char('[') => match Direction::Back.next_start(&contents.phases(), at) {
            Some(to) => (to, Motion::Up),
            None => return Some(Step::Stayed),
        },
        KeyCode::PageDown | KeyCode::PageUp => return Some(Step::Page),
        KeyCode::Enter => {
            let entry_key = entries[at].1.clone();
            nav.cursor = Some(entry_key.clone());
            if let Some(jump) = contents.jump() {
                return Some(Step::Follow(jump.clone()));
            }
            nav.toggle(entry_key);
            return Some(Step::Folded);
        }
        KeyCode::Char('=') => {
            nav.all = true;
            return Some(Step::Folded);
        }
        KeyCode::Char('-') => {
            nav.all = false;
            nav.open.clear();
            return Some(Step::Folded);
        }
        KeyCode::Char('y') => {
            nav.cursor = Some(entries[at].1.clone());
            return Some(Step::Copy(crate::log::entry_text(&entries[at].0)));
        }
        KeyCode::Char('e') => return Some(Step::Edit(edit_log(&contents.source, editing?, at))),
        _ => return None,
    };
    let to_key = entries[to].1.clone();
    nav.cursor = Some(to_key.clone());
    Some(Step::Moved { to: to_key, motion })
}

/// `s` on a log entry: the send box quoting it. `None` leaves the key to
/// the plain send.
fn reply_to_entry(m: &mut Model) -> Option<Vec<Effect>> {
    let ReplyTo { to, quote } = m.reply.clone()?;
    open_talk(m, &to, Some(quote));
    Some(Vec::new())
}

/// Opens the log as Markdown at its entry number `at`.
fn edit_log(
    source: &crate::log::Source<'_>,
    (root, title, name): (PathBuf, String, String),
    at: usize,
) -> Vec<Effect> {
    let (text, starts) = source.markdown(&title);
    let line = starts.get(at).copied().unwrap_or(NonZeroUsize::MIN);
    vec![Effect::Edit(Open::Transcript {
        root,
        name,
        text,
        line,
    })]
}

/// Control characters other than newlines and tabs could end a bracketed
/// paste early and run the rest as keystrokes.
fn sanitize_paste(text: &str) -> String {
    clean(text, CarriageReturn::Keep, None)
}

/// Keys that choose the panel `j`/`k` act on, and those a panel gives its
/// own meaning. `None` leaves the key to NAV.
fn on_panel_key(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    if m.main_focused() {
        return on_document_key(m, code);
    }
    if let Some(effects) = focus_key(m, code) {
        return Some(effects);
    }
    match m.panel.list() {
        Listed::Agents => agents_key(m, code),
        Listed::Agent { .. } => activity_key(m, code),
        Listed::Changes { .. } => changes_key(m, code),
    }
}

/// [0]'s keys. The panel numbers hand the keys to a list panel, so they
/// leave [0] first.
fn on_document_key(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    if code == KeyCode::Char('y')
        && let Some(crate::document::Hover::Activity(name)) = m.hover()
        && !m.activity_has_log(&name)
    {
        let message = m
            .selected_entry()
            .and_then(|e| crate::view::activity_message(m, e));
        let Some(text) = message else {
            m.info(format!("{name} has no message to copy"));
            return Some(Vec::new());
        };
        return Some(copy(m, text));
    }
    if code == KeyCode::Esc
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
    if code == KeyCode::Esc && m.opened.take().is_some() {
        m.diff_view.owner = None;
        return Some(Vec::new());
    }
    if code == KeyCode::Esc
        && let Some(spot) = m.panel.back().cloned()
    {
        m.changes_scope = spot.scope;
        m.panel = spot.panel();
        return Some(Vec::new());
    }
    if code == KeyCode::Char('s') {
        return reply_to_entry(m);
    }
    if let Some(effects) = read_log(m, code)
        .or_else(|| diff_key(m, code))
        .or_else(|| branch_key(m, code))
    {
        return Some(effects);
    }
    match code {
        KeyCode::Esc => m.panel.leave_document(),
        KeyCode::Char('1' | '2' | '3') => {
            m.panel.leave_document();
            return focus_key(m, code);
        }
        KeyCode::Char('j') | KeyCode::Down => m.docs.scroll_by(1),
        KeyCode::Char('k') | KeyCode::Up => m.docs.scroll_by(-1),
        KeyCode::Char('g') | KeyCode::Home => m.docs.scroll_to_start(),
        KeyCode::Char('G') | KeyCode::End => m.docs.scroll_to_end(),
        KeyCode::PageDown => m.docs.scroll_by(m.doc_half_page() * 2),
        KeyCode::PageUp => m.docs.scroll_by(-m.doc_half_page() * 2),
        KeyCode::Enter => {}
        _ => return None,
    }
    Some(Vec::new())
}

/// The keys that move the keys between the panels.
fn focus_key(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    match (code, m.panel.list()) {
        (KeyCode::Esc | KeyCode::Char('1'), Listed::Agent { .. } | Listed::Changes { .. }) => {
            m.panel = Panel::Agents;
        }
        (KeyCode::Char('1'), Listed::Agents) => {}
        (KeyCode::Char('2'), _) if m.selected_entry().is_none() => {
            m.info("select an agent to see it");
        }
        (KeyCode::Char('2'), _)
            if m.selected_entry()
                .is_some_and(|e| m.activity(e.name()).is_none()) =>
        {
            m.info("no activity recorded · r restarts it through agent");
        }
        (KeyCode::Char('2'), _) => m.panel = Panel::activity(),
        (KeyCode::Char('3'), _) if m.change_rows() == 0 => m.info("no files to move to"),
        (KeyCode::Char('3'), _) if m.require_room().is_err() => {
            m.info(Missing::Room.to_string());
        }
        (KeyCode::Char('3'), _) => m.panel = Panel::Changes { file: 0 },
        _ => return None,
    }
    Some(Vec::new())
}

/// Opens the document under the list panel's cursor.
fn open_document(m: &mut Model) -> Vec<Effect> {
    m.focus_document();
    m.opened = None;
    Vec::new()
}

fn agents_key(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    match code {
        KeyCode::Char('0') => m.selected_entry().map(|e| e.name().clone()).map(|name| {
            let intent = enter_intent(m, &name);
            activate(m, &name, intent)
        }),
        _ => None,
    }
}

fn activity_key(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    match code {
        KeyCode::Enter | KeyCode::Char('0') if m.hover().is_some() => Some(open_document(m)),
        KeyCode::Char('e') => edit_transcript(m),
        _ => None,
    }
}

fn changes_key(m: &mut Model, code: KeyCode) -> Option<Vec<Effect>> {
    match code {
        KeyCode::Enter if m.folder_under_cursor().is_some() => m.toggle_folder(),
        KeyCode::Char('`') => m.toggle_changes_view(),
        KeyCode::Char('-' | '=') if m.changes_view == ChangesView::Flat => {
            m.info("the flat list has no folders · ` shows the tree");
        }
        KeyCode::Char(c @ ('-' | '=')) => m.fold_all(c == '-'),
        KeyCode::Char('e') if m.folder_under_cursor().is_some() => {
            let root = m
                .selected_entry()
                .and_then(|e| e.path())
                .map(Path::to_path_buf);
            let folder = m.folder_under_cursor().map(|r| r.path);
            let Some(dir) = root.zip(folder).map(|(root, folder)| root.join(folder)) else {
                m.info("that folder is gone from the checkout");
                return Some(Vec::new());
            };
            return Some(vec![Effect::Edit(Open::Checkout(dir))]);
        }
        KeyCode::Enter | KeyCode::Char('0') if m.hover().is_some() => {
            return Some(open_document(m));
        }
        KeyCode::Char('e') => {
            return m
                .file_under_cursor()
                .map(|(root, file)| vec![Effect::Edit(Open::File { root, file })]);
        }
        _ => return None,
    }
    Some(Vec::new())
}

fn has_tabs(m: &Model) -> bool {
    matches!(m.panel, Panel::Agents | Panel::Changes { .. })
}

fn cycle_tab(m: &mut Model, direction: Direction) {
    if m.panel == Panel::Agents {
        m.cycle_view(direction);
    } else if matches!(m.panel, Panel::Changes { .. }) {
        m.toggle_changes_scope();
    }
}

/// The left panels in the order `h` and `l` cycle them.
#[derive(Clone, Copy)]
enum Slot {
    Agents,
    Activity,
    Changes,
}

impl Slot {
    const ALL: [Self; 3] = [Self::Agents, Self::Activity, Self::Changes];

    fn digit(self) -> char {
        match self {
            Self::Agents => '1',
            Self::Activity => '2',
            Self::Changes => '3',
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Agents => 0,
            Self::Activity => 1,
            Self::Changes => 2,
        }
    }
}

fn panel_focusable(m: &Model, slot: Slot) -> bool {
    match slot {
        Slot::Agents => true,
        Slot::Activity => m
            .selected_entry()
            .is_some_and(|e| m.activity(e.name()).is_some()),
        Slot::Changes => m.change_rows() > 0 && m.work_room != WorkRoom::TooShort,
    }
}

fn cycle_panel(m: &mut Model, direction: Direction) -> Vec<Effect> {
    let current = match m.panel.list() {
        Listed::Agent { .. } => Slot::Activity,
        Listed::Changes { .. } => Slot::Changes,
        Listed::Agents => Slot::Agents,
    };
    let step = match direction {
        Direction::Forward => 1,
        Direction::Back => 2,
    };
    let target = (1..=Slot::ALL.len())
        .map(|k| Slot::ALL[(current.index() + k * step) % Slot::ALL.len()])
        .find(|slot| panel_focusable(m, *slot))
        .unwrap_or(Slot::Agents);
    on_panel_key(m, KeyCode::Char(target.digit())).unwrap_or_default()
}

/// What `key` does in the peek while one is open, else in NAV, or `None`
/// when nothing is bound to it there.
pub(crate) fn nav_or_peek_command(m: &mut Model, key: &KeyEvent) -> Option<Vec<Effect>> {
    if m.peek.is_some() {
        peek_command(m, key.code)
    } else {
        nav_command(m, key)
    }
}

/// What `key` does in NAV, or `None` when nothing is bound to it here.
fn nav_command(m: &mut Model, key: &KeyEvent) -> Option<Vec<Effect>> {
    let selected = m.selected_entry().cloned();
    on_panel_key(m, key.code)
        .or_else(|| scroll_key(m, key))
        .or_else(|| view_key(m, key))
        .or_else(|| overlay_key(m, key, selected.as_ref()))
        .or_else(|| action_key(m, key, selected))
}

/// The nav key in NAV: the agent the keys last left, if it follows a nav
/// key from AGENT, else the one under the cursor.
fn leave_pane(m: &mut Model, armed: bool) -> Vec<Effect> {
    if armed {
        let current = m.pane.session().cloned();
        let previous = m
            .mru()
            .iter()
            .find(|n| Some(*n) != current.as_ref())
            .cloned();
        if let Some(name) = previous {
            return activate(m, &name, Intent::Activate);
        }
    }
    enter_agent(m)
}

fn scroll_key(m: &mut Model, key: &KeyEvent) -> Option<Vec<Effect>> {
    match key.code {
        KeyCode::Char('d') if ctrl(key, 'd') => half_page(m, Direction::Forward),
        KeyCode::Char('u') if ctrl(key, 'u') => half_page(m, Direction::Back),
        KeyCode::Char('J') if m.hover().is_some() => m.docs.scroll_by(SCROLL),
        KeyCode::Char('K') if m.hover().is_some() => m.docs.scroll_by(-SCROLL),
        KeyCode::PageDown => m.page_in_panel(m.half_page() * 2),
        KeyCode::PageUp => m.page_in_panel(-m.half_page() * 2),
        KeyCode::Char('j') | KeyCode::Down => m.move_in_panel(1),
        KeyCode::Char('k') | KeyCode::Up => m.move_in_panel(-1),
        KeyCode::Char('g') | KeyCode::Home => m.panel_to_edge(Edge::First),
        KeyCode::Char('G') | KeyCode::End => m.panel_to_edge(Edge::Last),
        _ => return None,
    }
    Some(Vec::new())
}

fn half_page(m: &mut Model, direction: Direction) {
    let sign = match direction {
        Direction::Forward => 1,
        Direction::Back => -1,
    };
    match m.panel {
        Panel::Changes { .. } | Panel::Agent { .. } | Panel::Main(..) => {
            m.docs.scroll_by(sign * m.doc_half_page());
            if m.main_focused() {
                m.follow_page();
            }
        }
        Panel::Agents => m.page_in_panel(sign * m.half_page()),
    }
}

/// Keys that change what the lists show or which panel has the keys.
fn view_key(m: &mut Model, key: &KeyEvent) -> Option<Vec<Effect>> {
    match key.code {
        KeyCode::Char('}') => m.jump_repo(1),
        KeyCode::Char('{') => m.jump_repo(-1),
        KeyCode::Char(']') if has_tabs(m) => cycle_tab(m, Direction::Forward),
        KeyCode::Char('[') if has_tabs(m) => cycle_tab(m, Direction::Back),
        KeyCode::Tab | KeyCode::Char('l') => return Some(cycle_panel(m, Direction::Forward)),
        KeyCode::BackTab | KeyCode::Char('h') => return Some(cycle_panel(m, Direction::Back)),
        KeyCode::Char('+') => m.screen = Screen::Full,
        KeyCode::Char('_') => m.screen = Screen::Split,
        KeyCode::Char('.') if m.view == View::Pinned => {
            m.info("pinned lists every pin · . filters the repo and agents tabs");
        }
        KeyCode::Char('.') => m.visibility = m.visibility.toggled(),
        _ => return None,
    }
    Some(Vec::new())
}

/// Keys that open an overlay or a form.
fn overlay_key(m: &mut Model, key: &KeyEvent, selected: Option<&Entry>) -> Option<Vec<Effect>> {
    match key.code {
        KeyCode::Char('p') => m.overlay = Overlay::Picker(Query::default()),
        KeyCode::Char(' ' | '/') => m.overlay = Overlay::Switcher(Query::default()),
        KeyCode::Char('?') => m.overlay = Overlay::Help(HelpState::default()),
        KeyCode::Char('n') => open_form(m, FormKind::New),
        KeyCode::Char('c') if !m.main_focused() => open_form(m, FormKind::Checkout),
        KeyCode::Char('s') => {
            if let Some(entry) = selected {
                open_talk(m, entry.name(), None);
            }
        }
        KeyCode::Char('t') => m.info(SEND_MOVED),
        KeyCode::Char('v') => return Some(open_peek(m)),
        KeyCode::Char('x') => match selected {
            Some(entry) if matches!(m.reach(entry), Reach::Restarting) => {
                m.info(format!("{} is restarting", entry.name()));
            }
            _ if m.actions.is_empty() => m.info(format!(
                "no actions · add [[action]] to {}",
                crate::CONFIG_HINT
            )),
            Some(entry) => m.overlay = Overlay::Actions(entry.name().clone()),
            None => m.info("select an agent to run an action on"),
        },
        _ => return None,
    }
    Some(Vec::new())
}

/// Keys that act on the selected agent's session, or quit.
fn action_key(m: &mut Model, key: &KeyEvent, selected: Option<Entry>) -> Option<Vec<Effect>> {
    match key.code {
        KeyCode::Char('q') => {
            let unsent = m.unsent();
            if unsent.comments + unsent.messages + unsent.restarts == 0 {
                return Some(vec![Effect::Quit]);
            }
            m.overlay = Overlay::Confirm(Confirm::Quit(unsent));
        }
        KeyCode::Esc => return Some(enter_agent(m)),
        KeyCode::Char('i') => {
            if let Some(entry) = selected {
                let intent = enter_intent(m, entry.name());
                return Some(activate(m, entry.name(), intent));
            }
        }
        KeyCode::Enter => match (m.cursor().cloned(), selected) {
            (Some(Cursor::Repo(key)), _) => {
                m.view = View::Focus;
                m.select_repo(&key);
            }
            (_, Some(entry)) => {
                let intent = enter_intent(m, entry.name());
                return Some(activate(m, entry.name(), intent));
            }
            (None | Some(Cursor::Entry(_) | Cursor::Grouped(..)), None) => {}
        },
        KeyCode::Char('m') => {
            if let Some(entry) = selected {
                let name = entry.name().clone();
                return Some(vec![Effect::Job(match entry.pin() {
                    Some(_) => Job::Unpin(name),
                    None => Job::Pin(name),
                })]);
            }
        }
        KeyCode::Char('e') => {
            let path = m.selected().and_then(|(repo, e)| {
                e.and_then(|e| e.path())
                    .or_else(|| repo.key().root().map(agent_core::paths::RepoRoot::path))
                    .map(PathBuf::from)
            });
            if let Some(path) = path {
                return Some(vec![Effect::Edit(Open::Checkout(path))]);
            }
        }
        KeyCode::Char('d') => kill_selected(m, selected),
        KeyCode::Char('D') => return Some(request_removal(m)),
        KeyCode::Char('r') => return Some(restart::request_restart(m)),
        KeyCode::Char('R') => return Some(restart::request_restarts(m)),
        KeyCode::Char('a') => {
            let here = m.visible_session().cloned();
            let next = m
                .needs_you()
                .into_iter()
                .find(|name| Some(name) != here.as_ref());
            match next {
                Some(name) => return Some(activate(m, &name, Intent::Activate)),
                None => m.info("Nothing needs you"),
            }
        }
        _ => return None,
    }
    Some(Vec::new())
}

fn kill_selected(m: &mut Model, selected: Option<Entry>) {
    match selected.map(|e| (m.reach(&e), e)) {
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
        Some((Reach::Restarting, entry)) => m.info(format!("{} is restarting", entry.name())),
        Some((
            Reach::Other {
                enter: Enter::Resurrect | Enter::Restart(_),
                ..
            },
            entry,
        )) => m.info(format!("{} isn't running. D removes it", entry.name())),
        Some((Reach::Other { .. }, _)) | None => m.info("no session to kill"),
    }
}

/// Runs the action bound to `key` on `target`, refusing if a placeholder
/// it uses has nothing to fill it.
fn run_action(m: &mut Model, target: &SessionName, action: &Action) -> Vec<Effect> {
    let Some((repo, entry)) = m.snapshot().and_then(|s| s.entry(target)) else {
        m.error(format!("{target} is gone"));
        return Vec::new();
    };
    // A checkout git can't read is as good as gone, which only the worker
    // may check for.
    let readable = !matches!(m.facts(entry.name()), Some(Err(_)));
    let ctx = Context {
        name: entry.name(),
        path: entry.path().filter(|_| readable),
        repo: repo.key().root().map(agent_core::paths::RepoRoot::path),
    };
    match action.invoke(ctx) {
        Ok(Invocation::Run(launch)) => {
            m.elsewhere_due = Some(m.clock.mono + ELSEWHERE_RECHECK_AFTER);
            vec![Effect::Job(Job::Run {
                label: format!("{} on {target}", action.label),
                launch,
            })]
        }
        Ok(Invocation::Interactive(launch)) => vec![Effect::Interactive {
            label: action.label.to_string(),
            launch,
        }],
        Ok(Invocation::TerminalEvent { name, value }) => {
            m.info(format!("sent {} for {target}", action.label));
            m.elsewhere_due = Some(m.clock.mono + ELSEWHERE_RECHECK_AFTER);
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

/// Plans removing the selected agent. The confirmation opens when the plan
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
                planning: Planning::Removal(entry.name().clone()),
            };
            return vec![Effect::Job(Job::PlanRemoval { target, ticket })];
        }
        Reach::Other { remove: None, .. } => {
            m.info(format!("{} has nothing to remove", entry.name()));
        }
        Reach::Restarting => m.info(format!("{} is restarting", entry.name())),
    }
    Vec::new()
}

/// Opens the message box for `name`, showing what was left in it, or says
/// why it can't be messaged.
fn open_talk(m: &mut Model, name: &SessionName, quote: Option<crate::log::Quote>) {
    let Some(entry) = m.snapshot().and_then(|s| s.entry(name)).map(|(_, e)| e) else {
        return;
    };
    let to = match m.reachable(entry) {
        Ok(to) => to.name().clone(),
        Err(why) => return m.info(unreachable_says(name, why)),
    };
    match &mut m.track(&to).compose {
        Some(Compose::Sending { .. }) => {}
        Some(Compose::Editing { quote: kept, .. }) => {
            if quote.is_some() {
                *kept = quote;
            }
        }
        none @ None => {
            *none = Some(Compose::Editing {
                quote,
                text: Message::default(),
            });
        }
    }
    m.overlay = Overlay::Talk(to);
}

/// Opens the peek float on the selected live agent, which the pane moves to
/// first. A paused one is watched read-only for as long as the float is open.
fn open_peek(m: &mut Model) -> Vec<Effect> {
    let Some(entry) = m.selected_entry() else {
        m.info("select an agent to peek at");
        return Vec::new();
    };
    let name = entry.name().clone();
    match m.reach(entry) {
        reach if reach.is_attach() => {
            let mut effects = Vec::new();
            if m.pane.session() != Some(&name) {
                m.preview_at = None;
                effects.push(check_first(m, &name, AfterCheck::Preview));
            }
            m.peek = Some(Peek::Pane);
            effects.extend(start_watching(m));
            effects
        }
        Reach::Own => {
            m.info(format!("{name} is where this TUI runs"));
            Vec::new()
        }
        Reach::Restarting => {
            m.info(format!("{name} is restarting"));
            Vec::new()
        }
        Reach::Other { .. } => {
            m.info(format!("{name} isn't running · ⏎ starts it"));
            Vec::new()
        }
    }
}

/// Types the message now, or refuses and keeps the box open with its text.
fn talk(m: &mut Model, to: SessionName) -> Vec<Effect> {
    let draft = m.draft(&to).to_vec();
    let Some(Compose::Editing { quote, text }) = m.tracked(&to).and_then(|t| t.compose.as_ref())
    else {
        return Vec::new();
    };
    // A quote alone would send a message that says nothing, so it goes only with typed text.
    let typed = match quote {
        Some(quote) if !text.is_blank() => quote.before(text.as_str()),
        _ => text.as_str().to_string(),
    };
    let message = if draft.is_empty() {
        typed
    } else {
        crate::review::review_message(&typed, &draft)
    };
    let refused = match message.parse::<Prompt>() {
        Ok(prompt) => typeable(m, &to).map(|()| prompt),
        Err(InvalidPrompt::Empty) => Err("type a message first · Esc closes".to_string()),
        Err(InvalidPrompt::Control(c)) => Err(format!(
            "the message has a control character ({c:?}) · Esc closes"
        )),
    };
    let prompt = match refused {
        Ok(prompt) => prompt,
        Err(why) => {
            m.info(why);
            m.overlay = Overlay::Talk(to);
            return Vec::new();
        }
    };
    let ticket = m.ticket();
    let tracked = m.track(&to);
    tracked.draft.clear();
    if let Some(Compose::Editing { quote, text }) = tracked.compose.take() {
        tracked.compose = Some(Compose::Sending {
            quote,
            text,
            comments: draft,
            ticket,
        });
    }
    vec![Effect::Job(Job::Reply { to, prompt, ticket })]
}

/// What stops text being typed into `name` now, as a status line.
fn typeable(m: &Model, name: &SessionName) -> Result<(), String> {
    let entry = m
        .snapshot()
        .and_then(|s| s.entry(name))
        .map(|(_, e)| e)
        .ok_or_else(|| format!("{name} is gone"))?;
    let to = m.reachable(entry).map_err(|e| unreachable_says(name, e))?;
    m.hold(to.name())
        .map_or(Ok(()), |hold| Err(refusal(name, hold)))
}

fn unreachable_says(name: &SessionName, why: Unreachable) -> String {
    match why {
        Unreachable::NotRunning => format!("{name} isn't running · ⏎ starts it"),
        Unreachable::Own => format!("{name} is where this TUI runs"),
        Unreachable::Restarting => format!("{name} is restarting"),
        Unreachable::Unreported => format!(
            "{name} hasn't reported through hooks, so a menu open in it can't be known · ⏎ attaches"
        ),
    }
}

fn refusal(name: &SessionName, hold: Hold) -> String {
    format!("{} · {}", hold.says(name), hold.hint())
}

/// The worker typed the message sent under `ticket`, or didn't. What was
/// written stays with the agent to be edited, and nothing opens on its own.
fn on_sent(m: &mut Model, to: &SessionName, ticket: Ticket, result: Result<(), SendError>) {
    let Some(tracked) = m.agents.get_mut(to) else {
        return;
    };
    let answered =
        |c: &mut Compose| matches!(c, Compose::Sending { ticket: t, .. } if *t == ticket);
    let Some(Compose::Sending {
        quote,
        text,
        comments,
        ..
    }) = tracked.compose.take_if(answered)
    else {
        return;
    };
    match result {
        Ok(()) => {
            if matches!(&m.overlay, Overlay::Talk(open) if open == to) {
                m.overlay = Overlay::None;
            }
            m.info(format!("sent to {to}"));
        }
        Err(e) => {
            tracked.draft.splice(0..0, comments);
            tracked.compose = Some(Compose::Editing { quote, text });
            match e {
                SendError::Held(hold) => {
                    m.info(format!("{} · not sent, s to edit", hold.says(to)));
                }
                SendError::Failed(e) => {
                    m.error(format!("couldn't send to {to}: {e} · not sent, s to edit"));
                }
            }
        }
    }
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
    match typed_path(query, &m.start_dir, m.home.as_deref()) {
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
        Overlay::Talk(to) => on_talk_key(m, to, &key),
        Overlay::Comment(draft) => on_comment_key(m, draft, &key),
        Overlay::Switcher(query) => on_switcher_key(m, query, &key),
        Overlay::Picker(query) => on_picker_key(m, query, &key),
        Overlay::Confirm(confirm) => on_confirm_key(m, confirm, &key),
        Overlay::Planning { ticket, planning } => {
            if !matches!(key.code, KeyCode::Esc | KeyCode::Char('n' | 'q')) {
                m.overlay = Overlay::Planning { ticket, planning };
            }
            Vec::new()
        }
        Overlay::Actions(target) => on_actions_key(m, target, &key),
    }
}

/// The editing keys the send box and the comment box share. Up and down
/// move between wrapped rows, so they need the columns of the box, which
/// the one-row comment box has none of. Query and form fields stay
/// single-line strings.
fn edit(text: &mut Message, key: &KeyEvent, columns: Option<Columns>) {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Backspace
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            text.delete_word();
        }
        KeyCode::Backspace => text.backspace(),
        KeyCode::Delete => text.delete(),
        KeyCode::Left => text.left(),
        KeyCode::Right => text.right(),
        KeyCode::Home => text.home(),
        KeyCode::End => text.end(),
        KeyCode::Up => columns.into_iter().for_each(|c| text.up(c)),
        KeyCode::Down => columns.into_iter().for_each(|c| text.down(c)),
        KeyCode::Char('u') if control => text.clear(),
        KeyCode::Char('w') if control => text.delete_word(),
        KeyCode::Char('a') if control => text.home(),
        KeyCode::Char('e') if control => text.end(),
        KeyCode::Char(c) if !control => text.insert(c),
        _ => {}
    }
}

fn on_talk_key(m: &mut Model, to: SessionName, key: &KeyEvent) -> Vec<Effect> {
    let room = crate::view::talk_room(m.size);
    let Some(tracked) = m.tracked_mut(&to) else {
        return Vec::new();
    };
    match &mut tracked.compose {
        Some(Compose::Sending { .. }) => {
            if key.code != KeyCode::Esc {
                if ctrl(key, 'u') {
                    m.info("sending · can't cancel");
                }
                m.overlay = Overlay::Talk(to);
            }
            Vec::new()
        }
        Some(Compose::Editing { quote, text }) => {
            match key.code {
                KeyCode::Esc => {
                    if text.is_blank() {
                        tracked.compose = None;
                    }
                    return Vec::new();
                }
                KeyCode::Enter
                    if key
                        .modifiers
                        .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
                {
                    text.insert('\n');
                }
                KeyCode::Enter => return talk(m, to),
                _ if ctrl(key, 'j') => text.insert('\n'),
                _ if ctrl(key, 'e') => {
                    let text = text.as_str().to_string();
                    m.overlay = Overlay::Talk(to.clone());
                    return vec![Effect::EditMessage { to, text }];
                }
                KeyCode::Backspace if text.is_empty() => *quote = None,
                _ => edit(text, key, room.map(|r| r.columns)),
            }
            if let Some(room) = room {
                text.reveal(room);
            }
            m.overlay = Overlay::Talk(to);
            Vec::new()
        }
        None => Vec::new(),
    }
}

/// What editing the message for `to` in the editor came to: the text it
/// ended with goes back to the box, with the caret at its end and nothing
/// sent. A cancelled editor and a failed one leave the text as it was. The
/// quote and the drafted review comments are not part of it. When the box is
/// gone by the time the editor returns, the status says so and the text is
/// given back for the caller to keep.
#[must_use]
pub fn message_edited(
    m: &mut Model,
    to: &SessionName,
    result: Result<Option<String>, String>,
) -> Option<String> {
    let edited = match result {
        Ok(Some(edited)) => edited,
        Ok(None) => return None,
        Err(why) => {
            m.error(why);
            return None;
        }
    };
    let room = crate::view::talk_room(m.size);
    let Some(Compose::Editing { text, .. }) = m
        .tracked_mut(to)
        .and_then(|tracked| tracked.compose.as_mut())
    else {
        if edited.trim().is_empty() {
            return None;
        }
        m.error(format!("the edited message was dropped: {to} is gone"));
        return Some(edited);
    };
    *text = Message::from(edited.as_str());
    if let Some(room) = room {
        text.reveal(room);
    }
    None
}

fn on_comment_key(m: &mut Model, mut draft: CommentDraft, key: &KeyEvent) -> Vec<Effect> {
    match key.code {
        KeyCode::Esc => return Vec::new(),
        KeyCode::Enter => {
            add_comment(m, &draft.to, &draft.lines, draft.text.as_str(), draft.rev);
            return Vec::new();
        }
        _ => edit(&mut draft.text, key, None),
    }
    m.overlay = Overlay::Comment(draft);
    Vec::new()
}

fn on_switcher_key(m: &mut Model, mut query: Query, key: &KeyEvent) -> Vec<Effect> {
    let matches = switcher_matches(m, &query.text);
    match key.code {
        KeyCode::Esc | KeyCode::Tab => return Vec::new(),
        KeyCode::Enter => {
            if let Some(name) = query.pick(&matches).cloned() {
                return activate(m, &name, Intent::Activate);
            }
        }
        _ => edit_query(&mut query, key, matches.len(), m.keys.nav()),
    }
    m.overlay = Overlay::Switcher(query);
    Vec::new()
}

fn on_picker_key(m: &mut Model, mut query: Query, key: &KeyEvent) -> Vec<Effect> {
    let matches = picker_matches(m, &query.text);
    let rows = matches.rows();
    match key.code {
        KeyCode::Esc | KeyCode::Tab => return Vec::new(),
        KeyCode::Enter => match query.pick(rows).cloned() {
            Some(Pick::Listed(key)) => {
                m.select_repo(&key);
                return Vec::new();
            }
            Some(Pick::Found(root)) => {
                return vec![Effect::Job(Job::ListRepo(root.path().to_path_buf()))];
            }
            Some(Pick::Path(path)) => return vec![Effect::Job(Job::ListRepo(path))],
            None => {}
        },
        _ => edit_query(&mut query, key, rows.len(), m.keys.nav()),
    }
    m.overlay = Overlay::Picker(query);
    Vec::new()
}

fn on_actions_key(m: &mut Model, target: SessionName, key: &KeyEvent) -> Vec<Effect> {
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

/// Types `c` into the focused field of the new-agent form, dropping what
/// the field can't hold.
fn type_into(form: &mut NewAgent, c: char) {
    match form.focus {
        Field::Task if !c.is_control() || c == '\n' || c == '\t' => form.task.push(c),
        Field::Name if c.is_whitespace() => form.name.push('-'),
        Field::Name if !c.is_control() => form.name.push(c),
        Field::Repo if !c.is_control() => form.repo.push(c),
        Field::Task | Field::Name | Field::Repo => {}
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
    let now = m.clock.mono;
    let limit = m.name_limit;
    if let Overlay::New(form) = &mut m.overlay {
        if let Phase::Submitting { .. } = form.phase {
            return;
        }
        let before = form.task.clone();
        edit(form);
        form.error = None;
        if form.task != before {
            form.prompt_changed(now, limit);
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
            m.abandon(ticket);
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
            form.error = blocked.reason().map(Cause::message);
            return Vec::new();
        }
    };
    form.phase = Phase::Submitting {
        ticket,
        launch: launch.clone(),
    };
    let session = launch.session();
    m.set_pane(Pane::Connecting {
        name: session.clone(),
        ticket,
    });
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
        m.set_pane(Pane::Empty);
    }
}

fn remove(m: &mut Model, approved: Approved) -> Vec<Effect> {
    drop_connection_to(m, &approved.name());
    vec![Effect::Job(Job::Remove(approved))]
}

fn on_confirm_key(m: &mut Model, confirm: Confirm, key: &KeyEvent) -> Vec<Effect> {
    match (confirm, key.code) {
        (Confirm::Kill(running), KeyCode::Char('y') | KeyCode::Enter) => {
            drop_connection_to(m, running.name());
            vec![Effect::Job(Job::Kill(running))]
        }
        (Confirm::Quit(_), KeyCode::Char('y') | KeyCode::Enter) => vec![Effect::Quit],
        (_, KeyCode::Esc | KeyCode::Char('n' | 'q')) => Vec::new(),
        (confirm @ (Confirm::Restart(_) | Confirm::Restarts(_)), code) => {
            restart::on_restart_key(m, confirm, code)
        }
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

/// The row of `rows` drawn on screen row `y`.
fn row_at<T: Clone>(rows: &[(u16, T)], y: u16) -> Option<T> {
    rows.iter()
        .find(|(at, _)| *at == y)
        .map(|(_, row)| row.clone())
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
            Panel::Agents | Panel::Main(..) => false,
        };
        if focused && !typing {
            m.move_in_panel(step);
        }
        return Some(Vec::new());
    }
    m.pane.set_nav();
    if over_agent {
        let row = row_at(&m.areas.agent_rows, mouse.row).map(|hit| hit.row);
        m.panel.leave_document();
        if !matches!(m.panel, Panel::Agent { .. }) {
            m.panel = Panel::activity();
        }
        if let Some(row) = row {
            m.point_at(row);
        }
        return Some(Vec::new());
    }
    {
        let row = row_at(&m.areas.change_rows, mouse.row);
        let file = match m.panel.list() {
            Listed::Changes { file } => file,
            Listed::Agents | Listed::Agent { .. } => 0,
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
            m.focus_document();
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
        return forward
            .and_then(|c| {
                let bytes = encode_mouse(mouse.kind, cell, c.term.modes())?;
                Some(Effect::Pty {
                    to: c.name.clone(),
                    bytes,
                })
            })
            .into_iter()
            .collect();
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
        m.pane.set_nav();
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
    use crate::track::{LOG_REFRESH, LOG_SLOW_REFRESH, PROBE_EVERY};
    use std::time::{Instant, SystemTime};

    use crate::model::{Found, Unsent};

    use agent_core::activity::Hold;
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
        assemble(
            &obs,
            &Layout::new("/w".into()).expect("absolute"),
            &pins,
            agent_core::testing::NAME_LIMIT,
        )
        .0
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
        assemble(
            &obs,
            &Layout::new("/w".into()).expect("absolute"),
            &[],
            agent_core::testing::NAME_LIMIT,
        )
        .0
    }

    /// [0] as drawn with `rows` showing.
    fn paged(m: &mut Model, rows: usize) {
        m.areas.main = Some(crate::model::MainLayout {
            height: rows,
            ..Default::default()
        });
    }

    #[test]
    fn ctrl_d_and_ctrl_u_move_half_a_page_in_the_focused_panel() {
        let mut m = model(&[]);
        m.areas.page = 6;
        let selected_before = selected(&m);
        update(&mut m, key_with(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_ne!(selected(&m), selected_before, "moves the selection in [1]");
        assert_eq!(
            selected(&m).as_deref(),
            Some("idle"),
            "stops at the end rather than wrapping"
        );
        m.panel = Panel::activity();
        paged(&mut m, 6);
        update(&mut m, key_with(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(m.docs.scroll, 3, "from [2] it's half a page of [0]");
        update(&mut m, key_with(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(m.docs.scroll, 0);
        paged(&mut m, 1);
        update(&mut m, key_with(KeyCode::Char('d'), KeyModifiers::CONTROL));
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
                result: Err(Cause::message("no".into())),
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
        let mut m = crate::fixtures::model("/w/api/fix/src");
        m.view = crate::model::View::Focus;
        m.visibility = Visibility::All;
        update(
            &mut m,
            Msg::Snapshot(Ok((snapshot(sessions, pins), Vec::new()))),
        );
        m.changes_view = ChangesView::Flat;
        m
    }

    #[test]
    fn a_worktree_over_the_hosts_limit_says_why_it_cant_start_and_can_still_be_removed() {
        let long = "an-over-the-limit-worktree";
        let obs = Observation {
            sessions: Vec::new(),
            repos: vec![(
                RepoRoot::new("/src/api".into()).expect("absolute"),
                RepoWorktrees {
                    main: wt("/src/api"),
                    linked: vec![wt(&format!("/w/api/{long}"))],
                },
            )],
            warnings: Vec::new(),
        };
        let snapshot = assemble(
            &obs,
            &Layout::new("/w".into()).expect("absolute"),
            &[],
            agent_core::testing::NAME_LIMIT,
        )
        .0;
        let mut m = crate::fixtures::model("/elsewhere");
        m.visibility = Visibility::All;
        update(&mut m, Msg::Snapshot(Ok((snapshot, Vec::new()))));
        m.set_cursor(Cursor::Entry(name(long)));

        let effects = update(&mut m, key(KeyCode::Enter));
        assert!(
            effects
                .iter()
                .all(|e| !matches!(e, Effect::Job(Job::Start { .. }))),
            "nothing starts"
        );
        let status = m.status.as_ref().expect("a status says why");
        assert_eq!(status.kind, crate::model::StatusKind::Error);
        assert!(
            status.text.contains("session host's limit"),
            "{}",
            status.text
        );

        let hints = crate::keymap::footer(&crate::keymap::Situation::of(&m), "⌃\\");
        assert!(hints.iter().all(|h| h.keys != "⏎"), "no ⏎ hint");
        assert!(hints.iter().any(|h| h.keys == "D"), "D still offered");
    }

    fn key(code: KeyCode) -> Msg {
        key_with(code, KeyModifiers::NONE)
    }

    fn key_with(code: KeyCode, modifiers: KeyModifiers) -> Msg {
        Msg::Key(KeyEvent::new(code, modifiers))
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
            matches!(rows.last(), Some(Row::Entry { name, place: crate::model::Placement::Solo }) if name.as_str() == "web")
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
        let snap = assemble(
            &obs,
            &Layout::new("/w".into()).expect("absolute"),
            &[],
            agent_core::testing::NAME_LIMIT,
        )
        .0;
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

    fn with_activity(m: &mut Model) {
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
            .map(|i| {
                agent_core::ports::Change::untracked(
                    format!("src/f{i}.rs"),
                    agent_core::ports::Added::Unread,
                )
            })
            .collect();
        m.set_test_facts(
            &name("fix"),
            Ok(agent_core::ports::WorktreeFacts {
                pending: changed.clone(),
                changed,
                ..crate::fixtures::unchanged()
            }),
        );
        m.settle();
    }

    fn with_paths(m: &mut Model, paths: &[&str]) {
        let changed: Vec<agent_core::ports::Change> = paths
            .iter()
            .map(|p| {
                agent_core::ports::Change::tracked(
                    *p,
                    agent_core::ports::Lines::Counted {
                        added: 2,
                        removed: 1,
                    },
                    agent_core::ports::TrackedState::Committed,
                )
            })
            .collect();
        m.set_test_facts(
            &name("fix"),
            Ok(agent_core::ports::WorktreeFacts {
                pending: changed.clone(),
                changed,
                ..crate::fixtures::unchanged()
            }),
        );
        m.settle();
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
        assert_eq!(crate::fixtures::model("/").changes_view, ChangesView::Tree);
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
        use agent_core::ports::{Change, Edit, Lines, TrackedState};
        let change = |path: &str, added, state| {
            Change::tracked(path, Lines::Counted { added, removed: 0 }, state)
        };
        let edited = TrackedState::Unstaged(Edit::Modified);
        m.set_test_facts(
            &name("fix"),
            Ok(agent_core::ports::WorktreeFacts {
                ahead: 3,
                compared: agent_core::ports::Compared::Branch {
                    branch: "fix".parse().expect("parses"),
                    base: "origin/main".parse().expect("parses"),
                },
                changed: vec![
                    change("a/old.rs", 40, TrackedState::Committed),
                    change("a/hot.rs", 9, edited),
                ],
                pending: vec![change("a/hot.rs", 2, edited)],
                ..crate::fixtures::unchanged()
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
            m.change_at(1).map(agent_core::ports::Change::state),
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
        let mut facts = match m.facts(&name("fix")) {
            Some(Ok(facts)) => facts.clone(),
            _ => panic!("facts"),
        };
        facts.compared = Compared::Trunk {
            branch: "main".parse().expect("parses"),
            base: "origin/main".parse().expect("parses"),
        };
        m.set_test_facts(&name("fix"), Ok(facts));
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
            matches!(m.panel, Panel::Main(..)),
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
        assert!(m.tracked(&other).is_none_or(|t| t.collapsed.is_empty()));
    }

    fn closed(m: &Model) -> Vec<String> {
        m.tracked(&name("fix"))
            .into_iter()
            .flat_map(|t| &t.collapsed)
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
                .map(|p| agent_core::ports::Change::untracked(*p, agent_core::ports::Added::Unread))
                .collect();
            update(
                m,
                Msg::Inspected {
                    name: name("fix"),
                    read: Ok(Inspected::of(agent_core::ports::WorktreeFacts {
                        pending: changed.clone(),
                        changed,
                        ..crate::fixtures::unchanged()
                    })),
                },
            );
        };
        m.track(&name("fix")).inspection = Some(Inspection {
            events: None,
            at: Instant::now(),
        });
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
    fn the_tree_is_built_once_per_epoch_view_and_fold_state() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &PATHS);
        let first = m.change_list();
        assert!(std::sync::Arc::ptr_eq(&first, &m.change_list()));
        m.panel = Panel::Changes { file: 0 };
        m.toggle_folder();
        m.settle();
        let folded = m.change_list();
        assert!(
            !std::sync::Arc::ptr_eq(&first, &folded),
            "fold state changed"
        );
        assert!(std::sync::Arc::ptr_eq(&folded, &m.change_list()));
        m.toggle_changes_view();
        m.settle();
        assert!(!std::sync::Arc::ptr_eq(&folded, &m.change_list()), "view");
        m.toggle_changes_view();
        m.settle();
        let before = m.change_list();
        with_paths(&mut m, &PATHS);
        assert!(!std::sync::Arc::ptr_eq(&before, &m.change_list()), "facts");
    }

    #[test]
    fn changing_the_folds_or_facts_builds_the_tree_once_before_anything_reads_it() {
        let mut m = model(&[]);
        m.changes_view = ChangesView::Tree;
        with_paths(&mut m, &PATHS);
        m.panel = Panel::Changes { file: 0 };
        let first = m.change_list();
        m.toggle_folder();
        let folded = m.change_list();
        assert!(!std::sync::Arc::ptr_eq(&first, &folded));
        assert!(std::sync::Arc::ptr_eq(&folded, &m.change_list()));
        m.fold_all(false);
        let opened = m.change_list();
        assert!(std::sync::Arc::ptr_eq(&opened, &m.change_list()));
        with_paths(&mut m, &PATHS);
        let reread = m.change_list();
        assert!(std::sync::Arc::ptr_eq(&reread, &m.change_list()));
        m.settle();
        assert!(std::sync::Arc::ptr_eq(&reread, &m.change_list()));
    }

    #[test]
    fn resizing_gives_the_terminal_pane_its_new_room_before_the_next_draw() {
        let mut m = model(&[]);
        let size = ratatui::layout::Rect::new(0, 0, 150, 40);
        resized(&mut m, size);
        assert_eq!(
            m.areas.terminal,
            crate::view::geometry(size, m.screen).terminal
        );
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
        let effects = update(&mut m, key(KeyCode::Char('e')));
        assert!(
            matches!(effects.as_slice(), [Effect::Edit(Open::Checkout(dir))] if dir.ends_with("a")),
            "the runner reports a folder missing from disk"
        );
        m.panel = Panel::Changes { file: 2 };
        assert!(m.file_under_cursor().is_some());
    }

    #[test]
    fn number_keys_focus_panels_and_esc_returns_to_the_list() {
        let mut m = model(&[]);
        with_changes(&mut m, 3);
        with_activity(&mut m);
        update(&mut m, key(KeyCode::Char('2')));
        assert_eq!(m.panel, Panel::activity());
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
        with_activity(&mut m);
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
        assert_eq!(m.panel, Panel::activity());
        assert_eq!(m.row_at(), 0, "[2] has only its activity row");
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
        with_activity(&mut m);
        update(&mut m, key(KeyCode::Char('3')));
        update(&mut m, key(KeyCode::Char('}')));
        assert_eq!(m.panel, Panel::Agents, "a new selection starts in [1]");
        update(&mut m, key(KeyCode::Char('{')));
        update(&mut m, key(KeyCode::Char('2')));
        m.revalidate();
        assert_eq!(m.panel, Panel::activity(), "the same agent keeps [2]");
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
        with_activity(&mut m);
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::activity());
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Agents);
        update(&mut m, key(KeyCode::BackTab));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        update(&mut m, key(KeyCode::BackTab));
        assert_eq!(m.panel, Panel::activity());
    }

    #[test]
    fn h_and_l_move_between_the_left_panels_like_shift_tab_and_tab() {
        let mut m = model(&[]);
        with_changes(&mut m, 3);
        with_activity(&mut m);
        update(&mut m, key(KeyCode::Char('l')));
        assert_eq!(m.panel, Panel::activity());
        update(&mut m, key(KeyCode::Char('l')));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        update(&mut m, key(KeyCode::Char('l')));
        assert_eq!(m.panel, Panel::Agents);
        update(&mut m, key(KeyCode::Char('h')));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        update(&mut m, key(KeyCode::Char('h')));
        assert_eq!(m.panel, Panel::activity());
        m.panel = Panel::Main(
            crate::model::Origin::Activity {
                offset: 0,
                row: crate::model::ActivityRow::Main,
            },
            None,
        );
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
        assert_eq!(m.panel, Panel::Changes { file: 0 }, "no activity recorded");
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Agents);
        with_changes(&mut m, 0);
        with_activity(&mut m);
        update(&mut m, key(KeyCode::BackTab));
        assert_eq!(m.panel, Panel::activity(), "no files");
        m.work_room = WorkRoom::TooShort;
        with_changes(&mut m, 3);
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Agents, "no room for [3]");
    }

    #[test]
    fn tab_from_a_document_goes_to_the_panel_after_its_origin() {
        let mut m = model(&[]);
        with_changes(&mut m, 3);
        with_activity(&mut m);
        m.panel = Panel::Main(
            crate::model::Origin::Activity {
                offset: 0,
                row: crate::model::ActivityRow::Main,
            },
            None,
        );
        update(&mut m, key(KeyCode::Tab));
        assert_eq!(m.panel, Panel::Changes { file: 0 });
        m.panel = Panel::Main(crate::model::Origin::Changes { file: 0 }, None);
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
        with_activity(&mut m);
        update(&mut m, key(KeyCode::Char('[')));
        assert_eq!(m.view, View::Overview);
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.view, View::Focus);
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(m.view, View::Focus, "[2] has no tabs");
        assert_eq!(m.panel, Panel::activity());
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
                result: Err(Cause::message("boom".into())),
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
        let snapshot = assemble(
            &obs,
            &Layout::new("/w".into()).expect("absolute"),
            &[],
            agent_core::testing::NAME_LIMIT,
        )
        .0;
        let mut m = crate::fixtures::model("/w/api/fix/src");
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
                result: Err(Cause::message(
                    "branch fix-ci not found locally or on origin".into(),
                )),
            },
        );
        assert!(matches!(form(&m).phase, Phase::Editing));
        assert_eq!(
            form(&m).error.as_ref().map(ToString::to_string).as_deref(),
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
            NameCheck::TooLong { over, limit: 20 } if over.get() == 7
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
        update(&mut m, key_with(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(
            derived(&m).name,
            "add-dark-mode",
            "⌃R goes back to the suggestion"
        );
    }

    /// The moment `after` past the model's clock.
    fn later(m: &Model, after: Duration) -> Moment {
        Moment {
            wall: m.clock.wall + after,
            mono: m.clock.mono + after,
        }
    }

    mod interrupts {
        use super::*;
        use agent_core::ports::{Probe, TranscriptStamp};

        const WORKER: &str = "auth-refresh-2";

        fn transcript() -> TranscriptRef {
            TranscriptRef::new("/t/s.jsonl")
        }

        fn watching(states: &[(&str, State)]) -> Model {
            let mut m = crate::fixtures::fleet(10);
            let mut activities = HashMap::new();
            for (agent, state) in states {
                let mut activity = reported(state.clone(), 5);
                activity.transcript = Some(transcript());
                activity.interrupts = InterruptSignal::Transcript;
                activities.insert(name(agent), activity);
            }
            m.activities = Some(activities);
            m
        }

        fn probes(effects: &[Effect]) -> Vec<(String, Option<TranscriptStamp>)> {
            effects
                .iter()
                .filter_map(|e| match e {
                    Effect::ProbeTranscript { name, seen, .. } => {
                        Some((name.to_string(), seen.clone()))
                    }
                    _ => None,
                })
                .collect()
        }

        fn working() -> State {
            State::Working { tools: Vec::new() }
        }

        fn stamp(len: u64) -> TranscriptStamp {
            TranscriptStamp::new(len, None)
        }

        fn standing(m: &Model) -> Standing {
            m.activity(&name(WORKER))
                .and_then(Activity::standing)
                .expect("going or stopped")
        }

        fn answer(m: &mut Model, standing: Standing, result: Result<Probe, Cause>) {
            update(
                m,
                Msg::Probed {
                    name: name(WORKER),
                    standing,
                    result,
                },
            );
        }

        fn newer(m: &mut Model) {
            let mut newer = reported(working(), 0);
            newer.transcript = Some(transcript());
            newer.interrupts = InterruptSignal::Transcript;
            newer.events = 2;
            update(m, Msg::Activities(HashMap::from([(name(WORKER), newer)])));
        }

        #[test]
        fn every_going_or_interrupted_agent_is_read_not_only_the_one_shown() {
            let mut m = watching(&[
                (WORKER, working()),
                ("obs-layout-4", State::Delegating),
                ("docs-sweep-6", State::Interrupted),
                ("payroll-fix-3", State::Done { summary: None }),
                ("bell-fix-5", State::Idle),
            ]);
            m.set_cursor(Cursor::Entry(name("payroll-fix-3")));
            let mut checked: Vec<String> = probes(&tick(&mut m)).into_iter().map(|p| p.0).collect();
            checked.sort();
            assert_eq!(checked, ["auth-refresh-2", "docs-sweep-6", "obs-layout-4"]);
        }

        #[test]
        fn a_harness_that_sends_an_event_for_an_interrupt_is_not_read() {
            let mut m = watching(&[(WORKER, working())]);
            if let Some(activity) = m.activities.as_mut().and_then(|a| a.get_mut(&name(WORKER))) {
                activity.interrupts = InterruptSignal::Reported;
            }
            assert_eq!(probes(&tick(&mut m)), []);
        }

        #[test]
        fn an_agent_whose_hooks_named_no_transcript_has_none_to_read() {
            let mut m = watching(&[(WORKER, working())]);
            if let Some(activity) = m.activities.as_mut().and_then(|a| a.get_mut(&name(WORKER))) {
                activity.transcript = None;
            }
            assert_eq!(probes(&tick(&mut m)), []);
        }

        #[test]
        fn an_agent_is_read_again_only_once_the_interval_has_passed() {
            let mut m = watching(&[(WORKER, working())]);
            assert_eq!(probes(&tick(&mut m)).len(), 1);
            let soon = later(&m, PROBE_EVERY / 2);
            assert_eq!(probes(&tick_at(&mut m, soon)), [], "too soon");
            let due = later(&m, PROBE_EVERY);
            assert_eq!(probes(&tick_at(&mut m, due)).len(), 1);
        }

        #[test]
        fn the_file_as_last_found_travels_with_the_next_read_while_nothing_new_was_reported() {
            let mut m = watching(&[(WORKER, working())]);
            let proof = standing(&m);
            assert_eq!(probes(&tick(&mut m)), [(WORKER.to_string(), None)]);
            answer(&mut m, proof, Ok(Probe::Going(stamp(10))));
            let due = later(&m, PROBE_EVERY);
            assert_eq!(
                probes(&tick_at(&mut m, due)),
                [(WORKER.to_string(), Some(stamp(10)))]
            );
        }

        #[test]
        fn a_report_since_the_last_read_makes_the_next_one_read_the_file_again() {
            let mut m = watching(&[(WORKER, working())]);
            let proof = standing(&m);
            tick(&mut m);
            answer(&mut m, proof, Ok(Probe::Going(stamp(10))));
            newer(&mut m);
            let due = later(&m, PROBE_EVERY);
            assert_eq!(probes(&tick_at(&mut m, due)), [(WORKER.to_string(), None)]);
        }

        #[test]
        fn an_answer_for_an_older_proof_does_not_overwrite_what_a_newer_read_found() {
            let mut m = watching(&[(WORKER, working())]);
            let old = standing(&m);
            tick(&mut m);
            newer(&mut m);
            let due = later(&m, PROBE_EVERY);
            tick_at(&mut m, due);
            answer(&mut m, old, Ok(Probe::Going(stamp(10))));
            let due = later(&m, PROBE_EVERY * 2);
            assert_eq!(probes(&tick_at(&mut m, due)), [(WORKER.to_string(), None)]);
        }

        #[test]
        fn a_failed_read_is_told_once_and_the_next_one_reads_the_file_again() {
            let mut m = watching(&[(WORKER, working())]);
            let proof = standing(&m);
            tick(&mut m);
            answer(&mut m, proof, Ok(Probe::Going(stamp(10))));
            let due = later(&m, PROBE_EVERY);
            tick_at(&mut m, due);
            answer(&mut m, proof, Err(Cause::message("io".into())));
            assert!(status(&m).contains("couldn't read auth-refresh-2's transcript"));
            m.status = None;
            let due = later(&m, PROBE_EVERY * 2);
            assert_eq!(probes(&tick_at(&mut m, due)), [(WORKER.to_string(), None)]);
            answer(&mut m, proof, Err(Cause::message("io".into())));
            assert_eq!(status(&m), "", "once is enough");
        }

        #[test]
        fn an_agent_with_no_transcript_is_not_read_again_for_that_proof() {
            let mut m = watching(&[(WORKER, working())]);
            let proof = standing(&m);
            tick(&mut m);
            answer(&mut m, proof, Ok(Probe::NoTranscript));
            let due = later(&m, PROBE_EVERY * 5);
            assert_eq!(probes(&tick_at(&mut m, due)), []);
            newer(&mut m);
            let due = later(&m, PROBE_EVERY * 6);
            assert_eq!(
                probes(&tick_at(&mut m, due)).len(),
                1,
                "a new proof asks again"
            );
        }

        #[test]
        fn an_interrupted_agent_is_shown_as_stopped_and_its_transcript_is_still_watched() {
            let mut m = watching(&[(WORKER, State::Interrupted)]);
            let (_, entry) = m
                .snapshot()
                .and_then(|s| s.entry(&name(WORKER)))
                .expect("listed");
            assert!(matches!(
                m.status(entry),
                crate::model::AgentStatus::Interrupted { .. }
            ));
            assert_eq!(probes(&tick(&mut m)).len(), 1, "it may carry on");
        }
    }

    #[test]
    fn a_pause_in_typing_is_measured_on_the_clock_the_tick_is_given() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "why is the export slow");
        let asks = |effects: Vec<Effect>| {
            effects
                .iter()
                .filter(|e| matches!(e, Effect::SuggestName { .. }))
                .count()
        };
        let almost = later(
            &m,
            crate::model::NAMING_DEBOUNCE.saturating_sub(Duration::from_millis(1)),
        );
        assert_eq!(asks(tick_at(&mut m, almost)), 0, "typing hasn't paused");
        let paused = later(&m, Duration::from_millis(1));
        assert_eq!(asks(tick_at(&mut m, paused)), 1);
        let after = later(&m, crate::model::NAMING_DEBOUNCE);
        assert_eq!(asks(tick_at(&mut m, after)), 0, "asked once");
    }

    #[test]
    fn typing_again_before_the_pause_ends_asks_for_the_newer_prompt_only() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "why is the export slow");
        let half = later(&m, crate::model::NAMING_DEBOUNCE / 2);
        m.clock = half;
        type_text(&mut m, "?");
        let first_due = later(&m, crate::model::NAMING_DEBOUNCE / 2);
        let effects = tick_at(&mut m, first_due);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::SuggestName { .. })),
            "the earlier pause was pushed back"
        );
        let due = later(&m, crate::model::NAMING_DEBOUNCE);
        let asked: Vec<_> = tick_at(&mut m, due)
            .into_iter()
            .filter_map(|e| match e {
                Effect::SuggestName { edit, .. } => Some(edit),
                _ => None,
            })
            .collect();
        assert_eq!(asked, [form(&m).naming.edit]);
    }

    #[test]
    fn an_info_message_expires_on_the_clock_the_tick_is_given() {
        let mut m = model(&[]);
        m.info("took a moment");
        let almost = later(&m, INFO_TTL);
        tick_at(&mut m, almost);
        assert!(m.status.is_some(), "kept for exactly its time to live");
        let over = later(&m, Duration::from_millis(1));
        tick_at(&mut m, over);
        assert!(m.status.is_none());
        m.error("kept");
        let long = later(&m, INFO_TTL * 10);
        tick_at(&mut m, long);
        assert!(m.status.is_some(), "errors stay until a key");
    }

    #[test]
    fn the_preview_waits_out_its_debounce_on_the_models_clock() {
        let mut m = model(&[]);
        m.schedule_preview();
        let almost = later(
            &m,
            crate::model::PREVIEW_DEBOUNCE.saturating_sub(Duration::from_millis(1)),
        );
        tick_at(&mut m, almost);
        assert!(m.preview_at.is_some(), "still waiting");
        let due = later(&m, Duration::from_millis(1));
        tick_at(&mut m, due);
        assert!(m.preview_at.is_none(), "taken once it is due");
    }

    fn type_repo(m: &mut Model, text: &str) {
        update(m, key(KeyCode::BackTab));
        type_text(m, text);
    }

    #[test]
    fn a_typed_repo_path_is_looked_at_by_the_worker_once_and_never_by_the_form() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "add dark mode");
        type_repo(&mut m, "/no/such/place");
        let dir = std::path::PathBuf::from("/no/such/place");
        assert_eq!(derived(&m).repo_input, RepoInput::Checking(dir.clone()));
        assert!(
            matches!(
                derived(&m).submission,
                Submission::Blocked(Blocked::Checking)
            ),
            "nothing is submitted on a guess"
        );
        let checks = |effects: &[Effect]| {
            effects
                .iter()
                .filter(|e| matches!(e, Effect::Job(Job::CheckDir(_))))
                .count()
        };
        assert_eq!(checks(&tick(&mut m)), 1);
        assert_eq!(checks(&tick(&mut m)), 0, "asked once per path");

        update(
            &mut m,
            Msg::DirChecked {
                path: "/another".into(),
                found: Found::Dir,
            },
        );
        assert_eq!(
            derived(&m).repo_input,
            RepoInput::Checking(dir.clone()),
            "an answer about another path counts for nothing"
        );
        update(
            &mut m,
            Msg::DirChecked {
                path: dir.clone(),
                found: Found::Nothing,
            },
        );
        assert_eq!(derived(&m).repo_input, RepoInput::Missing(dir.clone()));
        assert_eq!(checks(&tick(&mut m)), 0, "answered");

        type_text(&mut m, "x");
        assert!(matches!(derived(&m).repo_input, RepoInput::Checking(_)));
        assert_eq!(checks(&tick(&mut m)), 1, "a new path is asked about");
    }

    #[test]
    fn a_directory_the_worker_found_is_a_path_to_launch_in() {
        let mut m = model(&[]);
        open_form(&mut m);
        type_text(&mut m, "add dark mode");
        type_repo(&mut m, "/src/new");
        tick(&mut m);
        update(
            &mut m,
            Msg::DirChecked {
                path: "/src/new".into(),
                found: Found::Dir,
            },
        );
        assert!(matches!(
            derived(&m).submission,
            Submission::Ready(Launchable::Create {
                repo: crate::model::RepoChoice::Path(_),
                ..
            })
        ));
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
        let Some(edit) = effects.iter().find_map(|e| match e {
            Effect::SuggestName { edit, .. } => Some(*edit),
            _ => None,
        }) else {
            panic!("no ask")
        };
        update(
            &mut m,
            Msg::NameSuggested {
                edit: PromptEdit::default(),
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
                edit,
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
                result: Err(Cause::message("boom".into())),
            },
        );
        assert_eq!(
            form(&m).error.as_ref().map(ToString::to_string).as_deref(),
            Some("boom")
        );
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
        update(&mut m, key_with(KeyCode::Char('u'), KeyModifiers::CONTROL));
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
                result: Err(Cause::message("boom".into())),
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

    fn asked_needs(effects: &[Effect]) -> Vec<Need> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::CheckElsewhere(look) => Some(look.need),
                _ => None,
            })
            .collect()
    }

    fn after_action(key_char: char) -> Model {
        let mut m = model(&[]);
        with_actions(&mut m);
        update(&mut m, key(KeyCode::Char('x')));
        update(&mut m, key(KeyCode::Char(key_char)));
        m.preview_at = None;
        m
    }

    #[test]
    fn an_action_that_leaves_the_tui_running_is_followed_shortly_by_one_look_for_other_windows() {
        for action in ['s', 't'] {
            let mut m = after_action(action);
            let early = later(
                &m,
                ELSEWHERE_RECHECK_AFTER.saturating_sub(Duration::from_millis(1)),
            );
            assert!(asked_needs(&tick_at(&mut m, early)).is_empty(), "not yet");
            let due = later(&m, ELSEWHERE_RECHECK_AFTER);
            assert_eq!(asked_needs(&tick_at(&mut m, due)), [Need::Refresh]);
            let after = later(&m, ELSEWHERE_RECHECK_AFTER * 2);
            assert!(
                asked_needs(&tick_at(&mut m, after)).is_empty(),
                "asked once"
            );
        }
    }

    #[test]
    fn the_look_after_an_action_waits_for_a_pane_that_is_checking_and_asks_once_it_settles() {
        let mut m = after_action('s');
        let due = later(&m, ELSEWHERE_RECHECK_AFTER);
        m.pane = Pane::Checking {
            name: name("fix"),
            then: AfterCheck::Preview,
            until: due.mono + Duration::from_secs(60),
        };
        assert!(asked_needs(&tick_at(&mut m, due)).is_empty(), "deferred");
        m.pane = Pane::Empty;
        let next = later(&m, Duration::from_millis(100));
        assert_eq!(asked_needs(&tick_at(&mut m, next)), [Need::Refresh]);
    }

    #[test]
    fn actions_needing_a_checkout_git_cannot_read_are_refused() {
        let mut m = model(&[]);
        with_actions(&mut m);
        m.track(&name("fix")).inspection = Some(Inspection {
            events: None,
            at: m.clock.mono,
        });
        update(
            &mut m,
            Msg::Inspected {
                name: name("fix"),
                read: (Err(Cause::message("not a git repository".into()))).map(Inspected::of),
            },
        );
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
        let mut m = crate::fixtures::model("/elsewhere");
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
        assert_eq!(checked_sessions(&m, &effects), ["web"]);
        let effects = crate::fixtures::sighted(&mut m, &[]);
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
        let mut m = crate::fixtures::model("/w/api/fix/src");
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
        let mut m = crate::fixtures::model("/elsewhere");
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
            for last in m.agents.values_mut().filter_map(|t| t.inspection.as_mut()) {
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
        let m = crate::fixtures::model("/");
        assert_eq!(m.view, View::Overview);
    }

    #[test]
    fn the_pinned_tab_lists_pins_then_the_other_agents_that_need_you() {
        let mut m = model(&["api"]);
        update(&mut m, key(KeyCode::Char(']')));
        let pin = Row::Entry {
            name: name("api"),
            place: crate::model::Placement::Pinned,
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
                place: crate::model::Placement::Pinned,
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
        assemble(
            &obs,
            &Layout::new("/w".into()).expect("absolute"),
            &pins,
            agent_core::testing::NAME_LIMIT,
        )
        .0
    }

    #[test]
    fn a_displaced_agent_that_has_gone_does_not_drag_the_selection() {
        let mut m = crate::fixtures::model("/w/api/fix/src");
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
        assert_eq!(checked_sessions(&m, &effects), ["web"]);
        let effects = crate::fixtures::sighted(&mut m, &[]);
        assert!(
            matches!(effects.as_slice(), [Effect::Attach { name, intent: Intent::Activate, .. }] if name.as_str() == "web")
        );
    }

    #[test]
    fn a_long_activity_finishing_elsewhere_rings_once() {
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
    fn focusing_the_activity_puts_a_cursor_on_its_rows_and_reads_the_subagent_it_is_on() {
        let mut m = delegating();
        assert_eq!(m.subagent_rows().len(), 3);
        assert!(
            reads(tick(&mut m)).is_empty(),
            "nothing is read while [2] is unfocused"
        );

        update(&mut m, key(KeyCode::Char('2')));
        assert_eq!(m.row_at(), 0, "the activity row first");
        assert!(
            reads(tick(&mut m)).is_empty(),
            "the activity needs no transcript"
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
            Panel::activity(),
            "j moves the cursor, not a scroll"
        );
    }

    #[test]
    fn g_and_k_walk_the_commands_below_the_subagents_and_enter_opens_one_and_esc_returns_to_its_row()
     {
        let mut m = delegating();
        crate::fixtures::run_commands(
            &mut m,
            "obs-layout-4",
            &[
                crate::fixtures::listed("c1", "cargo test"),
                crate::fixtures::listed("c2", "bin/rails s"),
            ],
            Duration::from_mins(4),
        );
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('G')));
        assert_eq!(m.row_at(), 5, "the activity, three subagents, two commands");
        assert!(
            reads(tick(&mut m)).is_empty(),
            "a command has no transcript to read"
        );
        update(&mut m, key(KeyCode::Char('k')));
        assert!(matches!(
            m.hover(),
            Some(crate::document::Hover::Command(_, ref id)) if id.as_str() == "c1"
        ));
        update(&mut m, key(KeyCode::Enter));
        assert!(m.main_focused());
        assert!(matches!(
            m.hover(),
            Some(crate::document::Hover::Command(..))
        ));
        update(&mut m, key(KeyCode::Esc));
        assert!(!m.main_focused());
        assert_eq!(m.row_at(), 4, "back on the command it opened");
    }

    #[test]
    fn the_cursor_row_lives_in_the_panel_and_goes_when_the_panel_is_left_and_refocused() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        assert_eq!(m.panel, Panel::activity());
        update(&mut m, key(KeyCode::Char('j')));
        let Panel::Agent {
            row: ActivityRow::Subagent { by, id },
            ..
        } = m.panel.clone()
        else {
            panic!("the cursor is on a subagent: {:?}", m.panel);
        };
        assert_eq!(by, name("obs-layout-4"));
        assert!(
            m.watched(&(by.clone(), id.clone()))
                .and_then(|w| w.held.as_ref())
                .is_some_and(|s| !s.is_finished()),
            "remembered as listed"
        );

        update(&mut m, key(KeyCode::Enter));
        assert!(m.main_focused());
        assert_eq!(
            m.panel.activity_row().and_then(|row| match row {
                ActivityRow::Subagent { id, .. } => Some(id.clone()),
                ActivityRow::Main | ActivityRow::Command { .. } => None,
            }),
            Some(id),
            "the cursor stays on its row while [0] has the keys"
        );

        update(&mut m, key(KeyCode::Esc));
        update(&mut m, key(KeyCode::Esc));
        assert_eq!(m.panel, Panel::Agents);
        assert_eq!(m.panel.activity_row(), None);
        update(&mut m, key(KeyCode::Char('2')));
        assert_eq!(
            m.panel,
            Panel::activity(),
            "refocusing starts on the activity"
        );
    }

    #[test]
    fn a_held_row_is_marked_finished_once_its_agent_stops_listing_it() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        let (agent, id) = m.subagent_selected().expect("a subagent is selected");
        let mut cleared = m.activities.clone().expect("reported");
        cleared
            .get_mut(&agent)
            .expect("has activity")
            .subagents
            .remove(&id);
        update(&mut m, Msg::Activities(cleared));
        let Some(ActivityRow::Subagent { by, id }) = m.panel.activity_row() else {
            panic!("the cursor stays on the row");
        };
        assert!(
            m.watched(&(by.clone(), id.clone()))
                .and_then(|w| w.held.as_ref())
                .is_some_and(agent_core::activity::Subagent::is_finished),
            "nothing will say it finished afterwards"
        );

        m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
        assert_eq!(
            m.panel.activity_row(),
            None,
            "another selection drops the row"
        );
        assert_eq!(m.subagent_rows().len(), 0);
    }

    #[test]
    fn a_result_for_an_agent_that_was_never_asked_about_is_dropped() {
        let mut m = crate::fixtures::fleet(10);
        let facts = match m.facts(&name("obs-layout-4")) {
            Some(Ok(facts)) => facts.clone(),
            _ => panic!("facts"),
        };
        let stranger = name("removed-agent");
        update(
            &mut m,
            Msg::Inspected {
                name: stranger.clone(),
                read: Ok(Inspected::of(facts)),
            },
        );
        assert!(m.tracked(&stranger).is_none());
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
        assert!(m.watched(&(agent.clone(), id.clone())).is_some());
        assert!(matches!(
            m.hover(),
            Some(crate::document::Hover::Subagent(_, ref held)) if *held == id
        ));

        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(m.subagent_rows().len(), 2, "gone once the cursor moved on");
        assert!(
            matches!(m.panel.activity_row(), Some(ActivityRow::Subagent { id: held, .. }) if *held != id)
        );
        assert!(m.watched(&(agent, id)).is_none());
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
        for read in m
            .agents
            .values_mut()
            .flat_map(|t| t.subagents.values_mut())
            .filter_map(|w| w.watch.read.as_mut())
        {
            read.at -= LOG_REFRESH;
        }
        assert_eq!(reads(tick(&mut m)).len(), 1, "after it");

        let detail = agent_core::ports::Log {
            prompt: Some("do it".into()),
            ..crate::log::fixtures::detail(Vec::new())
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
            m.watched(&(name.clone(), id.clone()))
                .and_then(|w| w.watch.log.as_ref()),
            Some(&Ok(Some(detail.clone())))
        );

        let mut gone = m.activities.clone().expect("reported");
        gone.get_mut(&name).expect("has activity").subagents.clear();
        update(&mut m, Msg::Activities(gone));
        update(&mut m, key(KeyCode::Char('g')));
        assert_eq!(m.watching(), 0);
        update(
            &mut m,
            Msg::Subagent {
                name,
                id,
                detail: Ok(Some(detail)),
            },
        );
        assert!(
            m.watching() == 0,
            "a late answer for an unwatched subagent is dropped"
        );
    }

    #[test]
    fn with_only_an_activity_there_is_one_row_and_zero_or_enter_open_its_document() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("rate-limit-1")));
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(m.row_at(), 0, "nowhere to go");
        assert!(matches!(
            m.hover(),
            Some(crate::document::Hover::Activity(_))
        ));
        assert_eq!(reads(tick(&mut m)), [""; 0]);
        update(&mut m, key(KeyCode::Enter));
        assert!(m.main_focused());
    }

    #[test]
    fn j_and_k_in_the_activity_pick_what_zero_opens() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        assert!(matches!(
            m.hover(),
            Some(crate::document::Hover::Activity(_))
        ));
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
    fn an_agent_with_no_report_has_no_activity_to_focus() {
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
                    .is_some_and(|s| s.text.contains("no activity recorded"))
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
                } => Some(format!(
                    "{} {}",
                    key.change.path.display(),
                    worktree.display()
                )),
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

        paged(&mut m, 10);
        update(&mut m, key(KeyCode::Char('J')));
        assert_eq!(m.docs.scroll, 2);
        update(&mut m, key(KeyCode::Char('K')));
        update(&mut m, key(KeyCode::Char('K')));
        assert_eq!(m.docs.scroll, 0);
        update(&mut m, key_with(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(m.docs.scroll, 5, "half a page of [0], not of the list");
        assert!(matches!(m.panel, Panel::Changes { file: 0 }));
        update(&mut m, key_with(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(m.docs.scroll, 0);

        update(&mut m, key(KeyCode::Char('j')));
        m.settle();
        assert_eq!(m.docs.scroll, 0, "another file starts at the top");
    }

    #[test]
    fn j_and_k_keep_moving_the_list_and_scroll_nothing_from_the_list() {
        let mut m = crate::fixtures::fleet(10);
        paged(&mut m, 10);
        update(&mut m, key(KeyCode::Char('J')));
        assert_eq!(m.docs.scroll, 0, "the terminal doesn't scroll from NAV");
    }

    fn on_changes(name_of: &str) -> Model {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name(name_of)));
        update(&mut m, key(KeyCode::Char('3')));
        paged(&mut m, 10);
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
        assert!(matches!(m.panel.list(), Listed::Changes { file: 0 }));
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
        assert_eq!(checked_sessions(&m, &effects), ["obs-layout-4"]);
        let effects = crate::fixtures::sighted(&mut m, &[]);
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
        assert_eq!(checked_sessions(&m, &effects), ["obs-layout-4"]);
        let effects = crate::fixtures::sighted(&mut m, &[]);
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
        assert!(matches!(m.overlay, Overlay::Talk(_)), "the box opens");
        type_text(m, text);
        update(m, key(KeyCode::Enter))
    }

    fn worker_says(m: &mut Model, effects: &[Effect], result: Result<(), SendError>) {
        let (to, ticket) = effects
            .iter()
            .find_map(|e| match e {
                Effect::Job(Job::Reply { to, ticket, .. }) => Some((to.clone(), *ticket)),
                _ => None,
            })
            .expect("a message went to the worker");
        update(m, Msg::Sent { to, ticket, result });
    }

    fn delivered(m: &mut Model, agent: &str) {
        let Some(Compose::Sending { ticket, .. }) =
            m.tracked(&name(agent)).and_then(|t| t.compose.as_ref())
        else {
            panic!("a message is with the worker");
        };
        let ticket = *ticket;
        update(
            m,
            Msg::Sent {
                to: name(agent),
                ticket,
                result: Ok(()),
            },
        );
    }

    fn composing(m: &Model, agent: &str) -> Option<(Option<String>, String)> {
        match m.tracked(&name(agent))?.compose.as_ref()? {
            Compose::Editing { quote, text } => Some((
                quote.as_ref().map(|q| q.as_str().to_string()),
                text.as_str().to_string(),
            )),
            Compose::Sending { .. } => None,
        }
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
                matches!(&m.overlay, Overlay::Talk(to) if to.as_str() == "auth-refresh-2"),
                "{:?}",
                m.panel
            );
            update(&mut m, key(KeyCode::Esc));
        }
        update(&mut m, key(KeyCode::Char('z')));
        assert!(matches!(m.overlay, Overlay::None), "z is free");
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
                .is_some_and(|s| s.text.contains("hasn't reported through hooks")),
            "says why"
        );
        let mut m = model(&[]);
        m.set_cursor(Cursor::Entry(name("api")));
        update(&mut m, key(KeyCode::Char('s')));
        assert!(matches!(m.overlay, Overlay::None), "exited");
    }

    #[test]
    fn a_message_to_a_finished_agent_is_sent_now() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, "auth-refresh-2", done(), 1);
        let effects = talk_to(&mut m, "auth-refresh-2", "rebase");
        assert_eq!(replies(&effects), ["rebase"]);
        assert!(matches!(m.overlay, Overlay::None));
    }

    #[test]
    fn a_message_to_a_working_agent_is_typed_at_once() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, "auth-refresh-2", working(), 1);
        let effects = talk_to(&mut m, "auth-refresh-2", "rebase");
        assert_eq!(replies(&effects), ["rebase"], "the harness queues it");
    }

    #[test]
    fn a_message_to_a_delegating_agent_is_typed_at_once() {
        let mut m = crate::fixtures::fleet(10);
        set_state(
            &mut m,
            "auth-refresh-2",
            agent_core::activity::State::Delegating,
            1,
        );
        assert_eq!(
            replies(&talk_to(&mut m, "auth-refresh-2", "rebase")),
            ["rebase"]
        );
    }

    #[test]
    fn a_message_to_an_asking_agent_is_refused_and_keeps_its_text_in_the_box() {
        let mut m = crate::fixtures::fleet(10);
        crate::fixtures::ask(&mut m, AGENT);
        let effects = talk_to(&mut m, AGENT, "rebase");
        assert_eq!(replies(&effects), Vec::<String>::new(), "a prompt is open");
        assert!(matches!(m.overlay, Overlay::Talk(_)), "the box stays open");
        assert_eq!(composing(&m, AGENT), Some((None, "rebase".into())));
        assert_eq!(status(&m), format!("{AGENT} is asking · attach to answer"));
    }

    #[test]
    fn a_refused_message_is_not_sent_later_when_the_question_is_answered() {
        let mut m = crate::fixtures::fleet(10);
        crate::fixtures::ask(&mut m, AGENT);
        talk_to(&mut m, AGENT, "rebase");
        update(&mut m, key(KeyCode::Esc));
        let mut answered = reported(working(), 0);
        answered.events = 2;
        let effects = update(
            &mut m,
            Msg::Activities(HashMap::from([(name(AGENT), answered)])),
        );
        assert_eq!(replies(&effects), Vec::<String>::new());
        assert_eq!(replies(&tick(&mut m)), Vec::<String>::new());
    }

    #[test]
    fn enter_in_the_box_again_sends_once_the_question_is_answered() {
        let mut m = crate::fixtures::fleet(10);
        crate::fixtures::ask(&mut m, AGENT);
        talk_to(&mut m, AGENT, "rebase");
        let mut answered = reported(working(), 0);
        answered.events = 2;
        update(
            &mut m,
            Msg::Activities(HashMap::from([(name(AGENT), answered)])),
        );
        assert_eq!(replies(&update(&mut m, key(KeyCode::Enter))), ["rebase"]);
        assert!(matches!(m.overlay, Overlay::None));
    }

    #[test]
    fn a_message_goes_at_once_while_a_tool_has_just_started() {
        let mut m = crate::fixtures::fleet(10);
        let mut activity = reported(working(), 5);
        activity.state = State::Working {
            tools: vec![agent_core::activity::Tool {
                id: agent_core::activity::ToolId::new("t1"),
                name: "Bash".into(),
                target: None,
                since: m.clock.wall,
            }],
        };
        m.activities
            .get_or_insert_default()
            .insert(name(AGENT), activity);
        assert_eq!(replies(&talk_to(&mut m, AGENT, "rebase")), ["rebase"]);
    }

    #[test]
    fn messages_in_a_row_each_go_at_once_commands_included() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, working(), 1);
        for text in ["first", "/model", "second"] {
            assert_eq!(replies(&talk_to(&mut m, AGENT, text)), [text]);
            delivered(&mut m, AGENT);
        }
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
        set_state(&mut m, AGENT, working(), 1);
        let effects = talk_to(&mut m, AGENT, "rebase");
        worker_says(&mut m, &effects, Ok(()));
        assert_eq!(status(&m), format!("sent to {AGENT}"));
        assert_eq!(
            composing(&m, AGENT),
            None,
            "nothing is kept once it is typed"
        );

        let mut m = crate::fixtures::fleet(10);
        crate::fixtures::ask(&mut m, AGENT);
        talk_to(&mut m, AGENT, "rebase");
        assert_eq!(status(&m), format!("{AGENT} is asking · attach to answer"));

        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        let effects = talk_to(&mut m, AGENT, "");
        assert!(effects.is_empty());
        assert!(matches!(m.overlay, Overlay::Talk(_)), "the box stays");
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
    fn a_booting_record_is_refused_within_the_quiet_time() {
        let mut m = crate::fixtures::fleet(10);
        let mut a = reported(done(), 1);
        a.session = crate::fixtures::launched(Duration::from_secs(1));
        report(&mut m, a);
        assert_eq!(
            replies(&talk_to(&mut m, AGENT, "rebase")),
            Vec::<String>::new()
        );
        assert_eq!(replies(&tick(&mut m)), Vec::<String>::new());
    }

    #[test]
    fn a_launched_agent_shows_as_starting_until_it_reports_or_the_launch_is_given_up_on() {
        use crate::model::AgentStatus;
        use agent_core::activity::{Join, MainEvent, Reported};
        let status_of = |m: &Model| {
            let (_, entry) = m
                .snapshot()
                .and_then(|s| s.entry(&name(AGENT)))
                .expect("listed");
            m.status(entry)
        };
        let mut m = crate::fixtures::fleet(10);
        let mut launched = Activity::next(
            None,
            Reported::main(MainEvent::Launched(Join::Untraced)),
            m.clock.wall - Duration::from_secs(1),
        );
        launched.events = 1;
        report(&mut m, launched);
        assert!(
            matches!(status_of(&m), AgentStatus::Starting { .. }),
            "{:?}",
            status_of(&m)
        );

        let expired = later(&m, agent_core::activity::LAUNCH_GRACE);
        tick_at(&mut m, expired);
        assert_eq!(
            status_of(&m),
            AgentStatus::Idle,
            "a launch that reported nothing isn't working"
        );
    }

    #[test]
    fn the_card_and_the_send_refusal_agree_on_a_session_that_just_reported_starting() {
        use crate::model::AgentStatus;
        let mut m = crate::fixtures::fleet(10);
        let mut booted = reported(State::Idle, 1);
        booted.session = crate::fixtures::booting();
        booted.heard = m.clock.wall;
        report(&mut m, booted);
        let (_, entry) = m
            .snapshot()
            .and_then(|s| s.entry(&name(AGENT)))
            .expect("listed");
        assert!(matches!(m.status(entry), AgentStatus::Starting { .. }));
        assert_eq!(
            m.hold(&name(AGENT)),
            Some(agent_core::activity::Hold::Booting)
        );

        let quiet = later(&m, agent_core::activity::BOOT_QUIET);
        tick_at(&mut m, quiet);
        let (_, entry) = m
            .snapshot()
            .and_then(|s| s.entry(&name(AGENT)))
            .expect("listed");
        assert_eq!(m.status(entry), AgentStatus::Idle);
        assert_eq!(m.hold(&name(AGENT)), None);
    }

    #[test]
    fn an_agent_delegating_to_subagents_that_all_went_quiet_says_so_not_handing_back() {
        use crate::model::{AgentStatus, Doing};
        use agent_core::activity::{Launch, Subagent};
        let mut m = crate::fixtures::fleet(10);
        let mut a = reported(State::Delegating, 700);
        a.subagents.insert(
            AgentId::new("s"),
            Subagent::running(
                "general-purpose",
                None,
                Launch::Background,
                m.clock.wall - Duration::from_secs(600),
            ),
        );
        report(&mut m, a);
        let (_, entry) = m
            .snapshot()
            .and_then(|s| s.entry(&name(AGENT)))
            .expect("listed");
        let AgentStatus::Working { doing, .. } = m.status(entry) else {
            panic!("working");
        };
        assert!(matches!(doing, Doing::Lost(n) if n.get() == 1), "{doing:?}");
    }

    #[test]
    fn a_failed_send_keeps_the_text_for_the_next_s_and_says_why_as_an_error() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        let effects = talk_to(&mut m, AGENT, "rebase onto main");
        worker_says(
            &mut m,
            &effects,
            Err(SendError::Failed(Cause::message("no pty".into()))),
        );
        assert!(
            matches!(m.overlay, Overlay::None),
            "nothing reopens on its own"
        );
        assert_eq!(
            composing(&m, AGENT),
            Some((None, "rebase onto main".into()))
        );
        let said = m.status.as_ref().expect("a status");
        assert_eq!(said.kind, StatusKind::Error);
        assert!(said.text.contains("no pty") && said.text.contains("s to edit"));
        update(&mut m, key(KeyCode::Char('s')));
        assert_eq!(talk_of(&m), Some((None, "rebase onto main")));
    }

    #[test]
    fn a_refusal_from_the_worker_opens_nothing_and_is_an_info_not_an_error() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        let effects = talk_to(&mut m, AGENT, "rebase");
        assert!(matches!(m.overlay, Overlay::None));
        worker_says(&mut m, &effects, Err(SendError::Held(Hold::Asking)));
        assert!(matches!(m.overlay, Overlay::None));
        let said = m.status.as_ref().expect("a status");
        assert_eq!(said.kind, StatusKind::Info);
        assert_eq!(
            said.text,
            format!("{AGENT} is asking · not sent, s to edit")
        );
        assert_eq!(composing(&m, AGENT), Some((None, "rebase".into())));
    }

    #[test]
    fn a_refusal_while_another_overlay_is_open_leaves_it_and_keeps_the_text_for_the_next_s() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        let effects = talk_to(&mut m, AGENT, "rebase");
        m.overlay = Overlay::Help(HelpState::default());
        worker_says(&mut m, &effects, Err(SendError::Held(Hold::Asking)));
        assert!(matches!(m.overlay, Overlay::Help(_)), "left alone");
        update(&mut m, key(KeyCode::Esc));
        m.set_cursor(Cursor::Entry(name(AGENT)));
        update(&mut m, key(KeyCode::Char('s')));
        assert_eq!(talk_of(&m), Some((None, "rebase")));
    }

    #[test]
    fn s_while_a_message_is_with_the_worker_shows_it_sending_and_a_stale_answer_is_ignored() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, working(), 1);
        let first = talk_to(&mut m, AGENT, "one");
        update(&mut m, key(KeyCode::Char('s')));
        assert!(
            matches!(m.overlay, Overlay::Talk(_)),
            "the box shows it sending"
        );
        update(&mut m, key(KeyCode::Char('x')));
        assert!(
            matches!(m.tracked(&name(AGENT)).and_then(|t| t.compose.as_ref()), Some(Compose::Sending { text, .. }) if text.as_str() == "one"),
            "typing changes nothing while it is with the worker"
        );

        let other = m.ticket();
        update(
            &mut m,
            Msg::Sent {
                to: name(AGENT),
                ticket: other,
                result: Err(SendError::Held(Hold::Asking)),
            },
        );
        assert!(matches!(
            m.tracked(&name(AGENT)).and_then(|t| t.compose.as_ref()),
            Some(Compose::Sending { .. })
        ));
        worker_says(&mut m, &first, Ok(()));
        update(&mut m, key(KeyCode::Esc));

        assert_eq!(replies(&talk_to(&mut m, AGENT, "two")), ["two"]);
    }

    #[test]
    fn ctrl_u_cannot_cancel_a_message_that_may_still_be_typed_and_its_answer_closes_the_box() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, working(), 1);
        m.track(&name(AGENT)).draft = vec![drafted("a.rs", "+x")];
        let effects = talk_to(&mut m, AGENT, "one");
        update(&mut m, key(KeyCode::Char('s')));
        update(&mut m, key_with(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(status(&m), "sending · can't cancel");
        assert!(matches!(m.overlay, Overlay::Talk(_)), "still showing it");
        assert!(matches!(
            m.tracked(&name(AGENT)).and_then(|t| t.compose.as_ref()),
            Some(Compose::Sending { comments, .. }) if comments.len() == 1
        ));
        worker_says(&mut m, &effects, Ok(()));
        assert!(matches!(m.overlay, Overlay::None), "it was typed");
        assert_eq!(m.drafted(), 0);
    }

    fn open_box(m: &mut Model) {
        set_state(m, AGENT, working(), 1);
        m.set_cursor(Cursor::Entry(name(AGENT)));
        update(m, key(KeyCode::Char('s')));
    }

    #[test]
    fn alt_enter_ctrl_j_and_shift_enter_add_a_line_and_plain_enter_sends() {
        for (code, modifiers) in [
            (KeyCode::Enter, KeyModifiers::ALT),
            (KeyCode::Char('j'), KeyModifiers::CONTROL),
            (KeyCode::Enter, KeyModifiers::SHIFT),
        ] {
            let mut m = crate::fixtures::fleet(10);
            open_box(&mut m);
            type_text(&mut m, "one");
            assert!(
                update(&mut m, key_with(code, modifiers)).is_empty(),
                "{code:?}"
            );
            type_text(&mut m, "two");
            assert_eq!(
                talk_of(&m),
                Some((None, "one\ntwo")),
                "{code:?} {modifiers:?}"
            );
            assert!(matches!(m.overlay, Overlay::Talk(_)), "still writing");
            let sent = update(&mut m, key(KeyCode::Enter));
            assert_eq!(replies(&sent), ["one\ntwo"], "{code:?} {modifiers:?}");
        }
    }

    #[test]
    fn a_message_of_several_lines_is_sent_with_its_newlines() {
        let mut m = crate::fixtures::fleet(10);
        open_box(&mut m);
        update(&mut m, Msg::Paste("a\r\nb\rc".into()));
        update(&mut m, key_with(KeyCode::Enter, KeyModifiers::ALT));
        type_text(&mut m, "d");
        assert_eq!(
            replies(&update(&mut m, key(KeyCode::Enter))),
            ["a\nb\nc\nd"]
        );
    }

    #[test]
    fn the_caret_moves_through_the_message_and_typing_and_backspace_act_at_it() {
        let mut m = crate::fixtures::fleet(10);
        open_box(&mut m);
        type_text(&mut m, "ac");
        update(&mut m, key(KeyCode::Left));
        type_text(&mut m, "b");
        assert_eq!(talk_of(&m), Some((None, "abc")));
        update(&mut m, key(KeyCode::Right));
        update(&mut m, key(KeyCode::Right));
        update(&mut m, key(KeyCode::Left));
        update(&mut m, key(KeyCode::Backspace));
        assert_eq!(talk_of(&m), Some((None, "ac")));
        update(&mut m, Msg::Paste("X\nY".into()));
        assert_eq!(talk_of(&m), Some((None, "aX\nYc")));
    }

    #[test]
    fn backspace_at_the_start_of_a_line_joins_it_to_the_one_above() {
        let mut m = crate::fixtures::fleet(10);
        open_box(&mut m);
        type_text(&mut m, "one");
        update(&mut m, key_with(KeyCode::Enter, KeyModifiers::ALT));
        type_text(&mut m, "two");
        for _ in 0..3 {
            update(&mut m, key(KeyCode::Left));
        }
        update(&mut m, key(KeyCode::Backspace));
        assert_eq!(talk_of(&m), Some((None, "onetwo")));
    }

    #[test]
    fn up_and_down_move_the_caret_between_wrapped_rows_of_the_box() {
        let mut m = crate::fixtures::fleet(10);
        m.size = ratatui::layout::Rect::new(0, 0, 40, 30);
        open_box(&mut m);
        assert_eq!(
            crate::view::talk_room(m.size).map(|room| room.columns),
            crate::typed::Columns::new(30)
        );
        let first = "a".repeat(29);
        type_text(&mut m, &format!("{first} bcd"));
        update(&mut m, key(KeyCode::Up));
        type_text(&mut m, "X");
        assert_eq!(
            talk_of(&m),
            Some((None, format!("aaaX{} bcd", &first[3..]).as_str()))
        );
        update(&mut m, key(KeyCode::Down));
        type_text(&mut m, "Y");
        assert_eq!(
            talk_of(&m),
            Some((None, format!("aaaX{} bcdY", &first[3..]).as_str()))
        );
    }

    #[test]
    fn up_inside_the_rows_in_view_moves_the_caret_and_scrolls_only_at_the_edge() {
        let mut m = crate::fixtures::fleet(10);
        m.size = ratatui::layout::Rect::new(0, 0, 100, 24);
        open_box(&mut m);
        let lines: Vec<String> = (1..=20).map(|n| n.to_string()).collect();
        update(&mut m, Msg::Paste(lines.join("\n")));
        let top = |m: &Model| match m.tracked(&name(AGENT)).and_then(|t| t.compose.as_ref()) {
            Some(Compose::Editing { text, .. }) => text.top(),
            _ => panic!("the box is open"),
        };
        assert_eq!(top(&m), 8, "twelve rows end at the caret's row");
        for _ in 0..11 {
            update(&mut m, key(KeyCode::Up));
        }
        assert_eq!(top(&m), 8, "the caret moved inside the rows in view");
        update(&mut m, key(KeyCode::Up));
        assert_eq!(top(&m), 7, "it scrolled when the caret left them");
    }

    #[test]
    fn home_end_delete_and_word_keys_edit_the_line_at_the_caret() {
        let mut m = crate::fixtures::fleet(10);
        open_box(&mut m);
        type_text(&mut m, "one two three");
        update(&mut m, key_with(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(talk_of(&m), Some((None, "one two ")));
        update(&mut m, key_with(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(talk_of(&m), Some((None, "one ")));
        update(&mut m, key_with(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(talk_of(&m), Some((None, "")));
        type_text(&mut m, "abc");
        update(&mut m, key(KeyCode::Home));
        update(&mut m, key(KeyCode::Delete));
        assert_eq!(talk_of(&m), Some((None, "bc")));
        update(&mut m, key(KeyCode::End));
        type_text(&mut m, "d");
        update(&mut m, key_with(KeyCode::Char('a'), KeyModifiers::CONTROL));
        type_text(&mut m, "^");
        update(&mut m, key(KeyCode::End));
        type_text(&mut m, "$");
        assert_eq!(talk_of(&m), Some((None, "^bcd$")));
    }

    #[test]
    fn a_box_with_no_room_yet_still_takes_text_and_ignores_the_row_keys() {
        let mut m = crate::fixtures::fleet(10);
        open_box(&mut m);
        m.size = ratatui::layout::Rect::default();
        type_text(&mut m, "ab");
        update(&mut m, key(KeyCode::Up));
        update(&mut m, key(KeyCode::Down));
        type_text(&mut m, "c");
        assert_eq!(talk_of(&m), Some((None, "abc")));
    }

    #[test]
    fn the_comment_box_edits_at_the_caret_with_the_same_keys() {
        let mut m = crate::fixtures::fleet(10);
        m.overlay = Overlay::Comment(CommentDraft {
            to: name(AGENT),
            lines: Vec::new(),
            text: Message::default(),
            rev: None,
        });
        for c in "ad".chars() {
            update(&mut m, key(KeyCode::Char(c)));
        }
        update(&mut m, key(KeyCode::Left));
        update(&mut m, key(KeyCode::Char('b')));
        update(&mut m, key(KeyCode::Char('c')));
        update(&mut m, key(KeyCode::Right));
        update(&mut m, key_with(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert!(
            matches!(&m.overlay, Overlay::Comment(CommentDraft { text, .. }) if text.as_str() == ""),
        );
    }

    #[test]
    fn ctrl_u_clears_the_whole_multi_line_message() {
        let mut m = crate::fixtures::fleet(10);
        open_box(&mut m);
        type_text(&mut m, "one");
        update(&mut m, key_with(KeyCode::Enter, KeyModifiers::ALT));
        update(&mut m, key_with(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(talk_of(&m), Some((None, "")));
    }

    #[test]
    fn quitting_asks_about_a_message_being_written_or_sent_not_about_an_empty_box() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, working(), 1);
        m.set_cursor(Cursor::Entry(name(AGENT)));
        update(&mut m, key(KeyCode::Char('s')));
        update(&mut m, key(KeyCode::Esc));
        assert!(matches!(
            update(&mut m, key(KeyCode::Char('q'))).as_slice(),
            [Effect::Quit]
        ));
        talk_to(&mut m, AGENT, "one");
        update(&mut m, key(KeyCode::Char('q')));
        assert!(matches!(
            m.overlay,
            Overlay::Confirm(Confirm::Quit(Unsent {
                comments: 0,
                messages: 1,
                restarts: 0
            }))
        ));
        update(&mut m, key(KeyCode::Esc));
        delivered(&mut m, AGENT);
        m.set_cursor(Cursor::Entry(name(AGENT)));
        update(&mut m, key(KeyCode::Char('s')));
        type_text(&mut m, "half");
        update(&mut m, key(KeyCode::Esc));
        update(&mut m, key(KeyCode::Char('q')));
        assert!(matches!(
            m.overlay,
            Overlay::Confirm(Confirm::Quit(Unsent { messages: 1, .. }))
        ));
    }

    #[test]
    fn an_agent_that_goes_takes_its_box_and_says_what_it_dropped_without_a_phantom_record() {
        let mut m = crate::fixtures::fleet(10);
        let gone = name("removed-agent");
        m.track(&gone).compose = Some(Compose::Editing {
            quote: None,
            text: "unsent".into(),
        });
        m.overlay = Overlay::Talk(gone.clone());
        m.track(&gone).listed();
        forget_gone(&mut m);
        assert!(matches!(m.overlay, Overlay::None));
        assert!(m.tracked(&gone).is_none());
        assert_eq!(
            status(&m),
            "removed-agent is gone · dropped the message for it"
        );

        update(&mut m, Msg::Paste("late".into()));
        m.overlay = Overlay::Talk(gone.clone());
        update(&mut m, key(KeyCode::Char('x')));
        update(&mut m, Msg::Paste("late".into()));
        assert!(m.tracked(&gone).is_none(), "no record is made for it");
    }

    #[test]
    fn esc_keeps_what_was_typed_for_the_next_s_and_ctrl_u_clears_it() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, working(), 1);
        m.set_cursor(Cursor::Entry(name(AGENT)));
        update(&mut m, key(KeyCode::Char('s')));
        type_text(&mut m, "half a thou");
        update(&mut m, key(KeyCode::Esc));
        assert!(matches!(m.overlay, Overlay::None));
        update(&mut m, key(KeyCode::Char('s')));
        assert_eq!(talk_of(&m), Some((None, "half a thou")));
        update(&mut m, key_with(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(talk_of(&m), Some((None, "")));
        update(&mut m, key(KeyCode::Esc));
        assert_eq!(
            composing(&m, AGENT),
            None,
            "an empty box leaves nothing behind"
        );
    }

    fn drafted(path: &str, text: &str) -> crate::review::Comment {
        let line = crate::fixtures::line_info(path, text, 1);
        crate::review::Comment::on(&[&line], None, "Fix.").expect("a comment")
    }

    #[test]
    fn a_review_of_a_crlf_file_is_sent_clean_and_a_control_character_gets_its_own_message() {
        let mut m = crate::fixtures::fleet(10);
        let agent = "auth-refresh-2";
        set_state(&mut m, agent, done(), 1);
        m.track(&name(agent)).draft = vec![drafted("a.rs", "+let x = 1;\r")];
        let effects = talk_to(&mut m, agent, "");
        assert_eq!(replies(&effects).len(), 1);
        assert!(!replies(&effects)[0].contains('\r'));

        talk_to(&mut m, "obs-layout-4", "a\x1b[201~b");
        assert!(matches!(m.overlay, Overlay::Talk(_)));
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("control character")),
            "{:?}",
            m.status.as_ref().map(|s| &s.text)
        );
        update(&mut m, key_with(KeyCode::Char('u'), KeyModifiers::CONTROL));
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
        assert_eq!(m.drafted(), 0);
    }

    #[test]
    fn a_comment_with_a_control_character_is_refused_and_keeps_its_box() {
        let mut m = crate::fixtures::fleet(10);
        let agent = name("auth-refresh-2");
        let line = crate::fixtures::line_info("a.rs", "+x", 1);
        add_comment(
            &mut m,
            &agent,
            &[line],
            "a\x1bb",
            Some(crate::review::Rev::new("abc1234".into())),
        );
        assert_eq!(m.drafted(), 0);
        assert!(
            matches!(&m.overlay, Overlay::Comment(CommentDraft { text, rev, .. })
                if text.as_str() == "a\x1bb" && rev.as_ref().map(ToString::to_string).as_deref() == Some("abc1234")),
            "the box stays with its commit"
        );
        assert!(
            m.status
                .as_ref()
                .is_some_and(|s| s.text.contains("control character"))
        );
    }

    #[test]
    fn everything_known_about_an_agent_goes_with_it() {
        let mut m = crate::fixtures::fleet(10);
        let kept = name("auth-refresh-2");
        let gone = name("removed-agent");
        let facts = match m.facts(&name("obs-layout-4")) {
            Some(Ok(facts)) => facts.clone(),
            _ => panic!("facts"),
        };
        for who in [&kept, &gone] {
            m.set_test_facts(who, Ok(facts.clone()));
            let tracked = m.track(who);
            tracked.collapsed.insert("src".into());
            tracked.draft = vec![drafted("a.rs", "+x")];
            tracked.inspection = Some(Inspection {
                events: None,
                at: Instant::now(),
            });
            tracked.company = crate::fixtures::in_nvim();
            tracked.launched(SystemTime::now());
            tracked.listed();
            m.set_log(
                (who.clone(), AgentId::new("s1")),
                Ok(Some(crate::log::fixtures::detail(Vec::new()))),
            );
        }
        forget_gone(&mut m);
        assert!(m.tracked(&kept).is_some());
        assert!(m.watched(&(kept.clone(), AgentId::new("s1"))).is_some());
        assert!(m.tracked(&gone).is_none(), "nothing about it is kept");
        assert!(m.agents.keys().all(|name| *name != gone));
        assert_eq!(*m.company(&gone), Company::Alone, "its windows go too");
        assert_eq!(*m.company(&kept), crate::fixtures::in_nvim());
    }

    #[test]
    fn a_launch_is_kept_until_its_session_has_been_listed_once() {
        let mut m = crate::fixtures::fleet(10);
        let slow = name("slow-checkout");
        let launched = SystemTime::now();
        m.track(&slow).launched(launched);
        forget_gone(&mut m);
        assert_eq!(
            m.tracked(&slow).map(|t| t.seen),
            Some(Seen::Pending {
                launched_at: launched
            }),
            "a snapshot without it yet keeps the launch"
        );

        m.track(&slow).listed();
        forget_gone(&mut m);
        assert!(m.tracked(&slow).is_none(), "gone once it has been listed");
    }

    #[test]
    fn a_launch_that_never_shows_up_is_dropped_after_a_while_and_says_what_it_took() {
        let mut m = crate::fixtures::fleet(10);
        let late = name("never-starts");
        let quiet = name("quiet-never-starts");
        let long_ago = SystemTime::now() - crate::track::PENDING_LAUNCH_FOR * 2;
        for who in [&late, &quiet] {
            m.track(who).launched(long_ago);
        }
        m.track(&late).draft = vec![drafted("a.rs", "+x")];
        forget_gone(&mut m);
        assert!(m.tracked(&late).is_none() && m.tracked(&quiet).is_none());
        assert!(
            status(&m).contains("never-starts never started")
                && status(&m).contains("dropped 1 drafted"),
            "{}",
            status(&m)
        );
        assert_eq!(m.drafted(), 0);
    }

    #[test]
    fn epochs_never_repeat_for_an_agent_removed_and_recreated() {
        let mut m = crate::fixtures::fleet(10);
        let facts = match m.facts(&name("obs-layout-4")) {
            Some(Ok(facts)) => facts.clone(),
            _ => panic!("facts"),
        };
        let again = name("removed-agent");
        m.set_test_facts(&again, Ok(facts.clone()));
        let epoch = |m: &Model| {
            m.tracked(&again)
                .and_then(|t| t.facts.as_ref())
                .expect("read")
                .epoch
        };
        let first = epoch(&m);
        m.track(&again).listed();
        forget_gone(&mut m);
        assert!(m.tracked(&again).is_none());
        m.set_test_facts(&again, Ok(facts));
        assert!(epoch(&m) > first);
    }

    #[test]
    fn a_review_that_fails_to_send_gives_its_comments_back_and_one_that_sends_does_not() {
        let mut m = crate::fixtures::fleet(10);
        set_state(&mut m, AGENT, done(), 1);
        m.track(&name(AGENT)).draft = vec![drafted("a.rs", "+x")];
        let effects = talk_to(&mut m, AGENT, "");
        assert_eq!(m.drafted(), 0, "with the worker, so not in the draft");
        m.track(&name(AGENT)).draft = vec![drafted("b.rs", "+y")];
        worker_says(
            &mut m,
            &effects,
            Err(SendError::Failed(Cause::message("no pty".into()))),
        );
        let paths: Vec<_> = m
            .draft(&name(AGENT))
            .iter()
            .map(|c| c.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, ["a.rs", "b.rs"], "back ahead of newer comments");

        for tracked in m.agents.values_mut() {
            tracked.draft.clear();
        }
        m.overlay = Overlay::None;
        m.track(&name(AGENT)).draft = vec![drafted("a.rs", "+x")];
        let effects = talk_to(&mut m, AGENT, "");
        worker_says(&mut m, &effects, Ok(()));
        assert_eq!(m.drafted(), 0, "delivered comments stay gone");
    }

    const AGENT: &str = "auth-refresh-2";

    fn report(m: &mut Model, mut activity: Activity) -> Vec<Effect> {
        activity.events = m.activity(&name(AGENT)).map_or(0, |a| a.events) + 1;
        update(m, Msg::Activities(HashMap::from([(name(AGENT), activity)])))
    }

    #[test]
    fn a_fresh_session_start_gets_quiet_time_for_a_startup_dialog() {
        let mut m = crate::fixtures::fleet(10);
        let mut booted = reported(done(), 1);
        booted.session = crate::fixtures::booting();
        report(&mut m, booted);
        assert_eq!(
            replies(&talk_to(&mut m, AGENT, "rebase")),
            Vec::<String>::new()
        );
        assert_eq!(m.hold(&name(AGENT)), Some(Hold::Booting));
        assert_eq!(
            status(&m),
            format!("{AGENT} is still booting · try again in a moment")
        );
        assert_eq!(composing(&m, AGENT), Some((None, "rebase".into())));
        let mut quiet = reported(done(), 6);
        quiet.session = crate::fixtures::booting();
        report(&mut m, quiet);
        assert_eq!(replies(&update(&mut m, key(KeyCode::Enter))), ["rebase"]);
    }

    #[test]
    fn q_with_drafted_comments_asks_before_quitting() {
        let mut m = crate::fixtures::fleet(10);
        assert!(matches!(
            update(&mut m, key(KeyCode::Char('q'))).as_slice(),
            [Effect::Quit]
        ));
        m.track(&name(AGENT)).draft = vec![drafted("a.rs", "+x")];
        assert!(update(&mut m, key(KeyCode::Char('q'))).is_empty());
        assert!(matches!(
            m.overlay,
            Overlay::Confirm(Confirm::Quit(Unsent {
                comments: 1,
                messages: 0,
                restarts: 0
            }))
        ));
        update(&mut m, key(KeyCode::Esc));
        assert!(matches!(m.overlay, Overlay::None));
        update(&mut m, key(KeyCode::Char('q')));
        assert!(matches!(
            update(&mut m, key(KeyCode::Char('y'))).as_slice(),
            [Effect::Quit]
        ));
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
        assert_eq!(m.panel, Panel::activity(), "Esc closed only the peek");
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
        assert!(matches!(m.overlay, Overlay::Talk(_)));
        type_text(&mut m, "hi");
        let effects = update(&mut m, key(KeyCode::Enter));
        assert_eq!(replies(&effects), ["hi"]);
        assert!(m.peeking(), "back to the peek");
    }

    #[test]
    fn s_in_a_peek_over_a_log_sends_without_quoting_the_entry() {
        let (mut m, who) = reading_the_story();
        update(&mut m, key(KeyCode::Char('g')));
        assert!(m.reply.is_some(), "in [0] it quotes");
        update(&mut m, key(KeyCode::Char('v')));
        assert!(m.peek.is_some(), "the peek of {who} opens");
        assert!(m.reply.is_none());
        update(&mut m, key(KeyCode::Char('s')));
        assert_eq!(talk_of(&m), Some((None, "")));
    }

    #[test]
    fn i_and_enter_in_the_peek_enter_the_agent() {
        for code in [KeyCode::Char('i'), KeyCode::Enter] {
            let mut m = crate::fixtures::fleet(10);
            m.set_cursor(Cursor::Entry(name("obs-layout-4")));
            update(&mut m, key(KeyCode::Char('v')));
            let effects = update(&mut m, key(code));
            assert_eq!(checked_sessions(&m, &effects), ["obs-layout-4"], "{code:?}");
            let effects = crate::fixtures::sighted(&mut m, &[]);
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
    fn e_on_a_subagent_row_opens_its_transcript_and_on_the_activity_row_the_checkout() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        let effects = update(&mut m, key(KeyCode::Char('e')));
        assert!(
            matches!(effects.as_slice(), [Effect::Edit(Open::Checkout(_))]),
            "the activity row"
        );

        update(&mut m, key(KeyCode::Char('j')));
        let key_of = m.subagent_selected().expect("a subagent is selected");
        let effects = update(&mut m, key(KeyCode::Char('e')));
        assert!(edits(&effects) == 0, "not read yet");

        m.set_log(
            key_of,
            Ok(Some(agent_core::ports::Log {
                transcript: TranscriptRef::new("/p/s/subagents/agent-a1.jsonl"),
                ..crate::log::fixtures::detail(Vec::new())
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
        let ok = || CallState::Done {
            summary: Summary::NoOutput,
            took: None,
        };
        let mut d = detail(vec![
            said("one"),
            bash("ls", ok()),
            said("two"),
            bash("pwd", ok()),
        ]);
        if finished {
            d.outcome = agent_core::ports::Outcome::Finished { report: None };
        }
        m.set_log((name.clone(), id.clone()), Ok(Some(d)));
        (name, id)
    }

    /// [0] focused on the log of the delegating agent's first subagent.
    fn reading_a_subagent(finished: bool) -> (Model, (SessionName, AgentId)) {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        let subagent = with_log(&mut m, finished);
        update(&mut m, key(KeyCode::Char('0')));
        (m, subagent)
    }

    #[test]
    fn the_log_cursor_moves_by_entry_folds_and_opens_the_transcript_at_it() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        let key_ = with_log(&mut m, true);
        update(&mut m, key(KeyCode::Char('0')));
        let nav = |m: &Model| {
            m.watched(&key_)
                .map(|w| w.watch.nav.clone())
                .unwrap_or_default()
        };

        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(
            nav(&m).cursor,
            Some(EntryKey::Log(0)),
            "a finished log starts at the prompt, so one step is its first entry"
        );
        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Enter));
        assert!(nav(&m).is_open(&EntryKey::Log(1)));
        update(&mut m, key(KeyCode::Enter));
        assert!(!nav(&m).is_open(&EntryKey::Log(1)));
        update(&mut m, key(KeyCode::Char('=')));
        assert!(nav(&m).is_open(&EntryKey::Log(3)));
        update(&mut m, key(KeyCode::Char('-')));
        assert!(!nav(&m).is_open(&EntryKey::Log(3)));
        update(&mut m, key(KeyCode::Char('G')));
        assert_eq!(nav(&m).cursor, Some(EntryKey::Log(3)));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(
            nav(&m).cursor,
            Some(EntryKey::Log(3)),
            "stops at the last entry"
        );
        update(&mut m, key(KeyCode::Char('g')));
        assert_eq!(nav(&m).cursor, Some(EntryKey::Prompt));

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
        let (mut m, _) = reading_a_subagent(false);
        update(&mut m, key(KeyCode::Char('k')));
        assert!(m.docs.paused() || !m.docs.following());
        update(&mut m, key(KeyCode::Char('G')));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(
            m.agents
                .values()
                .flat_map(|t| t.subagents.values())
                .next()
                .and_then(|w| w.watch.nav.cursor.clone()),
            Some(EntryKey::Log(3))
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
        assert_eq!(m.draft(&name).len(), 2);

        add_comment(&mut m, &name, &lines, "Use the configured limit.", None);
        assert_eq!(
            m.draft(&name).len(),
            2,
            "the same line replaces its comment"
        );
        assert_eq!(m.draft(&name)[0].text, "Use the configured limit.");

        add_comment(&mut m, &name, &[line("+more", 9)], "", None);
        assert_eq!(
            m.draft(&name).len(),
            1,
            "empty text on a commented line deletes it"
        );

        update(&mut m, key(KeyCode::Char('s')));
        assert!(matches!(m.overlay, Overlay::Talk(_)));
        update(&mut m, key(KeyCode::Char('o')));
        let effects = update(&mut m, key(KeyCode::Enter));
        assert!(m.drafted() == 0, "the draft clears once it is typed");
        let sent = &replies(&effects)[0];
        assert!(sent.starts_with("Review comments."), "{sent}");
        assert!(
            sent.contains("\n\no\n\na.rs:4\n> +let x = 3;\nUse the configured limit."),
            "{sent}"
        );
        let _ = Comment::on(&[&line("+x", 1)], None, "");
    }

    #[test]
    fn y_in_a_subagent_log_copies_the_entry_under_the_cursor() {
        let (mut m, _) = reading_a_subagent(true);
        update(&mut m, key(KeyCode::Char('j')));
        let effects = update(&mut m, key(KeyCode::Char('y')));
        assert!(matches!(effects.as_slice(), [Effect::Copy(text)] if text == "one"));
    }

    #[test]
    fn y_on_the_activity_copies_the_agents_message() {
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

    fn at_work() -> Model {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("payroll-fix-3")));
        m
    }

    fn activity_reads(effects: Vec<Effect>) -> Vec<(String, Option<String>)> {
        effects
            .into_iter()
            .filter_map(|e| match e {
                Effect::ReadActivity { name, at, workdir } => Some((
                    format!("{name} {}", workdir.display()),
                    at.map(|a| a.as_str().to_string()),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_activity_is_asked_for_through_an_effect_only_while_it_is_shown_and_when_it_may_have_moved()
     {
        let mut m = at_work();
        m.activities
            .as_mut()
            .and_then(|a| a.get_mut(&name("payroll-fix-3")))
            .expect("reports")
            .transcript = Some(TranscriptRef::new("/p/s.jsonl"));
        assert!(
            activity_reads(tick(&mut m)).is_empty(),
            "[0] shows the terminal, which needs no transcript"
        );

        update(&mut m, key(KeyCode::Char('2')));
        assert_eq!(
            activity_reads(tick(&mut m)),
            [(
                "payroll-fix-3 /w/heucristic0/payroll-fix-3".to_string(),
                Some("/p/s.jsonl".to_string())
            )],
            "the checkout and the transcript the hooks named, for the worker"
        );
        assert!(activity_reads(tick(&mut m)).is_empty(), "once");

        let again = |m: &mut Model, back: Duration| {
            m.track(&name("payroll-fix-3"))
                .activity_view
                .watch
                .read
                .as_mut()
                .expect("read")
                .at -= back;
            activity_reads(tick(m)).len()
        };
        assert_eq!(again(&mut m, LOG_REFRESH), 0, "nothing was reported");
        assert_eq!(
            again(&mut m, LOG_SLOW_REFRESH),
            1,
            "a working agent's transcript is read again whatever its hooks say"
        );

        m.activities
            .as_mut()
            .and_then(|a| a.get_mut(&name("payroll-fix-3")))
            .expect("reports")
            .events += 1;
        assert_eq!(again(&mut m, LOG_REFRESH), 1, "soon after it reports");
        assert_eq!(
            activity_reads(tick(&mut m)).len() + again(&mut m, Duration::ZERO),
            0,
            "not before the interval"
        );
    }

    #[test]
    fn a_finished_activity_is_read_again_only_when_its_agent_reports_something() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
        update(&mut m, key(KeyCode::Char('2')));
        assert_eq!(activity_reads(tick(&mut m)).len(), 1);
        let who = name("auth-refresh-2");
        m.track(&who)
            .activity_view
            .watch
            .read
            .as_mut()
            .expect("read")
            .at -= LOG_SLOW_REFRESH * 3;
        assert!(
            activity_reads(tick(&mut m)).is_empty(),
            "its transcript can still grow, but only a report says so"
        );
    }

    #[test]
    fn an_answer_replaces_the_activity_and_a_new_activity_starts_the_cursor_over() {
        let mut m = at_work();
        let who = name("payroll-fix-3");
        let detail = |mark: &str| {
            let mut detail =
                crate::log::fixtures::activity_of(vec![crate::log::fixtures::said("one")]);
            detail.mark = Some(agent_core::ports::ActivityMark::new(mark));
            detail
        };
        update(
            &mut m,
            Msg::Activity {
                name: who.clone(),
                detail: Ok(Some(detail("p1"))),
            },
        );
        m.track(&who).activity_view.watch.nav.cursor = Some(EntryKey::Log(1));
        update(
            &mut m,
            Msg::Activity {
                name: who.clone(),
                detail: Ok(Some(detail("p1"))),
            },
        );
        assert_eq!(
            m.track(&who).activity_view.watch.nav.cursor,
            Some(EntryKey::Log(1)),
            "the same activity, grown"
        );
        update(
            &mut m,
            Msg::Activity {
                name: who.clone(),
                detail: Ok(Some(detail("p2"))),
            },
        );
        assert_eq!(
            m.track(&who).activity_view.watch.nav.cursor,
            None,
            "a new prompt"
        );
        update(
            &mut m,
            Msg::Activity {
                name: who.clone(),
                detail: Err(Cause::message("denied".into())),
            },
        );
        assert!(m.activity_detail(&who).is_none());
        update(
            &mut m,
            Msg::Activity {
                name: name("nobody"),
                detail: Ok(None),
            },
        );
        assert!(
            m.tracked(&name("nobody")).is_none(),
            "an unknown agent is dropped"
        );
    }

    fn story() -> Vec<agent_core::ports::LogEntry> {
        use crate::log::fixtures::{bash, said};
        use agent_core::ports::{CallState, Summary};
        let ok = || CallState::Done {
            summary: Summary::NoOutput,
            took: None,
        };
        vec![
            said("one"),
            bash("ls", ok()),
            said("two"),
            bash("pwd", ok()),
            said("three"),
            bash("cargo test", CallState::Running),
        ]
    }

    #[test]
    fn the_activity_log_has_the_entry_cursor_folds_follow_and_copy_a_subagent_log_has() {
        let mut m = at_work();
        let who = reading(&mut m, crate::log::fixtures::activity_of(story()));
        let nav = |m: &Model| {
            m.nav(&LogKey::Activity(who.clone()))
                .cloned()
                .unwrap_or_default()
        };

        update(&mut m, key(KeyCode::Char('k')));
        assert_eq!(
            nav(&m).cursor,
            Some(EntryKey::Log(4)),
            "a working activity starts on its last entry: prompt, two folded phases, then three said and one call"
        );
        let paused = !m.docs.following();
        assert!(paused, "up pauses the follow");
        update(&mut m, key(KeyCode::Char('g')));
        assert_eq!(nav(&m).cursor, Some(EntryKey::Prompt));
        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Enter));
        assert!(
            nav(&m).is_open(&EntryKey::Log(0)),
            "⏎ opens the folded phase"
        );
        update(&mut m, key(KeyCode::Enter));
        assert!(!nav(&m).is_open(&EntryKey::Log(0)));
        update(&mut m, key(KeyCode::Char('=')));
        assert!(nav(&m).is_open(&EntryKey::Log(2)));
        update(&mut m, key(KeyCode::Char('-')));
        assert!(!nav(&m).is_open(&EntryKey::Log(2)));
        update(&mut m, key(KeyCode::Char('G')));
        update(&mut m, key(KeyCode::Char('j')));
        assert_eq!(
            nav(&m).cursor,
            Some(EntryKey::Log(5)),
            "stops at the last entry"
        );

        update(&mut m, key(KeyCode::Char('g')));
        update(&mut m, key(KeyCode::Char('j')));
        let effects = update(&mut m, key(KeyCode::Char('y')));
        assert!(
            matches!(effects.as_slice(), [Effect::Copy(text)] if text.contains("input")),
            "y copies the phase's entry"
        );
    }

    fn cursor_of(m: &Model, key: &LogKey) -> Option<EntryKey> {
        m.nav(key).and_then(|nav| nav.cursor.clone())
    }

    /// [0] focused on `activity`, the working agent's current one.
    fn reading(m: &mut Model, activity: agent_core::ports::ActivityDetail) -> SessionName {
        let who = name("payroll-fix-3");
        m.set_activity(&who, Ok(Some(activity)));
        update(m, key(KeyCode::Char('2')));
        update(m, key(KeyCode::Char('0')));
        who
    }

    /// An agent at work with `story()` as its activity, [0] focused on it.
    fn reading_the_story() -> (Model, SessionName) {
        let mut m = at_work();
        let who = reading(&mut m, crate::log::fixtures::activity_of(story()));
        (m, who)
    }

    /// The cursor after each of `keys`.
    fn after_each(m: &mut Model, log: &LogKey, keys: &str) -> Vec<Option<EntryKey>> {
        keys.chars()
            .map(|c| {
                update(m, key(KeyCode::Char(c)));
                cursor_of(m, log)
            })
            .collect()
    }

    #[test]
    fn brackets_move_the_activity_cursor_to_the_first_entry_of_the_previous_or_next_phase() {
        let (mut m, who) = reading_the_story();
        let log = LogKey::Activity(who);
        update(&mut m, key(KeyCode::Char('g')));
        assert_eq!(
            after_each(&mut m, &log, "]]]]"),
            [
                Some(EntryKey::Log(0)),
                Some(EntryKey::Log(2)),
                Some(EntryKey::Log(4)),
                Some(EntryKey::Log(4)),
            ],
            "two folded phases, then the open one from its first message, and the end stays put"
        );
        update(&mut m, key(KeyCode::Char('G')));
        assert_eq!(cursor_of(&m, &log), Some(EntryKey::Log(5)));
        assert_eq!(
            after_each(&mut m, &log, "[[[["),
            [
                Some(EntryKey::Log(4)),
                Some(EntryKey::Log(2)),
                Some(EntryKey::Log(0)),
                Some(EntryKey::Log(0)),
            ],
            "back from inside a phase goes to its start, and the first phase stays put"
        );
        update(&mut m, key(KeyCode::Char('g')));
        assert_eq!(after_each(&mut m, &log, "["), [Some(EntryKey::Prompt)]);
    }

    #[test]
    fn brackets_on_a_running_log_start_from_its_end_when_the_cursor_was_never_moved() {
        let (mut m, who) = reading_the_story();
        let log = LogKey::Activity(who);
        assert_eq!(cursor_of(&m, &log), None);
        assert_eq!(after_each(&mut m, &log, "]"), [None], "already at the end");
        assert_eq!(
            after_each(&mut m, &log, "["),
            [Some(EntryKey::Log(4))],
            "the end is inside the open phase, so back goes to its start"
        );
    }

    #[test]
    fn jumping_to_a_phase_that_is_the_last_entry_of_a_running_log_scrolls_to_its_end() {
        use crate::log::fixtures::{activity_of, bash, said};
        use agent_core::ports::{CallState, Summary};
        let mut m = at_work();
        let who = name("payroll-fix-3");
        let ok = CallState::Done {
            summary: Summary::NoOutput,
            took: None,
        };
        m.set_activity(
            &who,
            Ok(Some(activity_of(vec![
                said("one"),
                bash("ls", ok),
                said("two"),
            ]))),
        );
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('0')));
        update(&mut m, key(KeyCode::Char('g')));
        update(&mut m, key(KeyCode::Char(']')));
        assert_ne!(m.docs.scroll, usize::MAX, "not on the last entry yet");
        update(&mut m, key(KeyCode::Char(']')));
        assert_eq!(
            cursor_of(&m, &LogKey::Activity(who)),
            Some(EntryKey::Log(2))
        );
        assert_eq!(
            m.docs.scroll,
            usize::MAX,
            "scrolled to the end, which follows it"
        );
    }

    #[test]
    fn brackets_move_a_subagent_logs_cursor_between_its_messages() {
        let (mut m, subagent) = reading_a_subagent(true);
        let subagent = LogKey::Subagent(subagent);
        update(&mut m, key(KeyCode::Char('g')));
        assert_eq!(
            after_each(&mut m, &subagent, "]]]["),
            [
                Some(EntryKey::Log(0)),
                Some(EntryKey::Log(2)),
                Some(EntryKey::Log(2)),
                Some(EntryKey::Log(0)),
            ]
        );
    }

    #[test]
    fn a_running_activity_with_nothing_in_it_ignores_the_entry_keys_instead_of_panicking() {
        let mut m = at_work();
        let mut empty = crate::log::fixtures::activity_of(Vec::new());
        empty.log.prompt = None;
        reading(&mut m, empty);
        for c in ['j', ']', 'y', 's'] {
            update(&mut m, key(KeyCode::Char(c)));
        }
        assert!(m.hover().is_some(), "still alive");
    }

    fn talk_of(m: &Model) -> Option<(Option<&str>, &str)> {
        let Overlay::Talk(to) = &m.overlay else {
            return None;
        };
        match m.tracked(to)?.compose.as_ref()? {
            Compose::Editing { quote, text } => {
                Some((quote.as_ref().map(crate::log::Quote::as_str), text.as_str()))
            }
            Compose::Sending { .. } => None,
        }
    }

    #[test]
    fn s_on_an_activity_entry_opens_the_box_quoting_it_and_elsewhere_opens_it_without() {
        let (mut m, _) = reading_the_story();
        update(&mut m, key(KeyCode::Esc));
        update(&mut m, key(KeyCode::Char('s')));
        assert_eq!(
            talk_of(&m),
            Some((None, "")),
            "from the list, nothing is quoted"
        );
        update(&mut m, key(KeyCode::Esc));
        update(&mut m, key(KeyCode::Char('0')));

        update(&mut m, key(KeyCode::Char('g')));
        update(&mut m, key(KeyCode::Char('s')));
        assert_eq!(
            talk_of(&m),
            Some((Some("Fix the statusline."), "")),
            "the prompt"
        );
        update(&mut m, key(KeyCode::Esc));

        update(&mut m, key(KeyCode::Char(']')));
        update(&mut m, key(KeyCode::Char('s')));
        assert_eq!(
            talk_of(&m),
            Some((Some("one"), "")),
            "a folded phase quotes what it said"
        );
        update(&mut m, key(KeyCode::Esc));

        update(&mut m, key(KeyCode::Char('G')));
        update(&mut m, key(KeyCode::Char('s')));
        assert_eq!(
            talk_of(&m),
            Some((Some("Bash cargo test"), "")),
            "a call quotes its summary"
        );
    }

    #[test]
    fn s_on_a_subagent_log_entry_quotes_it_naming_the_subagent_without_control_characters() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        let (who, id) = m.subagent_selected().expect("a subagent");
        m.set_log(
            (who, id),
            Ok(Some(crate::log::fixtures::detail(vec![
                crate::log::fixtures::said("\u{1b}[2Jwiped\nsecond"),
            ]))),
        );
        update(&mut m, key(KeyCode::Char('0')));
        update(&mut m, key(KeyCode::Char('g')));
        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Char('s')));
        let (quote, text) = talk_of(&m).expect("the box");
        assert!(
            quote.is_some_and(|q| q.starts_with("(subagent") && q.ends_with(") wiped")),
            "{quote:?}"
        );
        assert_eq!(text, "");
    }

    #[test]
    fn s_on_a_subagent_log_whose_row_has_no_description_quotes_it_as_a_bare_subagent() {
        let mut m = delegating();
        update(&mut m, key(KeyCode::Char('2')));
        update(&mut m, key(KeyCode::Char('j')));
        let (who, id) = m.subagent_selected().expect("a subagent");
        m.activities
            .as_mut()
            .and_then(|a| a.get_mut(&who))
            .and_then(|a| a.subagents.get_mut(&id))
            .expect("the subagent")
            .description = None;
        m.set_log(
            (who, id),
            Ok(Some(crate::log::fixtures::detail(vec![
                crate::log::fixtures::said("hello"),
            ]))),
        );
        update(&mut m, key(KeyCode::Char('0')));
        update(&mut m, key(KeyCode::Char('g')));
        update(&mut m, key(KeyCode::Char('j')));
        update(&mut m, key(KeyCode::Char('s')));
        assert_eq!(talk_of(&m), Some((Some("(subagent) hello"), "")));
    }

    fn type_text_and_send(m: &mut Model, text: &str) -> String {
        for c in text.chars() {
            update(m, key(KeyCode::Char(c)));
        }
        let effects = update(m, key(KeyCode::Enter));
        replies(&effects).pop().expect("a message went out")
    }

    #[test]
    fn the_quote_goes_before_the_message_and_only_with_one() {
        let (mut m, _) = reading_the_story();
        update(&mut m, key(KeyCode::Char('g')));
        update(&mut m, key(KeyCode::Char(']')));
        update(&mut m, key(KeyCode::Char('s')));
        assert!(update(&mut m, key(KeyCode::Enter)).is_empty());
        assert!(
            matches!(talk_of(&m), Some((Some(_), ""))),
            "enter with nothing typed keeps the box, quote and all"
        );
        assert_eq!(type_text_and_send(&mut m, "why?"), "> one\n\nwhy?");
    }

    #[test]
    fn backspace_on_empty_text_drops_the_quote_and_ctrl_u_keeps_it() {
        let (mut m, _) = reading_the_story();
        update(&mut m, key(KeyCode::Char('g')));
        update(&mut m, key(KeyCode::Char('s')));
        update(&mut m, key(KeyCode::Char('a')));
        update(&mut m, key_with(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(talk_of(&m), Some((Some("Fix the statusline."), "")));
        update(&mut m, key(KeyCode::Backspace));
        assert_eq!(talk_of(&m), Some((None, "")));
    }

    #[test]
    fn a_quote_goes_before_what_you_type_and_review_comments_after_and_is_not_sent_alone() {
        let (mut m, who) = reading_the_story();
        let line = crate::fixtures::line_info("a.rs", "+let x = 3;", 4);
        add_comment(&mut m, &who, &[line], "Use the limit.", None);
        update(&mut m, key(KeyCode::Char('g')));
        update(&mut m, key(KeyCode::Char(']')));
        update(&mut m, key(KeyCode::Char('s')));
        assert_eq!(talk_of(&m), Some((Some("one"), "")));
        assert_eq!(m.draft(&who).len(), 1, "opening the box keeps the draft");
        let sent = type_text_and_send(&mut m, "why?");
        assert!(sent.starts_with("Review comments."), "{sent}");
        assert!(
            sent.contains("> one\n\nwhy?\n\na.rs:4\n> +let x = 3;\nUse the limit."),
            "{sent}"
        );
        assert_eq!(m.drafted(), 0);
        delivered(&mut m, "payroll-fix-3");

        let line = crate::fixtures::line_info("a.rs", "+let y = 4;", 9);
        add_comment(&mut m, &who, &[line], "Why?", None);
        update(&mut m, key(KeyCode::Char('s')));
        let sent = replies(&update(&mut m, key(KeyCode::Enter))).remove(0);
        assert!(
            !sent.contains("> one"),
            "no text typed, no quote sent: {sent}"
        );
    }

    #[test]
    fn y_on_an_activity_with_no_log_still_copies_the_agents_message() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
        update(&mut m, key(KeyCode::Char('2')));
        m.set_activity(&name("auth-refresh-2"), Ok(None));
        update(&mut m, key(KeyCode::Char('0')));
        let effects = update(&mut m, key(KeyCode::Char('y')));
        assert!(
            matches!(effects.as_slice(), [Effect::Copy(text)] if text.contains("token refresh"))
        );
    }

    #[test]
    fn the_activity_counts_as_seen_after_a_few_steady_seconds_in_front_of_the_user() {
        let mut m = at_work();
        let who = name("payroll-fix-3");
        let base = m.clock;
        let moved = |secs| Moment {
            wall: base.wall + Duration::from_secs(secs),
            mono: base.mono + Duration::from_secs(secs),
        };
        let now = moved(0);
        tick_at(&mut m, now);
        assert_eq!(
            m.track(&who).activity_view.looked.seen,
            None,
            "not on screen"
        );

        update(&mut m, key(KeyCode::Char('2')));
        let start = moved(1);
        tick_at(&mut m, start);
        let almost = moved(3);
        tick_at(&mut m, almost);
        assert_eq!(m.track(&who).activity_view.looked.seen, None, "only 2s");
        let steady = moved(5);
        tick_at(&mut m, steady);
        assert_eq!(m.track(&who).activity_view.looked.seen, Some(steady.wall));

        update(&mut m, key(KeyCode::Esc));
        let later = moved(60);
        tick_at(&mut m, later);
        assert_eq!(
            m.track(&who).activity_view.looked.seen,
            Some(steady.wall),
            "frozen when it left the screen"
        );
        assert_eq!(m.track(&who).activity_view.looked.since, None);
    }

    #[test]
    fn reading_a_finished_activity_acknowledges_it_as_the_marker_clears() {
        let mut m = crate::fixtures::fleet(10);
        let who = name("auth-refresh-2");
        m.set_cursor(Cursor::Entry(who.clone()));
        update(&mut m, key(KeyCode::Char('2')));
        let base = m.clock;
        let acknowledged = |effects: Vec<Effect>| {
            effects
                .iter()
                .any(|e| matches!(e, Effect::Job(Job::Acknowledge(n)) if *n == who))
        };
        let at = |secs| Moment {
            wall: base.wall + Duration::from_secs(secs),
            mono: base.mono + Duration::from_secs(secs),
        };
        assert!(!acknowledged(tick_at(&mut m, at(0))));
        assert!(!acknowledged(tick_at(&mut m, at(2))), "not yet 3s");
        let effects = tick_at(&mut m, at(4));
        assert!(acknowledged(effects), "the same dwell time as the marker");
        assert_eq!(m.track(&who).activity_view.looked.seen, Some(at(4).wall));
        assert!(!acknowledged(tick_at(&mut m, at(6))), "once per report");
    }

    /// The session the pane waits on when `effects` asked for a look.
    fn checked_sessions(m: &Model, effects: &[Effect]) -> Vec<String> {
        let asked = effects
            .iter()
            .any(|e| matches!(e, Effect::CheckElsewhere(_)));
        match &m.pane {
            Pane::Checking { name, .. } if asked => vec![name.to_string()],
            _ => Vec::new(),
        }
    }

    #[test]
    fn a_peeked_terminal_is_what_is_seen_over_the_activity_behind_it_whatever_the_agents_order() {
        for _ in 0..8 {
            let mut m = crate::fixtures::fleet(10);
            m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
            update(&mut m, key(KeyCode::Char('2')));
            crate::fixtures::connect(&mut m, "obs-layout-4", Mode::Nav);
            m.peek = Some(Peek::Pane);
            let base = m.clock;
            let at = |secs| Moment {
                wall: base.wall + Duration::from_secs(secs),
                mono: base.mono + Duration::from_secs(secs),
            };
            tick_at(&mut m, at(0));
            tick_at(&mut m, at(5));
            let seen = |m: &mut Model, who: &str| m.track(&name(who)).activity_view.looked.seen;
            assert_eq!(seen(&mut m, "obs-layout-4"), Some(at(5).wall), "the pane");
            assert_eq!(seen(&mut m, "auth-refresh-2"), None, "covered by the peek");
        }
    }

    #[test]
    fn a_watched_float_is_what_is_seen_even_when_the_pane_is_on_another_agent() {
        let mut m = crate::fixtures::fleet(10);
        m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
        update(&mut m, key(KeyCode::Char('2')));
        crate::fixtures::connect(&mut m, "obs-layout-4", Mode::Nav);
        crate::fixtures::watch(&mut m, "auth-refresh-2");
        set_state(&mut m, "auth-refresh-2", done(), 1);
        let base = m.clock;
        let at = |secs| Moment {
            wall: base.wall + Duration::from_secs(secs),
            mono: base.mono + Duration::from_secs(secs),
        };
        tick_at(&mut m, at(0));
        let effects = tick_at(&mut m, at(5));
        let seen = |m: &mut Model, who: &str| m.track(&name(who)).activity_view.looked.seen;
        assert_eq!(
            seen(&mut m, "auth-refresh-2"),
            Some(at(5).wall),
            "the float"
        );
        assert_eq!(seen(&mut m, "obs-layout-4"), None, "the pane behind it");
        let acknowledged: Vec<String> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::Job(Job::Acknowledge(who)) => Some(who.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(acknowledged, ["auth-refresh-2"]);
    }

    #[test]
    fn nothing_is_seen_or_acknowledged_while_the_terminal_window_is_unfocused() {
        let mut m = crate::fixtures::fleet(10);
        let who = name("auth-refresh-2");
        m.set_cursor(Cursor::Entry(who.clone()));
        update(&mut m, key(KeyCode::Char('2')));
        m.terminal = TerminalFocus::Unfocused;
        let base = m.clock;
        let at = |secs| Moment {
            wall: base.wall + Duration::from_secs(secs),
            mono: base.mono + Duration::from_secs(secs),
        };
        tick_at(&mut m, at(0));
        let effects = tick_at(&mut m, at(10));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Job(Job::Acknowledge(_)))),
            "an activity on an unattended screen isn't read"
        );
        assert_eq!(m.track(&who).activity_view.looked.seen, None);
        m.terminal = TerminalFocus::Focused;
        let effects = tick_at(&mut m, at(11));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Job(Job::Acknowledge(_)))),
            "time away doesn't count toward the dwell"
        );
        let effects = tick_at(&mut m, at(30));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Job(Job::Acknowledge(_)))),
            "it counts once the window has been back for the dwell"
        );
    }

    mod links {
        use super::*;
        use crate::document::Hover;
        use crate::fixtures::{facts, modified};
        use crate::log::fixtures::{activity_of, bash, delegate, done, edit, said};
        use crate::model::{ActivitySpot, Opened};
        use crate::touched::Baseline;
        use crate::tree::ChangesScope;
        use agent_core::ports::{CallState, LogEntry, Progress};

        const WHO: &str = "obs-layout-4";
        const ROOT: &str = "/w/heucristic0/obs-layout-4";

        fn ok() -> CallState {
            done(agent_core::ports::Summary::NoOutput)
        }

        fn who() -> SessionName {
            name(WHO)
        }

        /// [0] focused on the activity of the delegating agent, whose log is `log`.
        fn reading(log: Vec<LogEntry>) -> Model {
            let mut m = crate::fixtures::fleet(10);
            m.set_cursor(Cursor::Entry(who()));
            m.set_activity(&who(), Ok(Some(activity_of(log))));
            update(&mut m, key(KeyCode::Char('2')));
            update(&mut m, key(KeyCode::Char('0')));
            m
        }

        fn on_entry(m: &mut Model, at: EntryKey) {
            m.track(&who()).activity_view.watch.nav.cursor = Some(at);
            m.settle();
        }

        fn cursor(m: &Model) -> Option<EntryKey> {
            m.tracked(&who())
                .and_then(|t| t.activity_view.watch.nav.cursor.clone())
        }

        fn said_by_the_status_bar(m: &Model) -> String {
            m.status
                .as_ref()
                .map(|s| s.text.clone())
                .unwrap_or_default()
        }

        /// Entries: the prompt, then said, the Agent call, the edit and a
        /// command, at log indexes 0 to 3.
        fn delegating_log(agent: Option<&str>) -> Vec<LogEntry> {
            vec![
                said("go"),
                delegate("QA pass", agent),
                edit(&format!("{ROOT}/src/a.rs")),
                bash("ls", ok()),
            ]
        }

        const AGENT_CALL: EntryKey = EntryKey::Log(1);
        const EDIT_CALL: EntryKey = EntryKey::Log(2);

        fn spot_of(m: &Model) -> Option<ActivitySpot> {
            m.activity_spot()
        }

        #[test]
        fn enter_on_an_agent_call_opens_its_subagents_log_and_escape_returns_to_the_activity_spot()
        {
            let mut m = reading(delegating_log(Some("s1")));
            let spot = spot_of(&m).expect("on the activity");
            on_entry(&mut m, AGENT_CALL);
            update(&mut m, key(KeyCode::Enter));
            assert_eq!(
                m.subagent_selected(),
                Some((who(), AgentId::new("s1"))),
                "[2]'s cursor is on the subagent the call started"
            );
            assert!(matches!(m.hover(), Some(Hover::Subagent(..))));
            assert_eq!(m.panel.back(), Some(&spot));

            update(&mut m, key(KeyCode::Esc));
            assert!(matches!(m.hover(), Some(Hover::Activity(_))));
            assert_eq!(spot_of(&m), Some(spot), "the same panel, offset and row");
            assert_eq!(m.panel.back(), None);
            assert_eq!(cursor(&m), Some(AGENT_CALL), "the cursor is where it was");
            update(&mut m, key(KeyCode::Esc));
            assert!(matches!(m.panel, Panel::Agent { .. }), "then to [2]");
        }

        #[test]
        fn enter_on_an_agent_call_with_no_known_subagent_folds_it_like_any_entry() {
            let mut m = reading(delegating_log(None));
            on_entry(&mut m, AGENT_CALL);
            update(&mut m, key(KeyCode::Enter));
            assert!(
                m.tracked(&who())
                    .is_some_and(|t| t.activity_view.watch.nav.is_open(&AGENT_CALL))
            );
            assert!(matches!(m.hover(), Some(Hover::Activity(_))));
        }

        #[test]
        fn enter_on_a_call_whose_subagent_is_no_longer_listed_says_so_and_stays() {
            let mut m = reading(delegating_log(Some("gone")));
            on_entry(&mut m, AGENT_CALL);
            update(&mut m, key(KeyCode::Enter));
            assert!(said_by_the_status_bar(&m).contains("isn't listed any more"));
            assert!(matches!(m.hover(), Some(Hover::Activity(_))));
            assert_eq!(m.panel.back(), None);
        }

        #[test]
        fn enter_on_an_edit_call_shows_the_file_in_the_tab_that_lists_it_and_escape_restores_the_tab()
         {
            let mut m = reading(delegating_log(None));
            m.set_test_facts(&who(), Ok(facts(Vec::new(), vec![modified("src/a.rs")])));
            let spot = spot_of(&m).expect("on the activity");
            assert_eq!(spot.scope, ChangesScope::Uncommitted);
            on_entry(&mut m, EDIT_CALL);
            update(&mut m, key(KeyCode::Enter));
            assert_eq!(m.changes_scope, ChangesScope::Since);
            assert_eq!(
                m.file_under_cursor().map(|(_, file)| file),
                Some(PathBuf::from("src/a.rs")),
                "the absolute path the call named is the file in the checkout"
            );
            assert!(matches!(m.hover(), Some(Hover::Diff(_))));

            update(&mut m, key(KeyCode::Esc));
            assert!(matches!(m.hover(), Some(Hover::Activity(_))));
            assert_eq!(spot_of(&m), Some(spot));
            assert_eq!(
                m.changes_scope,
                ChangesScope::Uncommitted,
                "the tab [3] had"
            );
            assert_eq!(cursor(&m), Some(EDIT_CALL));
        }

        #[test]
        fn a_file_in_a_closed_folder_is_shown_with_its_folders_opened() {
            let mut m = reading(delegating_log(None));
            m.changes_view = ChangesView::Tree;
            m.set_test_facts(&who(), Ok(facts(vec![modified("src/a.rs")], Vec::new())));
            m.track(&who()).collapsed.insert(PathBuf::from("src"));
            on_entry(&mut m, EDIT_CALL);
            update(&mut m, key(KeyCode::Enter));
            assert_eq!(
                m.file_under_cursor().map(|(_, file)| file),
                Some(PathBuf::from("src/a.rs"))
            );
            assert!(m.tracked(&who()).is_some_and(|t| t.collapsed.is_empty()));
        }

        #[test]
        fn a_touched_file_git_does_not_list_says_so_and_leaves_the_activity_where_it_was() {
            let mut m = reading(delegating_log(None));
            m.set_test_facts(&who(), Ok(facts(Vec::new(), Vec::new())));
            on_entry(&mut m, EDIT_CALL);
            update(&mut m, key(KeyCode::Enter));
            assert!(said_by_the_status_bar(&m).contains("src/a.rs has no diff in [3]"));
            assert!(matches!(m.hover(), Some(Hover::Activity(_))));
            assert_eq!(m.panel.back(), None);
        }

        #[test]
        fn a_file_when_there_is_no_room_for_changes_says_so() {
            let mut m = reading(delegating_log(None));
            m.set_test_facts(&who(), Ok(facts(vec![modified("src/a.rs")], Vec::new())));
            m.work_room = crate::model::WorkRoom::TooShort;
            on_entry(&mut m, EDIT_CALL);
            update(&mut m, key(KeyCode::Enter));
            assert_eq!(said_by_the_status_bar(&m), "no room for [3] at this height");
            assert!(matches!(m.hover(), Some(Hover::Activity(_))));
        }

        #[test]
        fn an_activity_still_delegating_counts_its_touched_files_but_lists_none() {
            let m = reading(delegating_log(None));
            assert_eq!(m.touched(&who()).map(<[_]>::len), Some(1));
            assert_eq!(m.listed(&who()), Some(&[][..]));
        }

        fn settled() -> (Model, SessionName) {
            let mut m = crate::fixtures::fleet(10);
            let agent = name("auth-refresh-2");
            m.set_cursor(Cursor::Entry(agent.clone()));
            let mut detail = activity_of(vec![
                said("go"),
                edit("/w/heucristic0/auth-refresh-2/src/b.rs"),
                bash("ls", ok()),
            ]);
            detail.progress = Progress::Finished { took: None };
            m.set_activity(&agent, Ok(Some(detail)));
            m.set_test_facts(&agent, Ok(facts(vec![modified("src/b.rs")], Vec::new())));
            update(&mut m, key(KeyCode::Char('2')));
            update(&mut m, key(KeyCode::Char('0')));
            (m, agent)
        }

        #[test]
        fn a_settled_activity_lists_its_touched_files_and_enter_on_one_opens_its_diff() {
            let (mut m, agent) = settled();
            assert_eq!(m.listed(&agent).map(<[_]>::len), Some(1));
            m.track(&agent).activity_view.watch.nav.cursor =
                Some(EntryKey::Touched("src/b.rs".into()));
            m.settle();
            update(&mut m, key(KeyCode::Enter));
            assert!(
                matches!(m.hover(), Some(Hover::Diff(key)) if key.change.path == Path::new("src/b.rs"))
            );
        }

        #[test]
        fn e_on_the_activity_writes_it_as_markdown_at_the_entry_under_the_cursor() {
            let mut m = reading(delegating_log(None));
            on_entry(&mut m, EDIT_CALL);
            let effects = update(&mut m, key(KeyCode::Char('e')));
            let [
                Effect::Edit(Open::Transcript {
                    text, line, name, ..
                }),
            ] = effects.as_slice()
            else {
                panic!("one transcript to open")
            };
            assert_eq!(name, "obs-layout-4-activity");
            assert_eq!(
                text.lines().nth(line.get() - 1),
                Some(format!("### Edit {ROOT}/src/a.rs").as_str())
            );
            assert!(text.contains("### Prompt"), "{text}");
        }

        #[test]
        fn the_markdown_of_an_activity_has_the_closing_message_and_touched_files_before_the_calls()
        {
            let (mut m, agent) = settled();
            m.track(&agent).activity_view.watch.nav.cursor =
                Some(EntryKey::Touched("src/b.rs".into()));
            let effects = update(&mut m, key(KeyCode::Char('e')));
            let [Effect::Edit(Open::Transcript { text, line, .. })] = effects.as_slice() else {
                panic!("one transcript to open")
            };
            assert_eq!(
                text.lines().nth(line.get() - 1),
                Some("- `src/b.rs` (+2 \u{2212}1)"),
                "{text}"
            );
            let at = |needle: &str| {
                text.find(needle)
                    .unwrap_or_else(|| panic!("{needle}\n{text}"))
            };
            assert!(at("### Prompt") < at("### Closing message"));
            assert!(at("### Closing message") < at("### Touched files"));
            assert!(at("### Touched files") < at("## Calls"));
        }

        /// The agent's next report, with `prompts` prompts taken in all.
        fn prompted(m: &mut Model, agent: &SessionName, state: State, prompts: u64) {
            let mut next = m.activities.clone().expect("reports");
            let activity = next.get_mut(agent).expect("reports");
            activity.state = state;
            if prompts > activity.prompts {
                activity.prompt_event = activity.events + 1;
            }
            activity.prompts = prompts;
            activity.events += 1;
            update(m, Msg::Activities(next));
        }

        fn working() -> State {
            State::Working { tools: Vec::new() }
        }

        fn finished() -> State {
            State::Done { summary: None }
        }

        /// Facts as read when the agent had reported `events`.
        fn asked_at(
            m: &mut Model,
            agent: &SessionName,
            events: u64,
            pending: Vec<agent_core::ports::Change>,
        ) {
            let read = Inspected {
                asked_events: Some(events),
                ..Inspected::of(facts(pending, Vec::new()))
            };
            m.set_facts(agent, Ok(read));
        }

        /// The agent reading its activity, which has begun with `old.rs` dirty.
        fn begun() -> (Model, SessionName) {
            let mut m = crate::fixtures::fleet(10);
            let agent = name("auth-refresh-2");
            let events = m.activity(&agent).expect("reports").events;
            asked_at(&mut m, &agent, events, vec![modified("old.rs")]);
            prompted(&mut m, &agent, working(), 2);
            m.set_cursor(Cursor::Entry(agent.clone()));
            m.set_activity(&agent, Ok(Some(activity_of(Vec::new()))));
            update(&mut m, key(KeyCode::Char('2')));
            (m, agent)
        }

        fn paths(files: Option<&[crate::touched::TouchedFile]>) -> Vec<&str> {
            files
                .unwrap_or_default()
                .iter()
                .filter_map(|f| f.path.to_str())
                .collect()
        }

        #[test]
        fn a_prompt_snapshots_the_changed_files_and_only_later_changes_count() {
            let (mut m, agent) = begun();
            let now = facts(vec![modified("old.rs"), modified("sed.rs")], Vec::new());
            m.set_test_facts(&agent, Ok(now));
            m.settle();
            assert_eq!(
                paths(m.touched(&agent)),
                ["sed.rs"],
                "old.rs was already dirty"
            );
            assert_eq!(
                m.listed(&agent),
                Some(&[][..]),
                "a working activity only counts"
            );
            prompted(&mut m, &agent, finished(), 2);
            m.settle();
            assert_eq!(paths(m.listed(&agent)), ["sed.rs"]);
        }

        #[test]
        fn the_same_words_typed_again_begin_a_new_activity_and_a_fresh_snapshot() {
            let (mut m, agent) = begun();
            prompted(&mut m, &agent, finished(), 2);
            let events = m.activity(&agent).expect("reports").events;
            let both = vec![modified("old.rs"), modified("sed.rs")];
            asked_at(&mut m, &agent, events, both.clone());
            prompted(&mut m, &agent, working(), 3);
            asked_at(&mut m, &agent, events + 1, both);
            m.settle();
            assert!(
                paths(m.touched(&agent)).is_empty(),
                "sed.rs belongs to the activity before"
            );
        }

        #[test]
        fn waking_after_rest_or_a_handback_keeps_the_snapshot_of_the_activity() {
            let (mut m, agent) = begun();
            let snapshot = m.track(&agent).baseline.clone();
            m.set_test_facts(&agent, Ok(facts(Vec::new(), Vec::new())));
            prompted(&mut m, &agent, State::Delegating, 2);
            prompted(&mut m, &agent, working(), 2);
            prompted(&mut m, &agent, finished(), 2);
            prompted(&mut m, &agent, working(), 2);
            assert_eq!(m.track(&agent).baseline, snapshot);
            assert_ne!(snapshot, Baseline::Unknown);
        }

        #[test]
        fn facts_asked_before_the_agents_last_report_or_after_the_prompt_are_not_a_snapshot() {
            let agent = name("auth-refresh-2");
            for (asked, why) in [(0, "before its last report"), (9, "after the prompt")] {
                let mut m = crate::fixtures::fleet(10);
                asked_at(&mut m, &agent, asked, vec![modified("old.rs")]);
                prompted(&mut m, &agent, working(), 2);
                assert_eq!(m.track(&agent).baseline, Baseline::Unknown, "{why}");
            }
            let mut m = crate::fixtures::fleet(10);
            m.set_test_facts(&agent, Ok(facts(vec![modified("old.rs")], Vec::new())));
            prompted(&mut m, &agent, working(), 2);
            assert_eq!(
                m.track(&agent).baseline,
                Baseline::Unknown,
                "facts that don't say when they were asked"
            );
        }

        #[test]
        fn edits_reported_in_the_same_poll_as_the_prompt_are_not_in_the_snapshot() {
            let agent = name("auth-refresh-2");
            let reported = m_events();
            for (asked_at_event, taken) in [(reported + 3, false), (reported, true)] {
                let mut m = crate::fixtures::fleet(10);
                let mut next = m.activities.clone().expect("reports");
                let activity = next.get_mut(&agent).expect("reports");
                activity.state = working();
                activity.prompts += 1;
                activity.prompt_event = reported + 1;
                activity.events = reported + 4;
                asked_at(&mut m, &agent, asked_at_event, vec![modified("old.rs")]);
                update(&mut m, Msg::Activities(next));
                assert_eq!(
                    m.track(&agent).baseline != Baseline::Unknown,
                    taken,
                    "a read asked at event {asked_at_event} of a prompt at {} and edits to {}",
                    reported + 1,
                    reported + 4
                );
            }
        }

        /// The events the fleet's agent had reported before the test's prompt.
        fn m_events() -> u64 {
            let m = crate::fixtures::fleet(10);
            m.activity(&name("auth-refresh-2")).expect("reports").events
        }

        #[test]
        fn an_agent_first_seen_mid_activity_has_no_snapshot_and_one_that_appears_later_does() {
            let mut m = crate::fixtures::fleet(10);
            let all = m.activities.take().expect("reports");
            update(&mut m, Msg::Activities(all.clone()));
            assert_eq!(m.track(&who()).baseline, Baseline::Unknown);

            let newcomer = name("auth-refresh-2");
            let mut without = all.clone();
            without.remove(&newcomer);
            update(&mut m, Msg::Activities(without));
            let events = all[&newcomer].events;
            asked_at(&mut m, &newcomer, events, vec![modified("old.rs")]);
            update(&mut m, Msg::Activities(all));
            assert_ne!(m.track(&newcomer).baseline, Baseline::Unknown);
        }

        #[test]
        fn escape_after_moving_on_to_another_agents_document_does_not_jump_to_the_old_activity() {
            let mut m = reading(delegating_log(None));
            m.set_test_facts(&who(), Ok(facts(vec![modified("src/a.rs")], Vec::new())));
            on_entry(&mut m, EDIT_CALL);
            update(&mut m, key(KeyCode::Enter));
            assert!(m.panel.back().is_some());

            update(&mut m, key(KeyCode::Char('l')));
            let other = name("auth-refresh-2");
            m.set_cursor(Cursor::Entry(other.clone()));
            update(&mut m, key(KeyCode::Char('2')));
            update(&mut m, key(KeyCode::Char('0')));
            assert!(m.panel.in_main());
            update(&mut m, key(KeyCode::Esc));
            assert!(
                matches!(m.panel, Panel::Agent { .. }),
                "back to [2], not to the first agent's activity: {:?}",
                m.panel
            );
        }

        #[test]
        fn selecting_another_agent_forgets_the_way_back_before_any_key() {
            let mut m = reading(delegating_log(None));
            m.set_test_facts(&who(), Ok(facts(vec![modified("src/a.rs")], Vec::new())));
            on_entry(&mut m, EDIT_CALL);
            update(&mut m, key(KeyCode::Enter));
            assert!(m.panel.back().is_some());
            m.set_cursor(Cursor::Entry(name("auth-refresh-2")));
            m.settle();
            assert_eq!(m.panel.back(), None);
        }

        #[test]
        fn escape_closes_an_open_commit_before_it_returns_to_the_activity() {
            let mut m = reading(delegating_log(None));
            m.set_test_facts(&who(), Ok(facts(vec![modified("src/a.rs")], Vec::new())));
            on_entry(&mut m, EDIT_CALL);
            update(&mut m, key(KeyCode::Enter));
            m.opened = Some(Opened::Commit {
                sha: "abc".into(),
                subject: "s".into(),
            });
            update(&mut m, key(KeyCode::Esc));
            assert!(m.opened.is_none());
            assert!(m.panel.back().is_some(), "still where the jump put it");
            update(&mut m, key(KeyCode::Esc));
            assert!(matches!(m.hover(), Some(Hover::Activity(_))));
        }

        #[test]
        fn a_checkout_reached_through_a_symlink_is_one_file_in_the_activity() {
            let mut m = reading(vec![
                said("go"),
                edit("/real/obs-layout-4/src/a.rs"),
                edit(&format!("{ROOT}/src/a.rs")),
            ]);
            let read = Inspected {
                real_root: Some("/real/obs-layout-4".into()),
                ..Inspected::of(facts(vec![modified("src/a.rs")], Vec::new()))
            };
            m.set_facts(&who(), Ok(read));
            m.settle();
            assert_eq!(paths(m.touched(&who())), ["src/a.rs"]);
        }

        #[test]
        fn the_files_of_another_agents_activity_are_not_this_ones() {
            let m = reading(delegating_log(None));
            assert_eq!(m.touched(&name("auth-refresh-2")), None);
            assert_eq!(m.listed(&name("auth-refresh-2")), None);
        }
    }
    mod command_output;
    mod elsewhere;
    mod restart;
    mod send_edit;
}
