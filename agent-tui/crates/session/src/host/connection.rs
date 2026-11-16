use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::{Duration, Instant};

use agent_host::{
    Admission, Command, Decoder, Greeting, Hello, HelloRefusal, HostFrame, Payload, Progress,
    ReadError, SUPPORTED, read_frame, write_frame,
};

use super::message::{Drain, Held, Joining, Message, Outgoing, queue};
use super::{Shared, Slot};
use crate::peer::Peer;

enum HelloFailure {
    Closed,
    TimedOut,
    Malformed,
}

fn read_hello(stream: &UnixStream, deadline: Instant) -> Result<Hello, HelloFailure> {
    let mut decoder = Decoder::<Hello>::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or(HelloFailure::TimedOut)?;
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|_| HelloFailure::Closed)?;
        match decoder.read_from(&mut &*stream) {
            Ok(Progress::Frame(hello, _)) => return Ok(hello),
            Ok(Progress::More(next) | Progress::Blocked(next)) => decoder = next,
            Err(ReadError::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(HelloFailure::TimedOut);
            }
            Ok(Progress::Closed) | Err(ReadError::Cut | ReadError::Io(_)) => {
                return Err(HelloFailure::Closed);
            }
            Err(ReadError::Decode(_)) => return Err(HelloFailure::Malformed),
        }
    }
}

fn refuse(stream: &UnixStream, refusal: HelloRefusal) {
    let _ = write_frame(&mut &*stream, &Greeting::Refused(refusal));
}

/// Serves one connection until it ends: checks the peer, reads the `Hello`,
/// then passes the client's commands on to the core. The `slot` is given back
/// when it returns.
pub(super) fn serve(
    stream: &UnixStream,
    shared: &Arc<Shared>,
    inbox: &SyncSender<Message>,
    slot: Slot,
) {
    let _slot = slot;
    let peer = match Peer::of(stream) {
        Ok(peer) => peer,
        Err(e) if e.kind() == io::ErrorKind::NotConnected => return,
        Err(e) => {
            shared
                .log
                .warn(format_args!("a peer could not be identified: {e}"));
            return;
        }
    };
    if let Err(foreign) = shared.policy.admit(peer) {
        shared.log.warn(format_args!("refused {foreign}"));
        return;
    }
    let deadline = Instant::now() + shared.timing.hello;
    let hello = match read_hello(stream, deadline) {
        Ok(hello) => hello,
        Err(HelloFailure::Closed) => return,
        Err(HelloFailure::TimedOut) => {
            shared
                .log
                .warn(format_args!("{} sent no hello in time", peer.pid.get()));
            refuse(stream, HelloRefusal::HelloTimeout);
            return;
        }
        Err(HelloFailure::Malformed) => {
            shared
                .log
                .warn(format_args!("{} sent a malformed hello", peer.pid.get()));
            refuse(stream, HelloRefusal::Malformed);
            return;
        }
    };
    if SUPPORTED.admits(hello.version) != Admission::Speaks {
        shared.log.warn(format_args!(
            "{} speaks version {}, which this host doesn't",
            peer.pid.get(),
            hello.version.get()
        ));
        refuse(
            stream,
            HelloRefusal::Version {
                supported: SUPPORTED,
            },
        );
        return;
    }
    if !shared.admits(hello.role) {
        shared.log.warn(format_args!(
            "{} was refused, the host serves its most connections",
            peer.pid.get()
        ));
        return;
    }
    if stream.set_read_timeout(None).is_err() {
        return;
    }
    let (Ok(writing), Ok(control)) = (stream.try_clone(), stream.try_clone()) else {
        return;
    };
    let client = shared.client();
    let (outbox, drain) = queue();
    let stall = shared.timing.write_stall;
    let writer = thread::Builder::new()
        .name("client writer".into())
        .spawn(move || write_loop(&drain, writing, stall));
    if writer.is_err() {
        return;
    }
    let joining = Joining {
        client,
        peer,
        role: hello.role,
        outbox,
        connection: control,
        pending: Vec::new(),
    };
    if inbox.send(Message::Join(joining)).is_err() {
        return;
    }
    let _gone = GoneGuard { client, inbox };
    read_commands(stream, client, shared, inbox);
}

/// Tells the core a client is gone however its reader thread ends, including
/// by a panic, so the core never keeps a seat for a connection nobody serves.
struct GoneGuard<'a> {
    client: agent_host::ClientRef,
    inbox: &'a SyncSender<Message>,
}

impl Drop for GoneGuard<'_> {
    fn drop(&mut self) {
        let _ = self.inbox.send(Message::Gone(self.client));
    }
}

fn read_commands(
    stream: &UnixStream,
    client: agent_host::ClientRef,
    shared: &Shared,
    inbox: &SyncSender<Message>,
) {
    let mut input = stream;
    loop {
        match read_frame::<Command>(&mut input) {
            Ok(Some(command)) => {
                let held = match &command {
                    Command::Input(bytes) | Command::Inject(bytes) => {
                        let wait = shared.timing.input_wait;
                        let Some(held) = shared.input.acquire(bytes.as_slice().len(), wait) else {
                            shared.log.warn(format_args!(
                                "client {} was closed: the program took none of its input for {wait:?}",
                                client.get()
                            ));
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                            return;
                        };
                        held
                    }
                    _ => Held::nothing(),
                };
                let sent = Message::Command {
                    client,
                    command,
                    held,
                };
                if inbox.send(sent).is_err() {
                    return;
                }
            }
            Ok(None) | Err(ReadError::Io(_) | ReadError::Cut) => return,
            Err(ReadError::Decode(e)) => {
                shared.log.warn(format_args!(
                    "client {} sent a frame that doesn't decode, so it was closed: {e}",
                    client.get()
                ));
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return;
            }
        }
    }
}

/// Writes what is queued until the queue is dropped or a write fails or
/// stalls for `stall`, then closes the connection, which ends the client's
/// reader too. A client that reads nothing is closed after `stall`, and a
/// snapshot it was sent gives up its permit.
fn write_loop(drain: &Drain, mut stream: UnixStream, stall: Duration) {
    let _ = stream.set_write_timeout(Some(stall));
    while let Some(outgoing) = drain.next() {
        let written = match outgoing {
            Outgoing::Frame(bytes) | Outgoing::Replay(bytes) => stream.write_all(&bytes),
            Outgoing::Snapshot(bytes, _permit) => write_snapshot(&mut stream, &bytes),
        };
        if written.is_err() {
            break;
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn write_snapshot(stream: &mut UnixStream, bytes: &[u8]) -> io::Result<()> {
    for chunk in Payload::split(bytes) {
        write_frame(stream, &HostFrame::Snapshot(chunk))?;
    }
    write_frame(stream, &HostFrame::SnapshotEnd)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use agent_host::ClientRef;

    use super::*;

    #[test]
    fn a_reader_thread_that_panics_still_tells_the_core_its_client_is_gone() {
        let (inbox, heard) = mpsc::sync_channel(4);
        let sender = inbox.clone();
        let panicked = thread::spawn(move || {
            let _gone = GoneGuard {
                client: ClientRef::new(7),
                inbox: &sender,
            };
            panic!("a reader that panics");
        })
        .join();
        assert!(panicked.is_err());
        assert!(matches!(
            heard.try_recv(),
            Ok(Message::Gone(client)) if client == ClientRef::new(7)
        ));
    }
}
