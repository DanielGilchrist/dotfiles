mod model;
mod update;
mod view;
mod worker;

#[cfg(test)]
mod fixtures;

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use agent_core::config::Config;
use agent_core::names::SessionName;
use agent_core::ops::Backend;
use agent_core::ports::{ClientId, HarnessId};
use agent_core::process::LaunchSpec;
use agent_term::{EmulatorFactory, Liveness, TermSize, Terminal};
use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;

use model::{Connection, Intent, Mode, Model, Pane, TerminalFocus, Ticket};
use update::{Effect, Open};
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
    pub own_session: Option<SessionName>,
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

fn enter_screen() -> Result<()> {
    enable_raw_mode()?;
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        EnableFocusChange
    )?;
    Ok(())
}

fn leave_screen() {
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
struct ScreenGuard;

impl ScreenGuard {
    fn enter() -> Result<Self> {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            leave_screen();
            previous(info);
        }));
        let guard = Self;
        enter_screen()?;
        Ok(guard)
    }
}

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        leave_screen();
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

struct Runtime {
    backend: Arc<Backend>,
    emulator: EmulatorFactory,
    workers: Workers,
    input: Sender<InputCtl>,
}

impl Runtime {
    fn connect(
        &self,
        m: &mut Model,
        name: SessionName,
        spec: LaunchSpec,
        intent: Intent,
        ticket: Ticket,
    ) -> Result<()> {
        let Some(_) = m.pane.awaiting(ticket) else {
            return Ok(());
        };
        let spec = spec.env_remove(self.backend.host.nesting_env().iter().copied());
        let size = TermSize::of(m.areas.terminal).context("the terminal pane has no size")?;
        let term = Terminal::spawn(&spec, size, (self.emulator)(size))
            .with_context(|| format!("attaching to {name}"))?;
        let own = term.pid().map(ClientId::new);
        m.pane = Pane::Connected(Connection {
            name: name.clone(),
            term,
            others: None,
            mode: Mode::Nav,
            opened: Instant::now(),
        });
        m.touch_mru(&name);
        match intent {
            Intent::Activate => {
                for effect in update::enter_agent(m) {
                    if let Effect::Job(job) = effect {
                        self.send(job)?;
                    }
                }
            }
            Intent::Preview => self.send(Job::CheckShared { name, own })?,
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

    /// Runs `$EDITOR` with `arg` in `cwd`, handing it the terminal; reports
    /// whether it exited successfully (`:cq` doesn't).
    fn edit(
        &self,
        terminal: &mut DefaultTerminal,
        cwd: &Path,
        arg: Option<&Path>,
    ) -> Result<Edited> {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
        let argv =
            shell_words::split(&editor).with_context(|| format!("parsing $EDITOR ({editor})"))?;
        let (program, args) = argv.split_first().context("$EDITOR is empty")?;

        let mut command = std::process::Command::new(program);
        command.args(args).args(arg).current_dir(cwd);
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
        leave_screen();
        let status = command.status();
        let resumed = enter_screen();
        let _ = self.input.send(InputCtl::Resume);
        forget_screen(terminal);
        resumed?;
        Ok(status?)
    }

    /// Zellij sizes a session to its smallest client, so while the editor
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
            match effect {
                Effect::Quit => return Ok(Flow::Quit),
                Effect::Job(job) => self.send(job)?,
                Effect::Inspect { name, worktree } => self.workers.inspect(name, worktree),
                Effect::Attach {
                    name,
                    intent,
                    ticket,
                } => {
                    let spec = self.backend.host.attach(&name);
                    let result = self.connect(m, name, spec, intent, ticket);
                    Self::connect_failed(m, ticket, result);
                }
                Effect::Connect {
                    launch,
                    intent,
                    ticket,
                } => {
                    let result = self.connect(m, launch.session, launch.spec, intent, ticket);
                    Self::connect_failed(m, ticket, result);
                }
                Effect::Edit(open) => {
                    Self::fill_screen(m, terminal);
                    let (cwd, file) = match &open {
                        Open::Checkout(dir) => (dir, None),
                        Open::File { root, file } => (root, Some(file.as_path())),
                    };
                    if let Err(e) = self.edit(terminal, cwd, file).map(drop) {
                        m.error(format!("{e:#}"));
                    }
                    m.schedule_preview();
                }
                Effect::Interactive { label, launch } => {
                    Self::fill_screen(m, terminal);
                    let mut command = std::process::Command::new(&launch.program);
                    command.args(&launch.args);
                    if let Some(cwd) = &launch.cwd {
                        command.current_dir(cwd);
                    }
                    match self.suspended(terminal, &mut command) {
                        Ok(status) if status.success() => {}
                        Ok(status) => m.error(format!("{label} exited with {status}")),
                        Err(e) => m.error(format!("{label}: {e:#}")),
                    }
                    m.schedule_preview();
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
                Effect::EditTask(task) => {
                    Self::fill_screen(m, terminal);
                    match self.edit_task(terminal, &task) {
                        Ok(Some(task)) => {
                            if let model::Overlay::New(form) = &mut m.overlay {
                                form.task = task;
                                form.prompt_changed();
                            }
                        }
                        Ok(None) => {}
                        Err(e) => m.error(format!("{e:#}")),
                    }
                }
            }
        }
        Ok(Flow::Continue)
    }

    /// Edits `task` in `$EDITOR` through a temp file. `None` if the editor
    /// was cancelled; the text is cleaned of carriage returns and control
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
        if let Edited::Cancelled = self.edit(terminal, &dir, Some(file.path()))? {
            return Ok(None);
        }
        let edited = std::fs::read_to_string(file.path())
            .with_context(|| format!("reading {}", file.path().display()))?;
        let clean: String = edited
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .collect();
        Ok(Some(clean.trim_end().to_string()))
    }

    fn connect_failed(m: &mut Model, ticket: Ticket, result: Result<()>) {
        let Err(e) = result else { return };
        m.pane.abandon(ticket);
        m.error(format!("{e:#}"));
    }
}

pub fn run(options: Options) -> Result<()> {
    let label = options
        .backend
        .harnesses
        .get(&options.harness)
        .with_context(|| format!("unknown harness {}", options.harness))?
        .label()
        .to_string();
    let _guard = ScreenGuard::enter()?;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
    event_loop(&mut terminal, options, label)
}

fn event_loop(terminal: &mut DefaultTerminal, options: Options, label: String) -> Result<()> {
    let (msgs, inbox) = mpsc::channel();
    let (input, input_ctl) = mpsc::channel();
    spawn_input(msgs.clone(), input_ctl);
    let workers = worker::spawn(Arc::clone(&options.backend), msgs);
    let _ = workers.refresh.send(());
    let rt = Runtime {
        backend: options.backend,
        emulator: options.emulator,
        workers,
        input,
    };

    let mut m = Model::new(
        options.start_dir,
        options.own_session,
        options.harness,
        label,
    );
    m.follow = rt
        .backend
        .store
        .last_selected()
        .ok()
        .flatten()
        .map(model::Follow::Restored);
    m.actions = options.config.actions;
    m.keys = options.config.keys;
    let mut last_refresh = Instant::now();
    let mut last_selection = None;
    let mut last_title = String::new();

    loop {
        if let Pane::Connected(c) = &mut m.pane
            && c.term.pump() == Liveness::Exited
        {
            let name = c.name.clone();
            m.pane = Pane::Empty;
            m.info(format!("detached from {name}"));
        }
        if let (Pane::Connected(c), Some(size)) = (&mut m.pane, TermSize::of(m.areas.terminal))
            && let Err(e) = c.term.resize(size)
        {
            m.error(format!("resizing the pane: {e}"));
        }
        terminal.draw(|f| view::draw(f, &mut m))?;
        let title = m.attention().title();
        if title != last_title {
            set_title(terminal, &title);
            last_title = title;
        }

        let mut effects = update::tick(&mut m);
        match inbox.recv_timeout(FRAME) {
            Ok(msg) => {
                effects.extend(update::update(&mut m, msg));
                for msg in inbox.try_iter() {
                    effects.extend(update::update(&mut m, msg));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if let Flow::Quit = rt.apply(&mut m, terminal, effects)? {
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
            if let Some(c) = m.pane.connection().filter(|c| c.mode != Mode::Agent) {
                let own = c.term.pid().map(ClientId::new);
                rt.send(Job::CheckShared {
                    name: c.name.clone(),
                    own,
                })?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
