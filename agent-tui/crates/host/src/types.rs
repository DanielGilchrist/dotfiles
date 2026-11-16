use std::num::NonZeroU8;

use agent_core::activity::Pid;
use agent_core::size::TermSize;
use agent_core::theme::Rgb;

use crate::codec::MAX_FRAME;
use crate::version::VersionRange;

/// Leaves room in a frame for the tag and the fixed fields before the bytes.
pub const MAX_PAYLOAD: usize = MAX_FRAME as usize - 16;

/// The most entries a `Clients` frame carries.
pub const MAX_CLIENTS: usize = 1024;

/// Bytes that fit one frame, so a frame built from the types always encodes
/// under `MAX_FRAME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload(Vec<u8>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{len} bytes don't fit one frame")]
pub struct TooLarge {
    pub len: usize,
}

impl Payload {
    pub fn new(bytes: Vec<u8>) -> Result<Self, TooLarge> {
        if bytes.len() > MAX_PAYLOAD {
            return Err(TooLarge { len: bytes.len() });
        }
        Ok(Self(bytes))
    }

    /// Bytes of any length, cut into payloads that each fit a frame.
    pub fn split(bytes: &[u8]) -> impl Iterator<Item = Self> {
        bytes.chunks(MAX_PAYLOAD).map(|chunk| Self(chunk.to_vec()))
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

/// A connected client, numbered by the host. It is not a pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientRef(u32);

impl ClientRef {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// What a client attaches as, and the fields that only that role has. A
/// window and a preview send their terminal's size and colours, a watcher
/// has neither because it never sizes the session, and a control client
/// attaches to nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Window {
        take: Take,
        size: TermSize,
        colours: Option<Colours>,
    },
    /// Owns the session only while no window does, and never displaces one.
    Preview {
        size: TermSize,
        colours: Option<Colours>,
    },
    Watch {
        purpose: Purpose,
    },
    Control,
}

impl Role {
    /// What the client said its terminal's colours are, if it said.
    pub fn colours(self) -> Option<Colours> {
        match self {
            Self::Window { colours, .. } | Self::Preview { colours, .. } => colours,
            Self::Watch { .. } | Self::Control => None,
        }
    }

    /// `None` for a control client, which is behind no window or preview.
    pub fn purpose(self) -> Option<Purpose> {
        match self {
            Self::Window { .. } => Some(Purpose::Window),
            Self::Preview { .. } => Some(Purpose::Preview),
            Self::Watch { purpose } => Some(purpose),
            Self::Control => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    /// Owns the session even if another window does. The old owner becomes a
    /// watcher that can claim it back.
    Over,
    /// Owns the session only when nobody does, or a preview does. Otherwise
    /// it watches.
    Politely,
}

/// Who is behind a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Window,
    Preview,
    /// A read-only view that no one is behind, such as a phone. It is no
    /// window and no preview, so nothing counts it as either.
    Watch,
}

/// What a client may do now, decided by the host from its `Role` and who
/// else is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    Owner,
    Watcher,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientInfo {
    pub client: ClientRef,
    pub pid: Pid,
    /// `None` for a control client.
    pub purpose: Option<Purpose>,
    pub standing: Standing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientList(Vec<ClientInfo>);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{count} clients don't fit one listing")]
pub struct TooManyClients {
    pub count: usize,
}

impl ClientList {
    pub fn new(clients: Vec<ClientInfo>) -> Result<Self, TooManyClients> {
        if clients.len() > MAX_CLIENTS {
            return Err(TooManyClients {
                count: clients.len(),
            });
        }
        Ok(Self(clients))
    }

    pub fn as_slice(&self) -> &[ClientInfo] {
        &self.0
    }
}

/// The owner's terminal colours, for answering the app's OSC 10 and 11.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colours {
    pub foreground: Rgb,
    pub background: Rgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scrollback {
    Screen,
    Full,
}

/// Whether a snapshot carries styling. Plain text comes from the screen's
/// cells, so the gaps the program left with cursor moves stay as spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Styled,
    Plain,
}

/// How the harness ended. Unix exit codes fit a byte and signals have
/// numbers, so what can't be expressed that way is `Unknown`, never a
/// guessed code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Code(u8),
    Signal(NonZeroU8),
    Unknown,
}

/// Signals that have one number on every unix, by the text `strsignal`
/// gives for them on Linux. macOS adds the number (`Killed: 9`), which is
/// read instead.
const SIGNALS: [(&str, u8); 14] = [
    ("Hangup", 1),
    ("Interrupt", 2),
    ("Quit", 3),
    ("Illegal instruction", 4),
    ("Trace/breakpoint trap", 5),
    ("Trace/BPT trap", 5),
    ("Aborted", 6),
    ("Abort trap", 6),
    ("Floating point exception", 8),
    ("Killed", 9),
    ("Segmentation fault", 11),
    ("Broken pipe", 13),
    ("Alarm clock", 14),
    ("Terminated", 15),
];

impl ExitStatus {
    /// From what `portable_pty::ExitStatus` reports: its `exit_code()` and
    /// `signal()`, which is `strsignal` text, or `Signal N` when unnamed.
    pub fn from_pty(code: u32, signal: Option<&str>) -> Self {
        match signal {
            Some(name) => Self::from_signal_name(name),
            None => u8::try_from(code).map_or(Self::Unknown, Self::Code),
        }
    }

    fn from_signal_name(name: &str) -> Self {
        let named = SIGNALS
            .iter()
            .find(|(text, _)| *text == name)
            .map(|(_, number)| *number);
        let numbered = || {
            name.strip_prefix("Signal ")
                .or_else(|| name.rsplit_once(": ").map(|(_, digits)| digits))
                .and_then(|digits| digits.parse().ok())
        };
        named
            .or_else(numbered)
            .and_then(NonZeroU8::new)
            .map_or(Self::Unknown, Self::Signal)
    }
}

/// Why a host ended the hello, before any session traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloRefusal {
    Version { supported: VersionRange },
    Malformed,
    HelloTimeout,
}
