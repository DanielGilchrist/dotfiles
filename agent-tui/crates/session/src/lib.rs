//! The native session host and its clients (design in docs/host.md).

mod answer;
mod client;
mod control;
mod daemon;
mod egress;
mod environment;
mod host;
mod ledger;
mod paths;
mod peer;
mod record;
mod screen;
mod seating;
mod spawn;

pub use client::{
    ClientError, Console, DetachKey, Ended, External, Options, Outer, Seat, Wiring, attach, run,
};
pub use control::{ANSWER_WITHIN, Control, ControlError};
pub use daemon::{DaemonError, detach_from_terminal};
pub use environment::{SESSION as SESSION_VAR, own_session};
pub use host::{HostConfig, HostError, Launch, Timing, serve};
pub use paths::{
    Creation, HostFiles, Runtime, RuntimeDir, RuntimeError, SocketPath, SocketPathTooLong, Unfit,
};
pub use peer::Foreign;
pub use record::{HostRecord, Liveness, LockError, RecordError, forget, probe};
pub use screen::ScrollbackCap;
pub use spawn::{SpawnError, spawn_host};

#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;
