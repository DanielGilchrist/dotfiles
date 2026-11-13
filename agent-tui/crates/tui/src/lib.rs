mod cause;
mod document;
mod keymap;
mod keyspec;
mod log;
mod model;
mod output;
mod palette;
mod review;
mod touched;
mod track;
mod tree;
mod typed;
mod update;
mod view;
mod worker;

#[cfg(test)]
mod fixtures;

use std::num::NonZeroU16;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use agent_core::config::Config;
use agent_core::names::{CarriageReturn, SessionName, clean};
use agent_core::ops::Backend;
use agent_core::ports::{ClientId, HarnessId, Observation};
use agent_core::process::LaunchSpec;
use agent_term::{Emulator, EmulatorFactory, Liveness, TermSize, Terminal};
use anyhow::{Context, Result};
use cause::Cause;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use keyspec::{Keyboard, Sendable};
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;

use model::{Connection, Mode, Model, Moment, Pane, StatusKind, TerminalFocus, Ticket, Watch};
use palette::{Colours, Palette};
use update::{Background, Effect, Open};
use worker::{Job, Msg, Workers};

const FRAME: Duration = Duration::from_millis(16);
const REFRESH_EVERY: Duration = Duration::from_secs(3);
const INPUT_POLL: Duration = Duration::from_millis(50);
/// Where actions are configured, for messages.
pub const CONFIG_HINT: &str = "~/.config/agent/config.toml";
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub struct Options {
    pub backend: Arc<Backend>,
    pub config: Config,
    pub harness: HarnessId,
    pub start_dir: PathBuf,
    pub home: Option<PathBuf>,
    pub emulator: EmulatorFactory,
}

fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(BASE64[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Most bytes of text sent to the clipboard, which terminals cap.
const COPY_MAX: usize = 70_000;

/// `OSC 52`: sets the clipboard through the terminal.
fn copy_to_clipboard(terminal: &mut DefaultTerminal, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut end = text.len().min(COPY_MAX);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let backend = terminal.backend_mut();
    write!(backend, "\x1b]52;c;{}\x07", base64(&text.as_bytes()[..end]))?;
    backend.flush()
}

/// `OSC 1337 SetUserVar`, which wezterm and iTerm2 hand to their config.
fn set_user_var(terminal: &mut DefaultTerminal, name: &str, value: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let backend = terminal.backend_mut();
    write!(backend, "\x1b]1337;SetUserVar={name}={}\x07", base64(value))?;
    backend.flush()
}

/// Makes the next draw repaint everything, after something else used the
/// screen. Re-entering the alternate screen has already cleared it.
/// `Terminal::clear` would ask the terminal for the cursor position first,
/// and stall if the answer never comes.
fn forget_screen(terminal: &mut DefaultTerminal) {
    terminal.swap_buffers();
    terminal.swap_buffers();
}

/// How `$EDITOR` exited: `:cq` and friends cancel.
enum Edited {
    Saved,
    Cancelled,
}

/// Whether the event loop goes on after applying effects.
enum Flow {
    Continue,
    Quit,
}

/// `OSC 2`, the terminal's window or tab title.
fn set_title(terminal: &mut DefaultTerminal, title: &str) {
    use std::io::Write;
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    let _ = write!(terminal.backend_mut(), "\x1b]2;{clean}\x07")
        .and_then(|()| terminal.backend_mut().flush());
}

enum InputCtl {
    /// Stop reading the tty and acknowledge once parked.
    Pause(SyncSender<()>),
    Resume,
}

/// Asks the terminal once. It needs raw mode, and it is asked before
/// the input thread takes the tty.
fn detect_keyboard() -> Result<Keyboard> {
    enable_raw_mode()?;
    Ok(if supports_keyboard_enhancement().unwrap_or(false) {
        Keyboard::Disambiguated
    } else {
        Keyboard::Legacy
    })
}

fn enter_screen(keys: Keyboard) -> Result<()> {
    enable_raw_mode()?;
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        EnableFocusChange
    )?;
    if matches!(keys, Keyboard::Disambiguated) {
        execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        )?;
    }
    Ok(())
}

/// The terminal keeps a keyboard flag stack for each screen, so the flags
/// pushed on the alternate screen are popped before leaving it.
fn leave_screen(keys: Keyboard) {
    if matches!(keys, Keyboard::Disambiguated) {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        std::io::stdout(),
        DisableFocusChange,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}

/// Restores the terminal however the TUI exits, including by panic.
struct ScreenGuard {
    keys: Keyboard,
}

impl ScreenGuard {
    fn enter() -> Result<Self> {
        let keys = detect_keyboard()?;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            leave_screen(keys);
            previous(info);
        }));
        let guard = Self { keys };
        enter_screen(keys)?;
        Ok(guard)
    }
}

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        leave_screen(self.keys);
    }
}

/// Forwards terminal events. While paused it doesn't touch the tty at all,
/// so an editor gets every keystroke.
fn spawn_input(msgs: Sender<Msg>, ctl: Receiver<InputCtl>) {
    thread::spawn(move || {
        loop {
            match ctl.try_recv() {
                Ok(InputCtl::Pause(ack)) => {
                    let _ = ack.send(());
                    loop {
                        match ctl.recv() {
                            Ok(InputCtl::Resume) => break,
                            Ok(InputCtl::Pause(ack)) => {
                                let _ = ack.send(());
                            }
                            Err(_) => return,
                        }
                    }
                }
                Ok(InputCtl::Resume) | Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => return,
            }
            match event::poll(INPUT_POLL) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(_) => return,
            }
            let msg = match event::read() {
                Ok(Event::Key(key)) => Msg::Key(key),
                Ok(Event::Mouse(mouse)) => Msg::Mouse(mouse),
                Ok(Event::Paste(text)) => Msg::Paste(text),
                Ok(Event::Resize(..)) => Msg::Resize,
                Ok(Event::FocusGained) => Msg::Focus(TerminalFocus::Focused),
                Ok(Event::FocusLost) => Msg::Focus(TerminalFocus::Unfocused),
                Err(_) => return,
            };
            if msgs.send(msg).is_err() {
                return;
            }
        }
    });
}

/// A watcher's terminal is never bigger than this, whatever size the session host
/// reports, so a garbage display area can't ask the kernel and the emulator
/// for a huge grid. The float crops anything bigger than the screen anyway.
const OBSERVED_MAX: TermSize = TermSize {
    cols: NonZeroU16::new(500).unwrap(),
    rows: NonZeroU16::new(200).unwrap(),
};

struct Runtime {
    backend: Arc<Backend>,
    emulator: EmulatorFactory,
    workers: Workers,
    input: Sender<InputCtl>,
    keys: Keyboard,
}

impl Runtime {
    fn connect(
        &self,
        m: &mut Model,
        name: &SessionName,
        spec: LaunchSpec,
        ticket: Ticket,
    ) -> Result<()> {
        if m.pane.awaiting(ticket).is_none() {
            return Ok(());
        }
        let size = TermSize::of(m.areas.terminal).context("the terminal pane has no size")?;
        let term = self
            .start_terminal(spec, size)
            .with_context(|| format!("attaching to {name}"))?;
        let arrival = m.arrival(ticket);
        m.set_pane(Pane::Connected(Connection {
            name: name.clone(),
            id: ClientId::new(term.pid()),
            term,
            mode: Mode::Nav,
            opened: m.clock.mono,
        }));
        m.touch_mru(name);
        for work in update::connected(m, arrival) {
            self.dispatch(work)?;
        }
        Ok(())
    }

    /// Starts, or restarts, the float's read-only view at the size the
    /// session host reports for it.
    fn observe(&self, m: &mut Model, observation: Observation, ticket: Ticket) -> Result<()> {
        let now = m.clock.mono;
        let Some(watch) = m.watch_mut().filter(|w| w.awaiting(ticket).is_some()) else {
            return Ok(());
        };
        let seen = (observation.cols, observation.rows);
        let size = TermSize {
            cols: observation.cols.min(OBSERVED_MAX.cols),
            rows: observation.rows.min(OBSERVED_MAX.rows),
        };
        let term = self.start_terminal(observation.spec, size)?;
        watch.install(term, seen, now);
        Ok(())
    }

    fn start_terminal(
        &self,
        spec: LaunchSpec,
        size: TermSize,
    ) -> std::io::Result<Terminal<Box<dyn Emulator>>> {
        let spec = spec.env_remove(self.backend.host.nesting_env().iter().copied());
        Terminal::spawn(&spec, size, (self.emulator)(size))
    }

    /// Sends what a connection starts in the background.
    fn dispatch(&self, work: Background) -> Result<()> {
        match work {
            Background::Job(job) => self.send(job),
            Background::Look(look) => {
                self.workers.look_elsewhere(look);
                Ok(())
            }
        }
    }

    fn restart(&self, restarts: Vec<agent_core::ops::Restart>) -> Result<()> {
        for go in restarts {
            self.workers
                .restarts
                .send(go)
                .ok()
                .context("the restart worker stopped")?;
        }
        Ok(())
    }

    fn send(&self, job: Job) -> Result<()> {
        self.workers
            .jobs
            .send(job)
            .ok()
            .context("the background worker stopped")
    }

    /// Runs `$EDITOR` with `arg` in `cwd`, handing it the terminal. Reports
    /// whether it exited successfully (`:cq` doesn't).
    fn edit(
        &self,
        terminal: &mut DefaultTerminal,
        cwd: &Path,
        arg: Option<&Path>,
        line: Option<std::num::NonZeroUsize>,
    ) -> Result<Edited> {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
        let argv =
            shell_words::split(&editor).with_context(|| format!("parsing $EDITOR ({editor})"))?;
        let (program, args) = argv.split_first().context("$EDITOR is empty")?;

        let mut command = std::process::Command::new(program);
        command
            .args(args)
            .args(line.map(|l| format!("+{l}")))
            .args(arg)
            .current_dir(cwd);
        let status = self
            .suspended(terminal, &mut command)
            .with_context(|| format!("running {program}"))?;
        Ok(if status.success() {
            Edited::Saved
        } else {
            Edited::Cancelled
        })
    }

    /// Hands the terminal to `command` until it exits.
    fn suspended(
        &self,
        terminal: &mut DefaultTerminal,
        command: &mut std::process::Command,
    ) -> Result<std::process::ExitStatus> {
        let (ack, parked) = mpsc::sync_channel(1);
        self.input
            .send(InputCtl::Pause(ack))
            .context("input thread is gone")?;
        parked.recv().context("input thread is gone")?;
        leave_screen(self.keys);
        let status = command.status();
        let resumed = enter_screen(self.keys);
        let _ = self.input.send(InputCtl::Resume);
        forget_screen(terminal);
        resumed?;
        Ok(status?)
    }

    /// The session host sizes a session to its smallest client, so while the editor
    /// has the screen, the hidden pane's client takes the whole terminal and
    /// can't crop the session for clients the editor attaches. The event
    /// loop sizes it back to the pane on the next frame.
    fn fill_screen(m: &mut Model, terminal: &DefaultTerminal) {
        let full = terminal
            .size()
            .ok()
            .and_then(|s| TermSize::of(Rect::new(0, 0, s.width, s.height)));
        if let (Pane::Connected(c), Some(full)) = (&mut m.pane, full) {
            let _ = c.term.resize(full);
        }
    }

    fn apply(
        &self,
        m: &mut Model,
        terminal: &mut DefaultTerminal,
        effects: Vec<Effect>,
    ) -> Result<Flow> {
        for effect in effects {
            if let Some((kind, why)) = refusal(&effect) {
                match kind {
                    StatusKind::Info => m.info(why),
                    StatusKind::Error => m.error(why),
                    StatusKind::Warning => m.warn(why),
                }
                continue;
            }
            match effect {
                Effect::Quit => return Ok(Flow::Quit),
                Effect::Job(job) => self.send(job)?,
                Effect::Restart(restarts) => self.restart(restarts)?,
                Effect::CheckElsewhere(look) => self.workers.look_elsewhere(look),
                Effect::Inspect {
                    name,
                    worktree,
                    events,
                } => self.workers.inspect(name, worktree, events),
                Effect::ReadDocument {
                    key,
                    worktree,
                    reuse,
                } => self.workers.read_document(key, worktree, reuse),
                Effect::ReadSubagent { name, id, workdir } => {
                    self.workers.read_subagent(name, id, workdir);
                }
                Effect::ReadActivity { name, workdir, at } => {
                    self.workers.read_activity(name, workdir, at);
                }
                Effect::ReadCommandOutput(request) => self.workers.read_command_output(request),
                Effect::ProbeTranscript {
                    name,
                    at,
                    standing,
                    seen,
                } => self.workers.probe(name, at, standing, seen),
                Effect::SuggestName {
                    prompt,
                    harness,
                    edit,
                } => self.workers.suggest_name(prompt, harness, edit),
                Effect::Attach { name, ticket } => {
                    let spec = self.backend.host.attach(&name);
                    let result = self.connect(m, &name, spec, ticket);
                    Self::connect_failed(m, ticket, result);
                }
                Effect::Observe {
                    observation,
                    ticket,
                } => {
                    if let Err(e) = self.observe(m, observation, ticket) {
                        update::observe_failed(m, ticket, Cause::of(e.as_ref()));
                    }
                }
                Effect::Connect { launch, ticket } => {
                    let result = self.connect(m, &launch.session, launch.spec, ticket);
                    Self::connect_failed(m, ticket, result);
                }
                Effect::Edit(open) => self.open(m, terminal, &open),
                Effect::Interactive { label, launch } => {
                    self.run_interactive(m, terminal, &label, &launch);
                }
                Effect::Copy(text) => {
                    if let Err(e) = copy_to_clipboard(terminal, &text) {
                        m.error(format!("copying: {e}"));
                    }
                }
                Effect::Pty { to, bytes } => {
                    if let Some(c) = m.pane.connection().filter(|c| c.name == to) {
                        c.term.send(bytes);
                    }
                }
                Effect::Bell => {
                    use std::io::Write;
                    let _ = terminal
                        .backend_mut()
                        .write_all(b"\x07")
                        .and_then(|()| terminal.backend_mut().flush());
                }
                Effect::TerminalEvent { name, value } => {
                    if let Err(e) = set_user_var(terminal, name.as_str(), value.as_bytes()) {
                        m.error(format!("sending {}: {e}", name.as_str()));
                    }
                }
                Effect::EditTask(task) => self.edit_form_task(m, terminal, &task),
                Effect::EditMessage { to, text } => self.edit_message(m, terminal, &to, &text),
            }
        }
        Ok(Flow::Continue)
    }

    fn open(&self, m: &mut Model, terminal: &mut DefaultTerminal, open: &Open) {
        Self::fill_screen(m, terminal);
        let mut line = None;
        let mut transcript = None;
        let (cwd, file) = match open {
            Open::Checkout(dir) => (dir, None),
            Open::File { root, file } => (root, Some(file.as_path())),
            Open::At {
                root,
                file,
                line: at,
            } => {
                line = Some(*at);
                (root, Some(file.as_path()))
            }
            Open::Transcript {
                root,
                name,
                text,
                line: at,
            } => match write_transcript(name, text) {
                Ok(path) => {
                    line = Some(*at);
                    (root, transcript.insert(path).as_path().into())
                }
                Err(e) => {
                    m.error(format!("{e:#}"));
                    return;
                }
            },
        };
        let edited = self.edit(terminal, cwd, file, line).map(drop);
        m.clock = Moment::now();
        if let Err(e) = edited {
            m.error(format!("{e:#}"));
        }
        m.schedule_preview();
    }

    fn run_interactive(
        &self,
        m: &mut Model,
        terminal: &mut DefaultTerminal,
        label: &str,
        launch: &agent_core::actions::CommandLine,
    ) {
        Self::fill_screen(m, terminal);
        let mut command = std::process::Command::new(&launch.program);
        command.args(&launch.args);
        if let Some(cwd) = &launch.cwd {
            command.current_dir(cwd);
        }
        let ran = self.suspended(terminal, &mut command);
        m.clock = Moment::now();
        match ran {
            Ok(status) if status.success() => {}
            Ok(status) => m.error(format!("{label} exited with {status}")),
            Err(e) => m.error(format!("{label}: {e:#}")),
        }
        m.schedule_preview();
    }

    fn edit_form_task(&self, m: &mut Model, terminal: &mut DefaultTerminal, task: &str) {
        Self::fill_screen(m, terminal);
        let edited = self.edit_task(terminal, task);
        m.clock = Moment::now();
        match edited {
            Ok(Some(task)) => {
                let now = m.clock.mono;
                let limit = m.name_limit;
                if let model::Overlay::New(form) = &mut m.overlay {
                    form.task = task;
                    form.prompt_changed(now, limit);
                }
            }
            Ok(None) => {}
            Err(e) => m.error(format!("{e:#}")),
        }
    }

    fn edit_message(
        &self,
        m: &mut Model,
        terminal: &mut DefaultTerminal,
        to: &SessionName,
        text: &str,
    ) {
        Self::fill_screen(m, terminal);
        let edited = self.edit_task(terminal, text);
        m.clock = Moment::now();
        let Some(lost) = update::message_edited(m, to, edited.map_err(|e| format!("{e:#}"))) else {
            return;
        };
        match write_transcript(&format!("{to}-message"), &lost) {
            Ok(path) => m.error(format!(
                "the edited message was dropped: {to} is gone · kept in {}",
                path.display()
            )),
            Err(e) => m.error(format!(
                "the edited message was dropped: {to} is gone · {e:#}"
            )),
        }
    }

    /// Edits `task` in `$EDITOR` through a temp file. `None` if the editor
    /// was cancelled. The text is cleaned of carriage returns and control
    /// characters, as pasted text is.
    fn edit_task(&self, terminal: &mut DefaultTerminal, task: &str) -> Result<Option<String>> {
        let file = tempfile::Builder::new()
            .prefix("agent-task-")
            .suffix(".md")
            .tempfile()?;
        std::fs::write(file.path(), task)
            .with_context(|| format!("writing {}", file.path().display()))?;
        let dir = file
            .path()
            .parent()
            .map_or_else(std::env::temp_dir, Path::to_path_buf);
        if let Edited::Cancelled = self.edit(terminal, &dir, Some(file.path()), None)? {
            return Ok(None);
        }
        let edited = std::fs::read_to_string(file.path())
            .with_context(|| format!("reading {}", file.path().display()))?;
        let clean = clean(&edited, CarriageReturn::Newline, None);
        Ok(Some(clean.trim_end().to_string()))
    }

    fn connect_failed(m: &mut Model, ticket: Ticket, result: Result<()>) {
        let Err(e) = result else { return };
        m.abandon(ticket);
        m.error(format!("{e:#}"));
    }
}

pub fn run(options: Options) -> Result<()> {
    clear_stale_transcripts(&std::env::temp_dir(), TRANSCRIPT_KEPT);
    let label = options
        .backend
        .harnesses
        .get(&options.harness)
        .with_context(|| format!("unknown harness {}", options.harness))?
        .label()
        .to_string();
    let colours = Colours::from_env();
    let backdrop = palette::backdrop(&options.config.theme, colours);
    let palette = Palette::resolve(&options.config.theme, backdrop, colours);
    let guard = ScreenGuard::enter()?;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
    event_loop(&mut terminal, options, label, palette, guard.keys)
}

fn use_keys(m: &mut Model, keyboard: Keyboard, configured: agent_core::config::Keys) {
    m.keys = Sendable::resolve(configured, keyboard);
    if let Some(warning) = m.keys.warning() {
        m.warn(warning);
    }
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    options: Options,
    label: String,
    palette: Palette,
    keys: Keyboard,
) -> Result<()> {
    let (msgs, inbox) = mpsc::channel();
    let (input, input_ctl) = mpsc::channel();
    spawn_input(msgs.clone(), input_ctl);
    let workers = worker::spawn(
        Arc::clone(&options.backend),
        options.config.sources.clone(),
        msgs,
    );
    let _ = workers.refresh.send(());
    let rt = Runtime {
        backend: options.backend,
        emulator: options.emulator,
        workers,
        input,
        keys,
    };

    let mut m = Model::new(
        options.start_dir,
        rt.backend.own.clone(),
        options.harness,
        label,
        rt.backend.host.name_limit(),
        options.home,
    );
    m.follow = rt
        .backend
        .store
        .last_selected()
        .ok()
        .flatten()
        .map(model::Follow::Restored);
    m.actions = options.config.actions;
    use_keys(&mut m, keys, options.config.keys);
    m.palette = palette;
    let mut last_refresh = Instant::now();
    let mut last_selection = None;
    let mut last_title = String::new();

    loop {
        if let Pane::Connected(c) = &mut m.pane
            && c.term.pump() == Liveness::Exited
        {
            update::connection_ended(&mut m);
        }
        if let Some(watcher) = m.watch_mut().and_then(Watch::watcher_mut)
            && watcher.term.pump() == Liveness::Exited
        {
            update::watcher_ended(&mut m);
        }
        let size = Rect::from(terminal.size()?);
        if size != m.size {
            update::resized(&mut m, size);
        }
        if let (Pane::Connected(c), Some(size)) = (&mut m.pane, TermSize::of(m.areas.terminal))
            && let Err(e) = c.term.resize(size)
        {
            m.error(format!("resizing the pane: {e}"));
        }
        view::paint(terminal, &mut m)?;
        let title = m.attention().title();
        if title != last_title {
            set_title(terminal, &title);
            last_title = title;
        }

        let mut effects = update::tick(&mut m);
        match inbox.recv_timeout(FRAME) {
            Ok(msg) => {
                m.clock = Moment::now();
                effects.extend(update::update(&mut m, msg));
                for msg in inbox.try_iter() {
                    effects.extend(update::update(&mut m, msg));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if let Flow::Quit = rt.apply(&mut m, terminal, effects)? {
            if let Some(finishing) = m.restarts.as_ref().and_then(model::Batch::finishing) {
                m.info(finishing);
                view::paint(terminal, &mut m)?;
            }
            break;
        }

        let selection = m.selected_entry().map(|e| e.name().clone());
        if let Some(selection) = selection.filter(|s| last_selection.as_ref() != Some(s)) {
            last_selection = Some(selection.clone());
            rt.send(Job::Select(selection))?;
        }

        if last_refresh.elapsed() >= REFRESH_EVERY {
            last_refresh = Instant::now();
            rt.workers
                .refresh
                .send(())
                .ok()
                .context("the refresh worker stopped")?;
            if let Some(look) = update::recheck_elsewhere(&mut m) {
                rt.workers.look_elsewhere(look);
            }
        }
    }
    Ok(())
}

/// Why an effect that runs in a folder can't: the folder is gone, which only
/// the runner may look for.
fn refusal(effect: &Effect) -> Option<(StatusKind, String)> {
    match effect {
        Effect::Edit(open) => Some(open.dir())
            .filter(|dir| !dir.is_dir())
            .map(|dir| (StatusKind::Info, format!("{} is gone", dir.display()))),
        Effect::Interactive { label, launch } => launch
            .cwd
            .as_deref()
            .filter(|cwd| !cwd.is_dir())
            .map(|cwd| {
                (
                    StatusKind::Error,
                    format!("{label}: {} is gone", cwd.display()),
                )
            }),
        _ => None,
    }
}

const TRANSCRIPT_PREFIX: &str = "agent-transcript-";

/// How long a transcript file stays: the editor may outlive the call, so
/// they are cleared at startup once they are older than this.
const TRANSCRIPT_KEPT: Duration = Duration::from_hours(24);

/// Removes transcript files in `dir` older than `kept`.
fn clear_stale_transcripts(dir: &Path, kept: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_string_lossy()
            .starts_with(TRANSCRIPT_PREFIX)
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|at| at.elapsed().is_ok_and(|age| age > kept));
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Writes a transcript's Markdown to a new private file (0600, a unique name
/// that can't be a link planted in advance) under the temp dir and returns
/// its path. The editor may detach and read it later, so it is left for
/// [`clear_stale_transcripts`].
fn write_transcript(name: &str, text: &str) -> Result<std::path::PathBuf> {
    let safe: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    let mut file = tempfile::Builder::new()
        .prefix(&format!("{TRANSCRIPT_PREFIX}{safe}-"))
        .suffix(".md")
        .tempfile()
        .context("creating a transcript file")?;
    std::io::Write::write_all(&mut file, text.as_bytes())
        .with_context(|| format!("writing {}", file.path().display()))?;
    let (_, path) = file.keep().context("keeping the transcript file")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use agent_core::actions::CommandLine;

    use super::*;

    #[test]
    fn running_in_a_folder_that_is_gone_is_refused_with_its_name() {
        let dir = tempfile::tempdir().expect("a dir");
        let gone = dir.path().join("deleted");
        let edit = Effect::Edit(Open::File {
            root: gone.clone(),
            file: "a.rs".into(),
        });
        let (kind, why) = refusal(&edit).expect("refused");
        assert_eq!(kind, StatusKind::Info);
        assert!(why.contains("deleted") && why.ends_with("is gone"), "{why}");
        assert!(refusal(&Effect::Edit(Open::Checkout(gone.clone()))).is_some());
        assert!(refusal(&Effect::Edit(Open::Checkout(dir.path().into()))).is_none());

        let interactive = |cwd| Effect::Interactive {
            label: "lazygit".into(),
            launch: CommandLine {
                program: "lazygit".into(),
                args: Vec::new(),
                cwd: Some(cwd),
            },
        };
        let (kind, why) = refusal(&interactive(gone)).expect("refused");
        assert_eq!(kind, StatusKind::Error);
        assert!(why.starts_with("lazygit: "), "{why}");
        assert!(refusal(&interactive(dir.path().into())).is_none());
        assert!(refusal(&Effect::Quit).is_none());
    }

    #[test]
    fn old_transcript_files_are_cleared_and_other_files_and_new_ones_stay() {
        let dir = tempfile::tempdir().expect("a dir");
        let old = dir.path().join("agent-transcript-x-old.md");
        let new = dir.path().join("agent-transcript-x-new.md");
        let other = dir.path().join("notes.md");
        for path in [&old, &new, &other] {
            std::fs::write(path, "x").expect("written");
        }
        let day_ago = std::time::SystemTime::now() - std::time::Duration::from_hours(48);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .and_then(|f| f.set_modified(day_ago))
            .expect("aged");
        super::clear_stale_transcripts(dir.path(), std::time::Duration::from_hours(24));
        assert!(!old.exists() && new.exists() && other.exists());
    }

    #[test]
    fn transcripts_get_their_own_private_file_each_time() {
        let a = super::write_transcript("a.b/c", "one").expect("written");
        let b = super::write_transcript("a_b/c", "two").expect("written");
        assert_ne!(a, b, "names that sanitise alike don't share a file");
        assert_eq!(std::fs::read_to_string(&a).expect("reads"), "one");
        assert_eq!(std::fs::read_to_string(&b).expect("reads"), "two");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&a).expect("exists").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(
            super::base64("/w/api/fix ✓".as_bytes()),
            "L3cvYXBpL2ZpeCDinJM="
        );
    }
    fn keys_config(nav: &str) -> agent_core::config::Keys {
        let config: Config = format!("[keys]\nnav = '{nav}'").parse().expect("parses");
        config.keys
    }

    #[test]
    fn the_fallback_warning_reaches_the_status_bar_and_the_default_key_is_in_force() {
        let mut m = fixtures::fleet(1);
        use_keys(&mut m, Keyboard::Legacy, keys_config("super-j"));
        let status = m.status.as_ref().expect("a status");
        assert_eq!(status.kind, StatusKind::Warning);
        assert_eq!(
            status.text,
            "keys.nav ⌘j is not available in this terminal, using ⌃]"
        );
        assert_eq!(m.keys.nav().to_string(), "⌃]");
    }

    #[test]
    fn nothing_is_said_when_every_key_is_reachable() {
        let mut m = fixtures::fleet(1);
        use_keys(&mut m, Keyboard::Disambiguated, keys_config("super-j"));
        assert!(m.status.is_none());
        assert_eq!(m.keys.nav().to_string(), "⌘j");
    }
}
