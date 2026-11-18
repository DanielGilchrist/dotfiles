//! The raw thin client: it puts a terminal on a session. What it draws is
//! what the host sends, byte for byte, and what is typed goes to the host.

mod detach;
mod outer;
mod tty;

use std::io::Write;
use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use agent_core::size::TermSize;
use agent_host::{
    ClientInfo, Command, ExitStatus, Frame, Greeting, Hello, HelloRefusal, HostFrame,
    PROTOCOL_VERSION, Payload, Purpose, ReadError, Role, Standing, Take, Welcome, read_frame,
    write_frame,
};

use crate::paths::SocketPath;
use crate::peer::{Foreign, Peer, PeerPolicy};

pub use detach::DetachKey;
use detach::Scanner;
pub use outer::Outer;
pub use tty::{Console, run};

/// How long a held byte that may start the detach key waits for the rest of
/// it, which is also how long a lone Esc is held back.
const HELD_KEY_WAIT: Duration = Duration::from_millis(50);

/// How long the host has to answer a `Hello`.
/// The longest a bracketed paste is taken for one without its end, in
/// time. Past it the detach key works again, and nothing is said.
const PASTE_WINDOW: Duration = Duration::from_secs(10);

/// Bytes of typing waiting for a host that doesn't read, past which more is
/// dropped. Typing is still scanned for the detach key, which is what lets a
/// person leave a session whose host has stopped.
const UPLINK_BYTES: usize = 16 * 1024 * 1024;

const GREETING_WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connecting to the session: {0}")]
    Connect(io::Error),
    #[error("the session's host was refused: {0}")]
    Refused(String),
    #[error("the socket is served by {0}")]
    Foreign(#[from] Foreign),
    #[error("the host sent something that isn't a greeting")]
    NoGreeting,
    #[error("writing to the terminal: {0}")]
    Terminal(io::Error),
    #[error("talking to the host: {0}")]
    Host(io::Error),
}

fn refusal_text(refusal: HelloRefusal) -> String {
    match refusal {
        HelloRefusal::Version { supported } => format!(
            "this host speaks protocol {} to {} and this client {}, restart the session to upgrade its host",
            supported.min().get(),
            supported.max().get(),
            PROTOCOL_VERSION.get()
        ),
        HelloRefusal::Malformed => "its hello was malformed".into(),
        HelloRefusal::HelloTimeout => "its hello came too late".into(),
    }
}

/// How the attachment ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The detach key, or a `Bye`.
    Detached,
    /// A signal asked this process to end.
    Terminated(i32),
    /// Another client closed this one.
    DetachedBy(ClientInfo),
    Exited(ExitStatus),
    HostGone,
}

/// What reaches the client from outside its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum External {
    Resized(TermSize),
    /// A signal that asks the process to end, by number.
    Terminated(i32),
}

pub struct Wiring {
    pub input: Box<dyn Read + Send>,
    pub outer: Arc<Outer>,
    pub external: Receiver<External>,
}

/// What a client attaches as, before it has a terminal size to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    Window(Take),
    Preview,
    Watch(Purpose),
}

impl Seat {
    fn role(self, size: TermSize) -> Role {
        match self {
            Self::Window(take) => Role::Window {
                take,
                size,
                colours: None,
            },
            Self::Preview => Role::Preview {
                size,
                colours: None,
            },
            Self::Watch(purpose) => Role::Watch { purpose },
        }
    }
}

pub struct Options {
    pub role: Role,
    /// `None` for an embedder that must pass every key through.
    pub detach: Option<DetachKey>,
}

enum Event {
    Input(Vec<u8>),
    InputClosed,
    External(External),
    Host(HostFrame),
    HostClosed,
}

/// Where the client sits, which decides what a key does. Only a window that
/// was told it lost the session claims it back on a key. One that waits to
/// own it, because it asked politely or is a preview, is promoted when the
/// owner leaves, and a watcher never owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Posture {
    Owning,
    Demoted,
    /// A preview another window displaced: it draws nothing more, since what
    /// the new owner's size makes the program repaint would show for as long
    /// as it takes the TUI to learn the agent is open elsewhere.
    Displaced,
    Waiting,
    Watching,
}

impl Posture {
    fn at_welcome(role: Role, standing: Standing) -> Self {
        match (role, standing) {
            (_, Standing::Owner) => Self::Owning,
            (Role::Window { .. } | Role::Preview { .. }, Standing::Watcher) => Self::Waiting,
            (Role::Watch { .. } | Role::Control, Standing::Watcher) | (_, Standing::Control) => {
                Self::Watching
            }
        }
    }

    fn demoted(role: Role) -> Self {
        match role {
            Role::Window { .. } => Self::Demoted,
            Role::Preview { .. } => Self::Displaced,
            Role::Watch { .. } | Role::Control => Self::Watching,
        }
    }

    fn draws(self) -> bool {
        self != Self::Displaced
    }
}

fn role_size(role: Role) -> Option<TermSize> {
    match role {
        Role::Window { size, .. } | Role::Preview { size, .. } => Some(size),
        Role::Watch { .. } | Role::Control => None,
    }
}

fn connect(socket: &SocketPath, role: Role) -> Result<(UnixStream, Welcome), ClientError> {
    let mut stream = UnixStream::connect(socket.as_path()).map_err(ClientError::Connect)?;
    let peer = Peer::of(&stream).map_err(ClientError::Connect)?;
    PeerPolicy::current_user().admit(peer)?;
    stream
        .set_read_timeout(Some(GREETING_WAIT))
        .map_err(ClientError::Connect)?;
    write_frame(
        &mut stream,
        &Hello {
            version: PROTOCOL_VERSION,
            role,
        },
    )
    .map_err(ClientError::Host)?;
    match read_frame::<Greeting>(&mut stream) {
        Ok(Some(Greeting::Welcome(welcome))) => {
            stream.set_read_timeout(None).map_err(ClientError::Host)?;
            Ok((stream, welcome))
        }
        Ok(Some(Greeting::Refused(refusal))) => Err(ClientError::Refused(refusal_text(refusal))),
        Ok(None) | Err(ReadError::Decode(_) | ReadError::Cut) => Err(ClientError::NoGreeting),
        Err(ReadError::Io(e)) => Err(ClientError::Host(e)),
    }
}

/// Attaches to a session and runs until it ends. The caller restores the
/// terminal afterwards ([`Outer::restore`]), so it is restored however this
/// returns.
pub fn attach(socket: &SocketPath, options: Options, wiring: Wiring) -> Result<Ended, ClientError> {
    let Options { role, detach } = options;
    let (stream, welcome) = connect(socket, role)?;
    let (events, heard) = mpsc::channel();
    let mut host = stream.try_clone().map_err(ClientError::Host)?;
    let host_events = events.clone();
    thread::spawn(move || {
        while let Ok(Some(frame)) = read_frame::<HostFrame>(&mut host) {
            if host_events.send(Event::Host(frame)).is_err() {
                return;
            }
        }
        let _ = host_events.send(Event::HostClosed);
    });
    let Wiring {
        mut input,
        outer,
        external,
    } = wiring;
    let typed = events.clone();
    thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match input.read(&mut buffer) {
                Ok(0) | Err(_) => {
                    let _ = typed.send(Event::InputClosed);
                    return;
                }
                Ok(n) => {
                    if typed.send(Event::Input(buffer[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    });
    let outside = events.clone();
    let outside_events = events;
    thread::spawn(move || {
        for happened in external {
            if outside.send(Event::External(happened)).is_err() {
                return;
            }
        }
    });

    let uplink = Uplink::start(
        stream.try_clone().map_err(ClientError::Host)?,
        outside_events,
    );
    let mut client = Client {
        stream,
        uplink,
        role,
        posture: Posture::at_welcome(role, welcome.standing),
        size: role_size(role).unwrap_or(welcome.size),
        scanner: Scanner::new(detach),
        holding_since: None,
        paste_since: None,
        outer,
    };
    let ended = client.run(&heard);
    let _ = client.stream.shutdown(std::net::Shutdown::Both);
    ended
}

/// Writes to the host on a thread of its own, so a host that has stopped
/// reading stops only that thread, and the client still sees the detach key
/// and signals.
struct Uplink {
    queue: Sender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
}

impl Uplink {
    fn start(mut stream: UnixStream, events: Sender<Event>) -> Self {
        let (queue, writing) = mpsc::channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&queued);
        thread::spawn(move || {
            for bytes in writing {
                if stream.write_all(&bytes).is_err() {
                    let _ = events.send(Event::HostClosed);
                    return;
                }
                counted.fetch_sub(bytes.len(), Ordering::AcqRel);
            }
        });
        Self { queue, queued }
    }

    /// Typing is dropped once [`UPLINK_BYTES`] wait. A command is a few bytes
    /// and always queues.
    fn typing(&self, command: &Command) {
        self.push(command, UPLINK_BYTES);
    }

    fn command(&self, command: &Command) {
        self.push(command, usize::MAX);
    }

    fn push(&self, command: &Command, cap: usize) {
        let bytes = command.encode();
        if self
            .queued
            .load(Ordering::Acquire)
            .saturating_add(bytes.len())
            > cap
        {
            return;
        }
        self.queued.fetch_add(bytes.len(), Ordering::AcqRel);
        let _ = self.queue.send(bytes);
    }
}

struct Client {
    stream: UnixStream,
    uplink: Uplink,
    role: Role,
    posture: Posture,
    size: TermSize,
    scanner: Scanner,
    /// When the scanner began holding back bytes, so the wait for the rest of
    /// a key ends at a fixed time however much else arrives meanwhile.
    holding_since: Option<Instant>,
    /// When the scanner began taking input for a bracketed paste.
    paste_since: Option<Instant>,
    outer: Arc<Outer>,
}

impl Client {
    fn deadline(&self) -> Option<Instant> {
        let held = self.holding_since.map(|since| since + HELD_KEY_WAIT);
        let pasting = self.paste_since.map(|since| since + PASTE_WINDOW);
        [held, pasting].into_iter().flatten().min()
    }

    /// Acts on a deadline that passed: bytes held back as a possible key are
    /// typed, and a paste that never ended is let go of.
    fn expire(&mut self) {
        let now = Instant::now();
        if self
            .holding_since
            .is_some_and(|since| now >= since + HELD_KEY_WAIT)
        {
            self.holding_since = None;
            let held = self.scanner.flush();
            self.typed(&held);
        }
        if self
            .paste_since
            .is_some_and(|since| now >= since + PASTE_WINDOW)
        {
            self.paste_since = None;
            let held = self.scanner.abandon_paste();
            self.typed(&held);
        }
    }

    fn run(&mut self, heard: &Receiver<Event>) -> Result<Ended, ClientError> {
        loop {
            let event = match self.deadline() {
                Some(at) => {
                    match heard.recv_timeout(at.saturating_duration_since(Instant::now())) {
                        Ok(event) => event,
                        Err(RecvTimeoutError::Timeout) => {
                            self.expire();
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => return Ok(Ended::HostGone),
                    }
                }
                None => match heard.recv() {
                    Ok(event) => event,
                    Err(_) => return Ok(Ended::HostGone),
                },
            };
            if let Some(ended) = self.handle(event)? {
                return Ok(ended);
            }
        }
    }

    fn handle(&mut self, event: Event) -> Result<Option<Ended>, ClientError> {
        match event {
            Event::Input(bytes) => {
                let scanned = self.scanner.feed(&bytes);
                let now = Instant::now();
                self.holding_since = self.scanner.holding().then_some(now);
                self.paste_since = match (self.scanner.pasting(), self.paste_since) {
                    (true, since) => since.or(Some(now)),
                    (false, _) => None,
                };
                self.typed(&scanned.forward);
                if scanned.detach {
                    self.send(&Command::Bye);
                    return Ok(Some(Ended::Detached));
                }
            }
            Event::InputClosed => {}
            Event::External(External::Resized(size)) => {
                self.size = size;
                if self.posture == Posture::Owning {
                    self.send(&Command::Resize(size));
                }
            }
            Event::External(External::Terminated(signal)) => {
                return Ok(Some(Ended::Terminated(signal)));
            }
            Event::Host(frame) => return self.host(frame),
            Event::HostClosed => return Ok(Some(Ended::HostGone)),
        }
        Ok(None)
    }

    fn host(&mut self, frame: HostFrame) -> Result<Option<Ended>, ClientError> {
        match frame {
            HostFrame::Output(_) | HostFrame::Resync if !self.posture.draws() => {}
            HostFrame::Output(bytes) => self
                .outer
                .show(bytes.as_slice())
                .map_err(ClientError::Terminal)?,
            HostFrame::Resync => self
                .outer
                .show(b"\x1b[0m\x1b[2J\x1b[H")
                .map_err(ClientError::Terminal)?,
            HostFrame::Promoted => {
                self.posture = Posture::Owning;
                self.send(&Command::Resize(self.size));
            }
            HostFrame::Demoted { .. } => self.posture = Posture::demoted(self.role),
            HostFrame::Detached { by } => return Ok(Some(Ended::DetachedBy(by))),
            HostFrame::Exited(status) => return Ok(Some(Ended::Exited(status))),
            HostFrame::Snapshot(_)
            | HostFrame::SnapshotEnd
            | HostFrame::Clients(_)
            | HostFrame::Forbidden { .. } => {}
        }
        Ok(None)
    }

    fn typed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        match self.posture {
            Posture::Owning => {
                for chunk in Payload::split(bytes) {
                    self.uplink.typing(&Command::Input(chunk));
                }
            }
            Posture::Demoted => {
                self.send(&Command::Claim(self.size));
                self.posture = Posture::Owning;
            }
            Posture::Waiting | Posture::Displaced | Posture::Watching => {}
        }
    }

    fn send(&self, command: &Command) {
        self.uplink.command(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::size;

    fn window(take: Take) -> Role {
        Role::Window {
            take,
            size: size(1, 1),
            colours: None,
        }
    }

    #[test]
    fn only_a_window_that_was_demoted_claims_on_a_key() {
        let polite = Posture::at_welcome(window(Take::Politely), Standing::Watcher);
        assert_eq!(polite, Posture::Waiting);
        assert_eq!(Posture::demoted(window(Take::Over)), Posture::Demoted);
        let preview = Role::Preview {
            size: size(1, 1),
            colours: None,
        };
        assert_eq!(
            Posture::at_welcome(preview, Standing::Watcher),
            Posture::Waiting
        );
        assert_eq!(Posture::demoted(preview), Posture::Displaced);
        assert!(!Posture::Displaced.draws());
        assert!(Posture::Waiting.draws());
        let watch = Role::Watch {
            purpose: Purpose::Window,
        };
        assert_eq!(
            Posture::at_welcome(watch, Standing::Watcher),
            Posture::Watching
        );
        assert_eq!(
            Posture::at_welcome(Role::Control, Standing::Control),
            Posture::Watching
        );
        assert_eq!(
            Posture::at_welcome(window(Take::Over), Standing::Owner),
            Posture::Owning
        );
    }
}
