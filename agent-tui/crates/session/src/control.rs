use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use agent_core::size::TermSize;
use agent_host::{
    ClientList, ClientRef, Command, CommandKind, Decoder, Frame, Greeting, Hello, HelloRefusal,
    HostFrame, PROTOCOL_VERSION, Payload, Progress, Role, Scrollback, Style, read_frame,
    write_frame,
};

use crate::paths::SocketPath;

/// How long a call waits for the host's answer unless the caller says.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("connecting to the host: {0}")]
    Connect(io::Error),
    #[error("the host refused the connection: {0:?}")]
    Refused(HelloRefusal),
    #[error("talking to the host: {0}")]
    Io(io::Error),
    #[error("the host closed the connection")]
    Closed,
    #[error("the host didn't answer within {0:?}")]
    Silent(Duration),
    #[error("the host doesn't allow {0:?} from a control client")]
    Forbidden(CommandKind),
}

/// A connection to a host that attaches to nothing: it is sent no screen and
/// no output, and may ask for the client list, type into the program, take
/// a snapshot, detach a client or stop the session.
pub struct Control {
    stream: UnixStream,
    decoder: Decoder<HostFrame>,
    size: TermSize,
    within: Duration,
}

impl Control {
    pub fn connect(socket: &SocketPath) -> Result<Self, ControlError> {
        Self::connect_within(socket, ANSWER_WITHIN)
    }

    /// `connect`, with every wait on the host (this one and each call's)
    /// bounded by `within`.
    pub fn connect_within(socket: &SocketPath, within: Duration) -> Result<Self, ControlError> {
        let mut stream = UnixStream::connect(socket.as_path()).map_err(ControlError::Connect)?;
        stream
            .set_read_timeout(Some(within))
            .map_err(ControlError::Io)?;
        stream
            .set_write_timeout(Some(within))
            .map_err(ControlError::Io)?;
        write_frame(
            &mut stream,
            &Hello {
                version: PROTOCOL_VERSION,
                role: Role::Control,
            },
        )
        .map_err(ControlError::Io)?;
        match read_frame::<Greeting>(&mut stream) {
            Ok(Some(Greeting::Welcome(welcome))) => Ok(Self {
                stream,
                decoder: Decoder::new(),
                size: welcome.size,
                within,
            }),
            Ok(Some(Greeting::Refused(refusal))) => Err(ControlError::Refused(refusal)),
            Ok(None) => Err(ControlError::Closed),
            Err(e) => Err(ControlError::Io(io::Error::other(e.to_string()))),
        }
    }

    pub fn size(&self) -> TermSize {
        self.size
    }

    pub fn clients(&mut self) -> Result<ClientList, ControlError> {
        self.send(&Command::Clients)?;
        self.until(CommandKind::Clients, |frame| match frame {
            HostFrame::Clients(list) => Ok(list),
            other => Err(other),
        })
    }

    /// Types `bytes` into the program as the host's own input, in as many
    /// frames as they need, and ends this connection once the host has
    /// answered them, so a refusal is read before it does.
    pub fn inject(mut self, bytes: &[u8]) -> Result<(), ControlError> {
        for chunk in Payload::split(bytes) {
            self.send(&Command::Inject(chunk))?;
        }
        self.settle()
    }

    /// The screen as styled text, one row per line, after the rows that
    /// scrolled off when `scrollback` is `Full`.
    pub fn snapshot(
        &mut self,
        scrollback: Scrollback,
        style: Style,
    ) -> Result<Vec<u8>, ControlError> {
        self.send(&Command::Snapshot { scrollback, style })?;
        let mut text = Vec::new();
        self.until(CommandKind::Snapshot, |frame| match frame {
            HostFrame::Snapshot(chunk) => {
                text.extend_from_slice(chunk.as_slice());
                Err(HostFrame::Snapshot(chunk))
            }
            HostFrame::SnapshotEnd => Ok(()),
            other => Err(other),
        })?;
        Ok(text)
    }

    /// Detaches `clients` and ends this connection, so a refusal of any of
    /// them is read before it does.
    pub fn detach(mut self, clients: &[ClientRef]) -> Result<(), ControlError> {
        for client in clients {
            self.send(&Command::Detach(*client))?;
        }
        self.settle()
    }

    /// Asks the host to stop the program. The host ends the connection once
    /// it has begun to exit, which is not an error here.
    pub fn kill(&mut self) -> Result<(), ControlError> {
        self.send(&Command::Kill)
    }

    fn send(&mut self, command: &Command) -> Result<(), ControlError> {
        self.stream
            .write_all(&command.encode())
            .map_err(ControlError::Io)
    }

    /// A `Bye` makes the host answer after everything sent before it, so a
    /// refusal of an earlier command is read before the connection ends.
    fn settle(&mut self) -> Result<(), ControlError> {
        self.send(&Command::Bye)?;
        let deadline = Instant::now() + self.within;
        loop {
            match self.next(deadline) {
                Ok(HostFrame::Forbidden { command }) if command != CommandKind::Bye => {
                    return Err(ControlError::Forbidden(command));
                }
                Ok(_) => {}
                Err(ControlError::Closed) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    fn until<T>(
        &mut self,
        asked: CommandKind,
        mut take: impl FnMut(HostFrame) -> Result<T, HostFrame>,
    ) -> Result<T, ControlError> {
        let deadline = Instant::now() + self.within;
        loop {
            match take(self.next(deadline)?) {
                Ok(done) => return Ok(done),
                Err(HostFrame::Forbidden { command }) if command == asked => {
                    return Err(ControlError::Forbidden(command));
                }
                Err(_) => {}
            }
        }
    }

    fn next(&mut self, deadline: Instant) -> Result<HostFrame, ControlError> {
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
                .ok_or(ControlError::Silent(self.within))?;
            let _ = self.stream.set_read_timeout(Some(left));
            let decoder = std::mem::take(&mut self.decoder);
            match decoder.read_from(&mut &self.stream) {
                Ok(Progress::Frame(frame, next)) => {
                    self.decoder = next;
                    return Ok(frame);
                }
                Ok(Progress::More(next) | Progress::Blocked(next)) => self.decoder = next,
                Ok(Progress::Closed) => return Err(ControlError::Closed),
                Err(e) => return Err(ControlError::Io(io::Error::other(e.to_string()))),
            }
        }
    }
}
