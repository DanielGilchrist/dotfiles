use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;
use std::thread;

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

use super::message::Message;
use super::{Shared, connection};
use crate::paths::SocketPath;

/// How long to leave an accept that fails (no file descriptors, say) before
/// the next, so it isn't retried in a tight loop.
const ACCEPT_BACKOFF: std::time::Duration = std::time::Duration::from_millis(100);

/// The socket's file, as the filesystem knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Inode {
    device: u64,
    number: u64,
}

pub(super) struct Listener {
    socket: UnixListener,
    path: SocketPath,
    bound: Inode,
    /// Held for the host's life, so two hosts can't both find a dead socket
    /// at a path, remove it and bind there, whatever state directory each
    /// keeps its record in.
    _guard: Flock<File>,
}

#[derive(Debug)]
pub(super) enum BindError {
    /// Something that answers is already listening at the path.
    Occupied,
    Io(io::Error),
}

/// What the path names now, against the socket that was bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Socket {
    Intact,
    /// The file is gone, which `tmp_cleaner` or a person can do while the
    /// host runs. It is the only case the host binds again.
    Missing,
    /// Another file sits there, which isn't the host's to remove.
    Replaced,
}

impl Listener {
    /// Binds the path. A socket file nobody answers on is a dead host's and
    /// is replaced. One that answers is another host's, whatever state
    /// directory it keeps its lock in, and is left alone.
    pub(super) fn bind(path: SocketPath) -> Result<Self, BindError> {
        let guard = lock_beside(&path)?;
        match std::fs::symlink_metadata(path.as_path()) {
            Ok(meta) if meta.file_type().is_socket() => match UnixStream::connect(path.as_path()) {
                Ok(_) => return Err(BindError::Occupied),
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(path.as_path()).map_err(BindError::Io)?;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(BindError::Io(e)),
            },
            Ok(_) => {
                return Err(BindError::Io(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} is not a socket", path.as_path().display()),
                )));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(BindError::Io(e)),
        }
        let (socket, bound) = bind_free(&path).map_err(BindError::Io)?;
        Ok(Self {
            socket,
            path,
            bound,
            _guard: guard,
        })
    }

    /// Binds the path again after its file was removed, keeping the lock.
    fn rebind(&mut self) -> io::Result<()> {
        let (socket, bound) = bind_free(&self.path)?;
        self.socket = socket;
        self.bound = bound;
        Ok(())
    }

    fn state(&self) -> Socket {
        match std::fs::symlink_metadata(self.path.as_path()) {
            Ok(meta) if meta.dev() == self.bound.device && meta.ino() == self.bound.number => {
                Socket::Intact
            }
            Ok(_) => Socket::Replaced,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Socket::Missing,
            Err(_) => Socket::Intact,
        }
    }

    /// Unlinks the path, only if it is still the socket that was bound.
    pub(super) fn remove(&self) {
        if self.state() == Socket::Intact {
            let _ = std::fs::remove_file(self.path.as_path());
        }
    }
}

fn bind_free(path: &SocketPath) -> io::Result<(UnixListener, Inode)> {
    let socket = UnixListener::bind(path.as_path())?;
    std::fs::set_permissions(path.as_path(), std::fs::Permissions::from_mode(0o600))?;
    socket.set_nonblocking(true)?;
    let meta = std::fs::symlink_metadata(path.as_path())?;
    let bound = Inode {
        device: meta.dev(),
        number: meta.ino(),
    };
    Ok((socket, bound))
}

/// The lock file next to the socket, taken without waiting. It is left in
/// place when the host ends: removing it could let a host that is starting
/// lock a file another one is about to create.
fn lock_beside(path: &SocketPath) -> Result<Flock<File>, BindError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path.as_path().with_extension("lock"))
        .map_err(BindError::Io)?;
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(lock) => Ok(lock),
        Err((_, Errno::EWOULDBLOCK)) => Err(BindError::Occupied),
        Err((_, errno)) => Err(BindError::Io(errno.into())),
    }
}

/// Accepts connections until the host stops. When the program has exited
/// and every client was told, it takes the socket away first and only then
/// gives the lock up, so a host started in the meantime finds neither a lock
/// nor a socket of this one to fight over.
pub(super) fn accept_loop(
    mut listener: Listener,
    shared: &Arc<Shared>,
    inbox: &SyncSender<Message>,
) {
    let wait = PollTimeout::try_from(shared.timing.socket_check).unwrap_or(PollTimeout::MAX);
    let mut replaced_noted = false;
    while !shared.stop.load(Ordering::Acquire) {
        if shared.ended.load(Ordering::Acquire) {
            listener.remove();
            drop(listener);
            shared.release_lock();
            return;
        }
        let mut ready = [PollFd::new(listener.socket.as_fd(), PollFlags::POLLIN)];
        if poll(&mut ready, wait).is_err() {
            continue;
        }
        accept_pending(&listener, shared, inbox);
        match listener.state() {
            Socket::Intact => {}
            Socket::Missing => match listener.rebind() {
                Ok(()) => shared
                    .log
                    .warn("the socket file was removed, bound it again"),
                Err(e) => shared.log.warn(format_args!(
                    "the socket file was removed and can't be bound again: {e}"
                )),
            },
            Socket::Replaced => {
                if !replaced_noted {
                    replaced_noted = true;
                    shared.log.warn(
                        "another file replaced the socket, which isn't this host's to remove",
                    );
                }
            }
        }
    }
    listener.remove();
}

fn accept_pending(listener: &Listener, shared: &Arc<Shared>, inbox: &SyncSender<Message>) {
    loop {
        let stream = match listener.socket.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                shared.log.warn(format_args!("accepting a connection: {e}"));
                thread::sleep(ACCEPT_BACKOFF);
                return;
            }
        };
        let Some(slot) = shared.slot() else {
            shared.log.warn("too many connections, one was dropped");
            continue;
        };
        if let Err(e) = stream.set_nonblocking(false) {
            shared
                .log
                .warn(format_args!("a connection could not be made blocking: {e}"));
            continue;
        }
        let serving = Arc::clone(shared);
        let inbox = inbox.clone();
        let spawned = thread::Builder::new()
            .name("connection".into())
            .spawn(move || connection::serve(&stream, &serving, &inbox, slot));
        if let Err(e) = spawned {
            shared
                .log
                .warn(format_args!("starting a connection thread: {e}"));
        }
    }
}
