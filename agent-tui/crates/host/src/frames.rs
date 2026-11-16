use agent_core::size::TermSize;

use crate::types::{
    ClientInfo, ClientList, ClientRef, ExitStatus, HelloRefusal, Payload, Role, Scrollback,
    Standing, Style,
};
use crate::version::Version;

/// The first and only frame a client sends before a `Command`. It is its
/// own `Frame`, so a host reads it with a `Decoder<Hello>` and commands with
/// a `Decoder<Command>`, and neither can decode the other's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub version: Version,
    pub role: Role,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Input(Payload),
    Resize(TermSize),
    /// A watcher takes ownership and sizes the session to its terminal.
    Claim(TermSize),
    Detach(ClientRef),
    Inject(Payload),
    Snapshot {
        scrollback: Scrollback,
        style: Style,
    },
    Clients,
    Kill,
    Bye,
}

/// A `Command` without its fields, to name what was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    Input,
    Resize,
    Claim,
    Detach,
    Inject,
    Snapshot,
    Clients,
    Kill,
    Bye,
}

impl Command {
    pub fn kind(&self) -> CommandKind {
        match self {
            Self::Input(_) => CommandKind::Input,
            Self::Resize(_) => CommandKind::Resize,
            Self::Claim(_) => CommandKind::Claim,
            Self::Detach(_) => CommandKind::Detach,
            Self::Inject(_) => CommandKind::Inject,
            Self::Snapshot { .. } => CommandKind::Snapshot,
            Self::Clients => CommandKind::Clients,
            Self::Kill => CommandKind::Kill,
            Self::Bye => CommandKind::Bye,
        }
    }
}

/// The host's answer to a `Hello`. A `Welcome` to a window, preview or
/// watcher is followed by `Output` frames that rebuild the screen, then
/// live `Output`. A control client gets no screen and no `Output`, only
/// replies to its commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Greeting {
    Welcome(Welcome),
    Refused(HelloRefusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Welcome {
    pub version: Version,
    pub size: TermSize,
    pub standing: Standing,
    /// The client's own entry in `Clients`.
    pub you: ClientRef,
}

/// Frames after a `Greeting::Welcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostFrame {
    Output(Payload),
    /// The `Output` that follows is a fresh replay: the client drops its
    /// screen first.
    Resync,
    /// A chunk of the reply to `Snapshot`, ended by `SnapshotEnd`.
    Snapshot(Payload),
    SnapshotEnd,
    Clients(ClientList),
    /// A `Claim` succeeded.
    Promoted,
    Demoted {
        by: ClientInfo,
    },
    Detached {
        by: ClientInfo,
    },
    Exited(ExitStatus),
    Forbidden {
        command: CommandKind,
    },
}
