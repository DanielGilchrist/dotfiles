//! Helpers for tests that run a real host in a temporary directory and talk
//! to it as a client.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Write;
use std::num::NonZeroU16;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use agent_core::config::Osc52;
use agent_core::names::SessionName;
use agent_core::size::TermSize;
use agent_host::{
    Command, Decoder, ExitStatus, Frame, Greeting, Hello, HostFrame, PROTOCOL_VERSION, Progress,
    Role, Welcome, write_frame,
};
use nix::unistd::Uid;

use crate::paths::{Creation, HostFiles, Runtime, RuntimeDir, SocketPath};
use crate::record::HostRecord;
use crate::screen::ScrollbackCap;
use crate::{HostConfig, HostError, Launch, Timing, serve};

pub const WAIT: Duration = Duration::from_secs(10);

pub fn size(rows: u16, cols: u16) -> TermSize {
    TermSize {
        rows: NonZeroU16::new(rows).unwrap_or(NonZeroU16::MIN),
        cols: NonZeroU16::new(cols).unwrap_or(NonZeroU16::MIN),
    }
}

pub fn session(name: &str) -> SessionName {
    name.parse()
        .unwrap_or_else(|_| panic!("{name} is a session name"))
}

/// Calls `ready` until it holds or `within` passes.
pub fn eventually(within: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    loop {
        if ready() {
            return true;
        }
        if started.elapsed() >= within {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// A runtime directory and a state directory that are removed when it drops.
/// They live under `/tmp` so a socket path stays short.
pub struct Sandbox {
    _dir: tempfile::TempDir,
    pub runtime: Runtime,
    pub files: HostFiles,
    pub state: PathBuf,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("a")
            .tempdir_in("/tmp")
            .unwrap_or_else(|e| panic!("a temporary directory: {e}"));
        let runtime = RuntimeDir::locate(Some(dir.path().join("r")), None, Uid::current())
            .open(Creation::Create)
            .unwrap_or_else(|e| panic!("a runtime directory: {e}"));
        let state = dir.path().join("s");
        Self {
            files: HostFiles::under(&state),
            runtime,
            state,
            _dir: dir,
        }
    }

    /// Another sandbox that shares this one's runtime directory but keeps
    /// its records and logs elsewhere, like a second state directory.
    #[must_use]
    pub fn beside(&self) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("b")
            .tempdir_in("/tmp")
            .unwrap_or_else(|e| panic!("a temporary directory: {e}"));
        let state = dir.path().join("s");
        Self {
            files: HostFiles::under(&state),
            runtime: self.runtime.clone(),
            state,
            _dir: dir,
        }
    }

    /// A file a program waits for, so a test decides when it goes on: a
    /// program that exits by itself can be gone before a client connects.
    pub fn gate(&self, name: &str) -> Gate {
        std::fs::create_dir_all(&self.state).unwrap_or_else(|e| panic!("a state dir: {e}"));
        Gate(self.state.join(name))
    }

    pub fn socket(&self, name: &SessionName) -> SocketPath {
        self.runtime
            .socket(name)
            .unwrap_or_else(|e| panic!("a socket path: {e}"))
    }

    /// A host for `name` that runs `script` under `sh -c`, with timings short
    /// enough for a test.
    pub fn config(&self, name: &SessionName, script: &str) -> HostConfig {
        HostConfig {
            name: name.clone(),
            files: self.files.clone(),
            runtime: self.runtime.clone(),
            launch: Launch::Command(HostRecord {
                program: "sh".into(),
                args: vec!["-c".into(), script.into()],
                cwd: std::env::temp_dir(),
            }),
            size: size(24, 80),
            scrollback: ScrollbackCap::DEFAULT,
            osc52: Osc52::Off,
            inherited: vec![(
                OsString::from("PATH"),
                std::env::var_os("PATH").unwrap_or_default(),
            )],
            timing: Timing {
                hello: Duration::from_millis(400),
                drain: Duration::from_millis(600),
                kill_grace: Duration::from_millis(400),
                output_after_exit: Duration::from_millis(200),
                update_hold: Duration::from_millis(100),
                socket_check: Duration::from_millis(40),
                input_wait: Duration::from_millis(1500),
                write_stall: Duration::from_millis(1500),
            },
            max_connections: None,
            ready: None,
        }
    }

    pub fn start(&self, config: HostConfig) -> RunningHost {
        let socket = self.socket(&config.name);
        let thread = thread::spawn(move || serve(config));
        let up = eventually(WAIT, || UnixStream::connect(socket.as_path()).is_ok());
        assert!(up, "the host's socket never came up");
        RunningHost {
            thread: Some(thread),
            socket,
        }
    }

    pub fn host(&self, name: &str, script: &str) -> RunningHost {
        self.start(self.config(&session(name), script))
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Gate(PathBuf);

impl Gate {
    /// A shell command that returns once the gate is open.
    pub fn wait(&self) -> String {
        format!("while [ ! -f {} ]; do sleep 0.02; done", self.0.display())
    }

    pub fn open(&self) {
        std::fs::write(&self.0, "").unwrap_or_else(|e| panic!("opening a gate: {e}"));
    }
}

pub struct RunningHost {
    thread: Option<JoinHandle<Result<ExitStatus, HostError>>>,
    pub socket: SocketPath,
}

impl RunningHost {
    /// The host's result once it has ended, or a panic when it doesn't.
    pub fn finish(mut self) -> Result<ExitStatus, HostError> {
        let Some(thread) = self.thread.take() else {
            panic!("finished twice");
        };
        assert!(
            eventually(WAIT, || thread.is_finished()),
            "the host didn't end"
        );
        thread
            .join()
            .unwrap_or_else(|_| panic!("the host panicked"))
    }
}

impl Drop for RunningHost {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take()
            && !thread.is_finished()
            && let Ok(mut kill) = Conn::open(&self.socket, Role::Control)
        {
            kill.send(&Command::Kill);
            eventually(WAIT, || thread.is_finished());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    Frame,
    Quiet,
    Closed,
}

/// A client of a host, with a screen of its own built from what it was sent.
pub struct Conn {
    stream: UnixStream,
    decoder: Option<Decoder<HostFrame>>,
    pub welcome: Welcome,
    pub mirror: vt100::Parser,
    /// Frames that aren't output, oldest first.
    pub events: VecDeque<HostFrame>,
    pub snapshot: Vec<u8>,
    /// Bytes of output received, replay included.
    pub output_bytes: usize,
    /// Those bytes, as they came.
    pub output: Vec<u8>,
    pub closed: bool,
}

#[derive(Debug)]
pub enum OpenError {
    Refused(agent_host::HelloRefusal),
    Io(String),
}

impl Conn {
    pub fn open(socket: &SocketPath, role: Role) -> Result<Self, OpenError> {
        Self::open_with(
            socket,
            &Hello {
                version: PROTOCOL_VERSION,
                role,
            },
        )
    }

    pub fn open_with(socket: &SocketPath, hello: &Hello) -> Result<Self, OpenError> {
        let mut stream =
            UnixStream::connect(socket.as_path()).map_err(|e| OpenError::Io(e.to_string()))?;
        write_frame(&mut stream, hello).map_err(|e| OpenError::Io(e.to_string()))?;
        stream
            .set_read_timeout(Some(WAIT))
            .map_err(|e| OpenError::Io(e.to_string()))?;
        match agent_host::read_frame::<Greeting>(&mut stream) {
            Ok(Some(Greeting::Welcome(welcome))) => {
                let (rows, cols) = (welcome.size.rows.get(), welcome.size.cols.get());
                Ok(Self {
                    stream,
                    decoder: Some(Decoder::new()),
                    welcome,
                    mirror: vt100::Parser::new(rows, cols, 0),
                    events: VecDeque::new(),
                    snapshot: Vec::new(),
                    output_bytes: 0,
                    output: Vec::new(),
                    closed: false,
                })
            }
            Ok(Some(Greeting::Refused(refusal))) => Err(OpenError::Refused(refusal)),
            Ok(None) => Err(OpenError::Io("closed before a greeting".into())),
            Err(e) => Err(OpenError::Io(e.to_string())),
        }
    }

    pub fn send(&mut self, command: &Command) {
        self.send_bytes(&command.encode());
    }

    pub fn send_bytes(&mut self, bytes: &[u8]) {
        let _ = self.stream.write_all(bytes);
    }

    /// Reads at most one frame, waiting up to `within`.
    pub fn pump(&mut self, within: Duration) -> Heard {
        let deadline = Instant::now() + within;
        let mut decoder = self.decoder.take().unwrap_or_default();
        loop {
            let Some(left) = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
            else {
                self.decoder = Some(decoder);
                return Heard::Quiet;
            };
            // A socket the host already shut down refuses a timeout on macOS,
            // and its last frames are still there to read.
            let _ = self.stream.set_read_timeout(Some(left));
            match decoder.read_from(&mut &self.stream) {
                Ok(Progress::Frame(frame, next)) => {
                    self.decoder = Some(next);
                    self.take(frame);
                    return Heard::Frame;
                }
                Ok(Progress::More(next) | Progress::Blocked(next)) => decoder = next,
                Ok(Progress::Closed) | Err(_) => {
                    self.closed = true;
                    return Heard::Closed;
                }
            }
        }
    }

    fn take(&mut self, frame: HostFrame) {
        match frame {
            HostFrame::Output(bytes) => {
                self.output_bytes += bytes.as_slice().len();
                self.output.extend_from_slice(bytes.as_slice());
                self.mirror.process(bytes.as_slice());
            }
            HostFrame::Resync => {
                let (rows, cols) = self.mirror.screen().size();
                self.mirror = vt100::Parser::new(rows, cols, 0);
            }
            HostFrame::Snapshot(bytes) => self.snapshot.extend_from_slice(bytes.as_slice()),
            other => self.events.push_back(other),
        }
    }

    /// Reads until the mirror's screen text satisfies `ready`.
    pub fn wait_screen(&mut self, within: Duration, ready: impl Fn(&str) -> bool) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if ready(&self.mirror.screen().contents()) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.pump(left) {
                Heard::Frame => {}
                Heard::Quiet | Heard::Closed => return ready(&self.mirror.screen().contents()),
            }
        }
    }

    /// Reads until `ready` holds of the output received so far.
    pub fn wait_output(&mut self, within: Duration, ready: impl Fn(&[u8]) -> bool) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if ready(&self.output) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.pump(left) {
                Heard::Frame => {}
                Heard::Quiet | Heard::Closed => return ready(&self.output),
            }
        }
    }

    /// The next frame that isn't output, waiting up to `within`.
    pub fn next_event(&mut self, within: Duration) -> Option<HostFrame> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(event) = self.events.pop_front() {
                return Some(event);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.pump(left) {
                Heard::Frame => {}
                Heard::Quiet | Heard::Closed => return self.events.pop_front(),
            }
        }
    }

    /// Reads until `ready` holds of a frame that isn't output, and returns
    /// it. Frames before it are dropped.
    pub fn wait_event(
        &mut self,
        within: Duration,
        ready: impl Fn(&HostFrame) -> bool,
    ) -> Option<HostFrame> {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = self.next_event(left)?;
            if ready(&event) {
                return Some(event);
            }
        }
    }

    /// Reads until the connection closes.
    pub fn wait_closed(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while !self.closed {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            self.pump(left);
        }
        true
    }

    /// Asks for a snapshot and returns what it printed as text.
    pub fn snapshot_text(&mut self, scrollback: agent_host::Scrollback) -> String {
        let bytes = self.snapshot_bytes(scrollback, agent_host::Style::Styled);
        let mut parser = vt100::Parser::new(500, self.welcome.size.cols.get(), 0);
        parser.process(&bytes);
        parser.screen().contents()
    }

    /// Asks for a snapshot and returns the bytes it printed.
    pub fn snapshot_bytes(
        &mut self,
        scrollback: agent_host::Scrollback,
        style: agent_host::Style,
    ) -> Vec<u8> {
        self.snapshot.clear();
        self.send(&Command::Snapshot { scrollback, style });
        let ended = self.wait_event(WAIT, |frame| matches!(frame, HostFrame::SnapshotEnd));
        assert!(ended.is_some(), "no snapshot came");
        self.snapshot.clone()
    }
}

/// How two screens differ, cell by cell, with the cursor and the modes that
/// change how input is encoded.
pub fn screen_differences(left: &vt100::Screen, right: &vt100::Screen) -> Vec<String> {
    fn describe(screen: &vt100::Screen) -> Vec<String> {
        let (rows, cols) = screen.size();
        let mut cells = Vec::new();
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                cells.push(format!(
                    "{row},{col} {:?} {:?} {:?} b{} i{} u{} v{} w{}{}",
                    cell.contents(),
                    cell.fgcolor(),
                    cell.bgcolor(),
                    cell.bold(),
                    cell.italic(),
                    cell.underline(),
                    cell.inverse(),
                    cell.is_wide(),
                    cell.is_wide_continuation()
                ));
            }
        }
        cells.push(format!(
            "cursor {:?} hidden {} alt {} keypad {} appcursor {} paste {} mouse {:?} {:?}",
            screen.cursor_position(),
            screen.hide_cursor(),
            screen.alternate_screen(),
            screen.application_keypad(),
            screen.application_cursor(),
            screen.bracketed_paste(),
            screen.mouse_protocol_mode(),
            screen.mouse_protocol_encoding()
        ));
        cells
    }
    let (a, b) = (describe(left), describe(right));
    let mut differences: Vec<String> = a
        .iter()
        .zip(&b)
        .filter(|(x, y)| x != y)
        .map(|(x, y)| format!("{x} | {y}"))
        .collect();
    if a.len() != b.len() {
        differences.push(format!("{} cells against {}", a.len(), b.len()));
    }
    differences
}
