use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;

use agent_core::activity::Pid;
use agent_core::size::TermSize;
use agent_host::ExitStatus;
use agent_term::{HostedCommand, HostedPty, spawn_hosted};
use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid as UnixPid;

use super::message::{Budget, Held, Message};

const READ_CHUNK: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Typed {
    Queued,
    /// The pty no longer takes input. The bytes are lost.
    Lost,
}

pub(super) struct Program {
    pty: HostedPty,
    input: Sender<(Vec<u8>, Held)>,
    pid: Pid,
}

impl Program {
    pub(super) fn resize(&self, size: TermSize) -> io::Result<()> {
        self.pty.resize(size)
    }

    /// Queues bytes for the program. The queue is bounded by the budget the
    /// bytes were counted in, which is released when they have been written.
    pub(super) fn type_in(&self, bytes: Vec<u8>, held: Held) -> Typed {
        match self.input.send((bytes, held)) {
            Ok(()) => Typed::Queued,
            Err(_) => Typed::Lost,
        }
    }

    /// Signals the program's process group: the program is a session leader,
    /// so what it started is in the group. Only called while the program
    /// hasn't been reported exited, since its group id may be reused after.
    pub(super) fn signal(&self, signal: Signal) -> nix::Result<()> {
        let group = i32::try_from(self.pid.get()).map_err(|_| Errno::EINVAL)?;
        match killpg(UnixPid::from_raw(group), signal) {
            Err(Errno::ESRCH) => Ok(()),
            other => other,
        }
    }
}

/// Starts the program and the threads that read its output, wait for it and
/// write its input. Each tells the core through `inbox`.
pub(super) fn start(
    command: &HostedCommand,
    size: TermSize,
    inbox: &mpsc::SyncSender<Message>,
    budget: &Arc<Budget>,
) -> io::Result<Program> {
    let agent_term::Hosted {
        pty,
        mut reader,
        mut writer,
        child,
    } = spawn_hosted(command, size)?;
    let pid = child.pid();

    let output = inbox.clone();
    thread::spawn(move || {
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if output.send(Message::Output(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = output.send(Message::OutputClosed);
    });

    let exits = inbox.clone();
    thread::spawn(move || {
        let status = child.wait().map_or(ExitStatus::Unknown, |exit| {
            ExitStatus::from_pty(exit.code, exit.signal.as_deref())
        });
        let _ = exits.send(Message::Exited(status));
    });

    let (input, queued) = mpsc::channel::<(Vec<u8>, Held)>();
    let budget = Arc::clone(budget);
    thread::spawn(move || {
        for (bytes, _held) in queued {
            if writer
                .write_all(&bytes)
                .and_then(|()| writer.flush())
                .is_err()
            {
                break;
            }
        }
        budget.close();
    });

    Ok(Program { pty, input, pid })
}
