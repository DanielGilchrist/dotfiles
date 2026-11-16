//! The host process of a session: it runs the program in a pty, keeps its
//! screen and serves clients on a unix socket (docs/host.md).
//!
//! Threads: a pty reader and a waiter for the program, an accept thread that
//! also keeps the socket in place, a reader and a writer for each client,
//! and the core. The core owns the screen and every client's queue, so
//! everything that touches them is ordered by the one channel into it: a
//! replay built for a new client and the live output that follows can't
//! cross.

mod child;
mod connection;
mod core;
mod listener;
mod log;
mod message;

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use agent_core::config::Osc52;
use agent_core::names::SessionName;
use agent_core::size::TermSize;
use agent_host::{ClientRef, ExitStatus};
use nix::sys::resource::{Resource, getrlimit};

use crate::egress::Policy;
use crate::environment::ChildEnvironment;
use crate::paths::{HostFiles, Runtime, SocketPath, SocketPathTooLong};
use crate::peer::PeerPolicy;
use crate::record::{HostLock, HostRecord, LockError, Presence, RecordError};
use crate::screen::{ScrollbackCap, Vt100Screen};

use self::core::Core;
use self::listener::{BindError, Listener};
use self::log::Log;
use self::message::{Budget, Snapshots};

/// The messages waiting for the core, so a flood of output can't pile up in
/// memory while the core is busy.
const INBOX_DEPTH: usize = 256;

/// Connections a host serves at once, hello stage included, when file
/// descriptors allow.
const MAX_CONNECTIONS: usize = 256;

/// File descriptors a host keeps for itself: the pty, the socket, the log
/// and what its threads and the program's spawn need.
const FDS_RESERVED: u64 = 64;

/// What a connection holds: the stream and the two handles its threads
/// use, and a spare for the thread that closes it.
const FDS_PER_CONNECTION: u64 = 4;

/// Connections beyond the limit that only a control client may use, so a
/// host that is full can still be told to `Kill`.
const CONTROL_RESERVE: usize = 4;

/// Bytes of input waiting for the program, counted across every client.
const INPUT_BYTES: usize = 1024 * 1024;

/// Bytes of replies to the program's queries waiting to be written to it.
const REPLY_BYTES: usize = 64 * 1024;

/// Snapshots in flight at once. Each is a copy of the screen and scrollback.
const SNAPSHOTS_AT_ONCE: usize = 2;

/// The time limits a host works to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// How long a connection has to send its `Hello`.
    pub hello: Duration,
    /// How long the host stays up after the program exits, for the clients it
    /// had to take a snapshot.
    pub drain: Duration,
    /// How long a killed program has to end before it is killed harder.
    pub kill_grace: Duration,
    /// How long output may still arrive after the program exited.
    pub output_after_exit: Duration,
    /// The longest a new client waits for a synchronized update to end.
    pub update_hold: Duration,
    /// How often the socket's inode is checked.
    pub socket_check: Duration,
    /// How long a client's input waits for the program to take what came
    /// before it, before the client is closed.
    pub input_wait: Duration,
    /// How long a write to a client may stall before it is closed, which is
    /// also how long a snapshot it doesn't read keeps its permit.
    pub write_stall: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            hello: Duration::from_secs(2),
            drain: Duration::from_secs(2),
            kill_grace: Duration::from_secs(2),
            output_after_exit: Duration::from_millis(500),
            update_hold: Duration::from_millis(100),
            socket_check: Duration::from_millis(250),
            input_wait: Duration::from_secs(10),
            write_stall: Duration::from_secs(10),
        }
    }
}

/// What the host runs: a command it records, or the one recorded earlier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    Command(HostRecord),
    Recorded,
}

pub struct HostConfig {
    pub name: SessionName,
    pub files: HostFiles,
    pub runtime: Runtime,
    pub launch: Launch,
    /// The pty's size until the first window states its own.
    pub size: TermSize,
    pub scrollback: ScrollbackCap,
    /// Whether the owner's terminal is sent the program's clipboard writes.
    /// The starter passes it, since the host never reads the user's config.
    pub osc52: Osc52,
    /// The environment the host was started with, which becomes the
    /// program's after [`ChildEnvironment`] has been applied.
    pub inherited: Vec<(OsString, OsString)>,
    pub timing: Timing,
    /// The most clients served at once. `None` is what the file descriptor
    /// limit allows.
    pub max_connections: Option<usize>,
    /// Told `R` once the socket is up and the program runs, or `E` and the
    /// reason when the host can't start, so whoever started it knows from the
    /// host itself and not from files it left.
    pub ready: Option<File>,
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    SocketPath(#[from] SocketPathTooLong),
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error("writing the record: {0}")]
    Record(io::Error),
    #[error("reading the record: {0}")]
    Recorded(#[from] RecordError),
    #[error("there is no record of this session to start from")]
    Unrecorded,
    #[error("preparing the host's files: {0}")]
    Files(io::Error),
    #[error("another host already listens at {0}")]
    Occupied(std::path::PathBuf),
    #[error("binding the socket: {0}")]
    Bind(io::Error),
    #[error("starting {program}: {source}")]
    Spawn { program: String, source: io::Error },
}

impl HostError {
    /// Whether the session already has a host, by its lock or its socket.
    pub fn already_running(&self) -> bool {
        matches!(self, Self::Lock(LockError::Held) | Self::Occupied(_))
    }
}

/// State every thread of one host reads.
struct Shared {
    timing: Timing,
    log: Arc<Log>,
    policy: PeerPolicy,
    next_client: AtomicU32,
    connections: AtomicUsize,
    max_connections: usize,
    input: Arc<Budget>,
    replies: Arc<Budget>,
    snapshots: Arc<Snapshots>,
    stop: AtomicBool,
    /// The program exited and every client was told.
    ended: AtomicBool,
    lock: Mutex<Option<HostLock>>,
}

impl Shared {
    fn client(&self) -> ClientRef {
        ClientRef::new(self.next_client.fetch_add(1, Ordering::Relaxed))
    }

    fn slot(self: &Arc<Self>) -> Option<Slot> {
        let taken = self.connections.fetch_add(1, Ordering::AcqRel);
        let slot = Slot(Arc::clone(self));
        (taken < self.max_connections + CONTROL_RESERVE).then_some(slot)
    }

    /// Whether a client of this role may be served, now that it has said
    /// what it is. The reserve is for control clients.
    fn admits(&self, role: agent_host::Role) -> bool {
        role == agent_host::Role::Control
            || self.connections.load(Ordering::Acquire) <= self.max_connections
    }

    /// Lets another host take the session. Called only once the socket is
    /// gone, so a new host never meets this one's socket.
    fn release_lock(&self) {
        unpoisoned(self.lock.lock()).take();
    }
}

/// One of the connections a host serves at once, given back when dropped.
struct Slot(Arc<Shared>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::AcqRel);
    }
}

/// How many connections `soft` file descriptors allow.
fn connections_for(soft: u64) -> usize {
    let fits = soft.saturating_sub(FDS_RESERVED) / FDS_PER_CONNECTION;
    usize::try_from(fits).map_or(MAX_CONNECTIONS, |fits| fits.clamp(4, MAX_CONNECTIONS))
}

fn fd_connections() -> usize {
    getrlimit(Resource::RLIMIT_NOFILE).map_or(MAX_CONNECTIONS, |(soft, _)| connections_for(soft))
}

/// Runs a session until its program exits and the host has stayed up for
/// [`Timing::drain`]. The caller makes the process fit to be a host
/// (`detach_from_terminal`).
pub fn serve(mut config: HostConfig) -> Result<ExitStatus, HostError> {
    let mut ready = config.ready.take();
    let result = run(config, &mut ready);
    if let (Err(error), Some(mut pipe)) = (&result, ready) {
        let _ = write!(pipe, "E{error}");
    }
    result
}

struct Running {
    shared: Arc<Shared>,
    listener: Listener,
    messages: mpsc::Receiver<message::Message>,
    inbox: mpsc::SyncSender<message::Message>,
    program: child::Program,
}

fn run(config: HostConfig, ready: &mut Option<File>) -> Result<ExitStatus, HostError> {
    let HostConfig {
        name,
        files,
        runtime,
        launch,
        size,
        scrollback,
        osc52,
        inherited,
        timing,
        max_connections,
        ready: _,
    } = config;
    let files_for_record = files.clone();
    let socket = runtime.socket(&name)?;
    files.create_dir().map_err(HostError::Files)?;
    let record_path = files_for_record.record(&name);
    let presence = match launch {
        Launch::Command(_) => Presence::CreateIfMissing,
        Launch::Recorded => Presence::MustExist,
    };
    let lock = HostLock::acquire(&record_path, presence).map_err(|error| match error {
        LockError::Missing => HostError::Unrecorded,
        other => HostError::Lock(other),
    })?;
    let created = lock.created();
    let started = launch_host(
        Start {
            name,
            files,
            socket,
            launch,
            size,
            inherited,
            timing,
            max_connections,
        },
        lock,
    );
    let Running {
        shared,
        listener,
        messages,
        inbox,
        program,
    } = match started {
        Ok(running) => running,
        Err(error) => {
            if created {
                let _ = std::fs::remove_file(&record_path);
            }
            return Err(error);
        }
    };

    let accepting = {
        let shared = Arc::clone(&shared);
        let inbox = inbox.clone();
        thread::spawn(move || listener::accept_loop(listener, &shared, &inbox))
    };
    drop(inbox);
    if let Some(mut pipe) = ready.take() {
        let _ = pipe.write_all(b"R");
    }

    let status = Core::new(
        messages,
        Box::new(Vt100Screen::new(size, scrollback)),
        Policy { clipboard: osc52 },
        program,
        size,
        Arc::clone(&shared),
    )
    .run();

    shared.stop.store(true, Ordering::Release);
    if accepting.join().is_err() {
        shared.log.warn("the accept thread panicked");
    }
    shared.release_lock();
    Ok(status)
}

struct Start {
    name: SessionName,
    files: HostFiles,
    socket: SocketPath,
    launch: Launch,
    size: TermSize,
    inherited: Vec<(OsString, OsString)>,
    timing: Timing,
    max_connections: Option<usize>,
}

/// Everything between holding the lock and serving: the record, the log, the
/// socket and the program. A socket it bound is taken away again when the
/// program can't start.
fn launch_host(start: Start, mut lock: HostLock) -> Result<Running, HostError> {
    let Start {
        name,
        files,
        socket,
        launch,
        size,
        inherited,
        timing,
        max_connections,
    } = start;
    let record = match launch {
        Launch::Command(record) => {
            lock.store(&record).map_err(HostError::Record)?;
            record
        }
        Launch::Recorded => lock.load()?.ok_or(HostError::Unrecorded)?,
    };
    let log = Arc::new(Log::create(&files.log(&name)).map_err(HostError::Files)?);
    log.install_panic_hook();
    let shared = Arc::new(Shared {
        timing,
        log,
        policy: PeerPolicy::current_user(),
        next_client: AtomicU32::new(1),
        connections: AtomicUsize::new(0),
        max_connections: max_connections.unwrap_or_else(fd_connections),
        input: Budget::new(INPUT_BYTES),
        replies: Budget::new(REPLY_BYTES),
        snapshots: Arc::new(Snapshots::new(SNAPSHOTS_AT_ONCE)),
        stop: AtomicBool::new(false),
        ended: AtomicBool::new(false),
        lock: Mutex::new(Some(lock)),
    });
    let listener = Listener::bind(socket.clone()).map_err(|error| match error {
        BindError::Occupied => HostError::Occupied(socket.as_path().to_path_buf()),
        BindError::Io(e) => HostError::Bind(e),
    })?;
    let (inbox, messages) = mpsc::sync_channel(INBOX_DEPTH);
    let command = agent_term::HostedCommand {
        program: OsString::from(&record.program),
        args: record.args.iter().map(OsString::from).collect(),
        cwd: record.cwd.clone(),
        environment: ChildEnvironment::from_spawner(name, inherited).build(),
    };
    let program = match child::start(&command, size, &inbox, &shared.input) {
        Ok(program) => program,
        Err(source) => {
            listener.remove();
            return Err(HostError::Spawn {
                program: record.program,
                source,
            });
        }
    };
    Ok(Running {
        shared,
        listener,
        messages,
        inbox,
        program,
    })
}

fn unpoisoned<T>(result: Result<T, PoisonError<T>>) -> T {
    result.unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_are_capped_below_what_the_descriptor_limit_allows() {
        assert_eq!(connections_for(1_000_000), MAX_CONNECTIONS);
        assert_eq!(connections_for(256), 48);
        assert_eq!(connections_for(64), 4);
        assert_eq!(connections_for(0), 4);
        let at_default = connections_for(1024);
        assert!(
            u64::try_from(at_default).expect("small") * FDS_PER_CONNECTION + FDS_RESERVED <= 1024
        );
    }
}
