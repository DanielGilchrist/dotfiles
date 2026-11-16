//! The wire protocol between a session host and its clients.
//!
//! Frames are a `u32` big-endian length, a tag byte, then the fields. The
//! length counts the tag and the fields and is at most [`MAX_FRAME`]. Each
//! direction is read in two stages, each with its own closed frame type
//! decoded at the boundary:
//!
//! - client to host: [`Hello`], then [`Command`]s
//! - host to client: [`Greeting`], then [`HostFrame`]s
//!
//! A [`Decoder`] reads only what the frame in progress is missing, so
//! starting a `Decoder` of the second stage after the first stage's frame
//! loses no bytes.

mod admit;
mod codec;
mod frames;
mod types;
mod version;

#[cfg(test)]
mod tests;

pub use admit::{Admitted, ControlCall, Forbidden, OwnerCall, Shared, WatcherCall};
pub use codec::{
    DecodeError, Decoder, Frame, MAX_DIMENSION, MAX_FRAME, Progress, ReadError, read_frame,
    write_frame,
};
pub use frames::{Command, CommandKind, Greeting, Hello, HostFrame, Welcome};
pub use types::{
    ClientInfo, ClientList, ClientRef, Colours, ExitStatus, HelloRefusal, MAX_CLIENTS, MAX_PAYLOAD,
    Payload, Purpose, Role, Scrollback, Standing, Style, Take, TooLarge, TooManyClients,
};
pub use version::{Admission, PROTOCOL_VERSION, SUPPORTED, Version, VersionRange};
