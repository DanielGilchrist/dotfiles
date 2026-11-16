use agent_core::size::TermSize;

use crate::frames::{Command, CommandKind};
use crate::types::{ClientRef, Payload, Scrollback, Standing, Style};

/// A command refused for the sender's standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forbidden {
    pub command: CommandKind,
}

/// What any standing may send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shared {
    Inject(Payload),
    Snapshot {
        scrollback: Scrollback,
        style: Style,
    },
    Clients,
    Kill,
    Bye,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerCall {
    Input(Payload),
    Resize(TermSize),
    Detach(ClientRef),
    Shared(Shared),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatcherCall {
    Claim(TermSize),
    Shared(Shared),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlCall {
    Detach(ClientRef),
    Shared(Shared),
}

/// A command the sender's standing may send, in a type that holds only the
/// calls that standing has, so the host's handler for a watcher has no
/// `Input` arm to get wrong. Only `Standing::admit` builds one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admitted {
    Owner(OwnerCall),
    Watcher(WatcherCall),
    Control(ControlCall),
}

impl Standing {
    pub fn admit(self, command: Command) -> Result<Admitted, Forbidden> {
        let kind = command.kind();
        let shared = |shared: Shared| match self {
            Self::Owner => Admitted::Owner(OwnerCall::Shared(shared)),
            Self::Watcher => Admitted::Watcher(WatcherCall::Shared(shared)),
            Self::Control => Admitted::Control(ControlCall::Shared(shared)),
        };
        match (self, command) {
            (_, Command::Inject(bytes)) => Ok(shared(Shared::Inject(bytes))),
            (_, Command::Snapshot { scrollback, style }) => {
                Ok(shared(Shared::Snapshot { scrollback, style }))
            }
            (_, Command::Clients) => Ok(shared(Shared::Clients)),
            (_, Command::Kill) => Ok(shared(Shared::Kill)),
            (_, Command::Bye) => Ok(shared(Shared::Bye)),
            (Self::Owner, Command::Input(bytes)) => Ok(Admitted::Owner(OwnerCall::Input(bytes))),
            (Self::Owner, Command::Resize(size)) => Ok(Admitted::Owner(OwnerCall::Resize(size))),
            (Self::Owner, Command::Detach(client)) => {
                Ok(Admitted::Owner(OwnerCall::Detach(client)))
            }
            (Self::Control, Command::Detach(client)) => {
                Ok(Admitted::Control(ControlCall::Detach(client)))
            }
            (Self::Watcher, Command::Claim(size)) => {
                Ok(Admitted::Watcher(WatcherCall::Claim(size)))
            }
            (Self::Owner, Command::Claim(_))
            | (Self::Watcher, Command::Input(_) | Command::Resize(_) | Command::Detach(_))
            | (Self::Control, Command::Input(_) | Command::Resize(_) | Command::Claim(_)) => {
                Err(Forbidden { command: kind })
            }
        }
    }
}
