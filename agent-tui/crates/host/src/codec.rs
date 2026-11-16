use std::io::{self, Read};
use std::marker::PhantomData;
use std::num::{NonZeroU8, NonZeroU16};

use agent_core::activity::Pid;
use agent_core::size::TermSize;
use agent_core::theme::Rgb;

use crate::frames::{Command, CommandKind, Greeting, Hello, HostFrame, Welcome};
use crate::types::{
    ClientInfo, ClientList, ClientRef, Colours, ExitStatus, HelloRefusal, Payload, Purpose, Role,
    Scrollback, Standing, Style, Take, TooLarge,
};
use crate::version::{Version, VersionRange};

/// The most bytes a frame's tag and fields take, announced in its length.
pub const MAX_FRAME: u32 = 1 << 20;

/// The most rows or columns a peer may state. A host allocates a grid of
/// that size eagerly (a 20000 by 20000 screen is tens of GB), and no real
/// terminal is larger than this.
pub const MAX_DIMENSION: u16 = 1000;

const HEADER: usize = 4;
const CLIENT_ENTRY: usize = 10;
const READ_CHUNK: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("a frame announced {announced} bytes, over the {MAX_FRAME} byte cap")]
    Oversized { announced: u32 },
    #[error("a payload of {len} bytes doesn't fit one frame")]
    PayloadTooLarge { len: usize },
    #[error("a frame announced no bytes, not even a tag")]
    Empty,
    #[error("unknown frame tag {0}")]
    UnknownTag(u8),
    #[error("a frame ended inside a field")]
    Truncated,
    #[error("a frame had {extra} bytes after its last field")]
    Trailing { extra: usize },
    #[error("{field} has no meaning for {value}")]
    Unknown { field: &'static str, value: u8 },
    #[error("{field} is zero")]
    Zero { field: &'static str },
    #[error("{field} is {value}, over the {MAX_DIMENSION} a terminal size may have")]
    TooLarge { field: &'static str, value: u16 },
    #[error("a listing announced {count} clients")]
    TooManyClients { count: usize },
    #[error("a version range ends before it starts")]
    InvertedRange,
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("the stream ended inside a frame")]
    Cut,
}

/// A frame that crosses the socket in one direction.
pub trait Frame: Sized {
    /// The tag and the fields, without the length.
    fn encode_body(&self, out: &mut Vec<u8>);

    /// `body` is the whole of one frame's tag and fields.
    fn decode_body(body: &[u8]) -> Result<Self, DecodeError>;

    fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        self.encode_body(&mut body);
        // The types cap every variable part, so a body fits `MAX_FRAME`. A
        // body that didn't would be refused by the peer's length check.
        let announced = u32::try_from(body.len()).unwrap_or(u32::MAX);
        let mut frame = Vec::with_capacity(HEADER + body.len());
        frame.extend_from_slice(&announced.to_be_bytes());
        frame.extend_from_slice(&body);
        frame
    }
}

pub fn write_frame<F: Frame>(out: &mut impl io::Write, frame: &F) -> io::Result<()> {
    out.write_all(&frame.encode())
}

/// Blocks until a frame is whole. `None` is a stream that ended between
/// frames, and a reader that would block is an error.
pub fn read_frame<F: Frame>(input: &mut impl Read) -> Result<Option<F>, ReadError> {
    let mut decoder = Decoder::new();
    loop {
        match decoder.read_from(input)? {
            Progress::Frame(frame, _) => return Ok(Some(frame)),
            Progress::More(next) => decoder = next,
            Progress::Blocked(_) => return Err(io::Error::from(io::ErrorKind::WouldBlock).into()),
            Progress::Closed => return Ok(None),
        }
    }
}

fn body_len(announced: u32) -> Result<usize, DecodeError> {
    if announced > MAX_FRAME {
        return Err(DecodeError::Oversized { announced });
    }
    match usize::try_from(announced) {
        Ok(0) | Err(_) => Err(DecodeError::Empty),
        Ok(len) => Ok(len),
    }
}

#[derive(Debug)]
enum Want {
    Header { bytes: [u8; HEADER], have: usize },
    Body { bytes: Vec<u8>, len: usize },
}

/// What one `Decoder::read_from` call found. The decoder comes back in every
/// variant but the end of the stream, and is consumed by an error, so a
/// stream that failed can't be read on.
#[derive(Debug)]
pub enum Progress<F> {
    Frame(F, Decoder<F>),
    /// Some bytes arrived. Ask again.
    More(Decoder<F>),
    /// A non-blocking reader has nothing now. Ask again when it is readable.
    Blocked(Decoder<F>),
    /// The stream ended between frames.
    Closed,
}

/// Reads frames of one type from a stream. Each call reads at most the bytes
/// the frame in progress is missing, so it never takes a byte of the next
/// frame, which stays for the next stage's decoder, and a header that lies
/// about its length reserves nothing: a body grows only with the bytes that
/// arrive.
#[derive(Debug)]
pub struct Decoder<F> {
    want: Want,
    frame: PhantomData<fn() -> F>,
}

impl<F: Frame> Default for Decoder<F> {
    fn default() -> Self {
        Self::new()
    }
}

enum Got {
    Bytes(usize),
    Blocked,
    Eof,
}

fn read_some(input: &mut impl Read, into: &mut [u8]) -> io::Result<Got> {
    loop {
        match input.read(into) {
            Ok(0) => return Ok(Got::Eof),
            Ok(n) => return Ok(Got::Bytes(n)),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(Got::Blocked),
            Err(e) => return Err(e),
        }
    }
}

impl<F: Frame> Decoder<F> {
    pub fn new() -> Self {
        Self::waiting(Want::Header {
            bytes: [0; HEADER],
            have: 0,
        })
    }

    fn waiting(want: Want) -> Self {
        Self {
            want,
            frame: PhantomData,
        }
    }

    pub fn read_from(self, input: &mut impl Read) -> Result<Progress<F>, ReadError> {
        match self.want {
            Want::Header { mut bytes, have } => match read_some(input, &mut bytes[have..])? {
                Got::Eof if have == 0 => Ok(Progress::Closed),
                Got::Eof => Err(ReadError::Cut),
                Got::Blocked => Ok(Progress::Blocked(Self::waiting(Want::Header {
                    bytes,
                    have,
                }))),
                Got::Bytes(n) => {
                    let have = have + n;
                    if have < HEADER {
                        return Ok(Progress::More(Self::waiting(Want::Header { bytes, have })));
                    }
                    let len = body_len(u32::from_be_bytes(bytes))?;
                    Ok(Progress::More(Self::waiting(Want::Body {
                        bytes: Vec::new(),
                        len,
                    })))
                }
            },
            Want::Body { mut bytes, len } => {
                let mut chunk = [0; READ_CHUNK];
                let missing = (len - bytes.len()).min(READ_CHUNK);
                match read_some(input, &mut chunk[..missing])? {
                    Got::Eof => Err(ReadError::Cut),
                    Got::Blocked => Ok(Progress::Blocked(Self::waiting(Want::Body { bytes, len }))),
                    Got::Bytes(n) => {
                        bytes.extend_from_slice(&chunk[..n]);
                        if bytes.len() < len {
                            return Ok(Progress::More(Self::waiting(Want::Body { bytes, len })));
                        }
                        Ok(Progress::Frame(F::decode_body(&bytes)?, Self::new()))
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        match &self.want {
            Want::Header { .. } => 0,
            Want::Body { bytes, .. } => bytes.capacity(),
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let (head, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or(DecodeError::Truncated)?;
        self.0 = rest;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.0)
    }

    fn payload(&mut self) -> Result<Payload, DecodeError> {
        Payload::new(self.rest().to_vec())
            .map_err(|TooLarge { len }| DecodeError::PayloadTooLarge { len })
    }

    fn size(&mut self) -> Result<TermSize, DecodeError> {
        let dimension = |raw: u16, field: &'static str| {
            if raw > MAX_DIMENSION {
                return Err(DecodeError::TooLarge { field, value: raw });
            }
            NonZeroU16::new(raw).ok_or(DecodeError::Zero { field })
        };
        let rows = dimension(self.u16()?, "rows")?;
        let cols = dimension(self.u16()?, "columns")?;
        Ok(TermSize { rows, cols })
    }

    fn version(&mut self) -> Result<Version, DecodeError> {
        Ok(Version::new(self.u16()?))
    }

    fn pid(&mut self) -> Result<Pid, DecodeError> {
        Pid::new(self.u32()?).ok_or(DecodeError::Zero { field: "pid" })
    }

    fn rgb(&mut self) -> Result<Rgb, DecodeError> {
        let [r, g, b] = self.array()?;
        Ok(Rgb { r, g, b })
    }

    fn colours(&mut self) -> Result<Option<Colours>, DecodeError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(Colours {
                foreground: self.rgb()?,
                background: self.rgb()?,
            })),
            other => Err(unknown("colours", other)),
        }
    }

    fn client(&mut self) -> Result<ClientInfo, DecodeError> {
        Ok(ClientInfo {
            client: ClientRef::new(self.u32()?),
            pid: self.pid()?,
            purpose: optional_purpose(self.u8()?)?,
            standing: standing(self.u8()?)?,
        })
    }

    fn finish(self) -> Result<(), DecodeError> {
        match self.0.len() {
            0 => Ok(()),
            extra => Err(DecodeError::Trailing { extra }),
        }
    }
}

fn unknown(field: &'static str, value: u8) -> DecodeError {
    DecodeError::Unknown { field, value }
}

fn purpose(code: u8) -> Result<Purpose, DecodeError> {
    match code {
        0 => Ok(Purpose::Window),
        1 => Ok(Purpose::Preview),
        2 => Ok(Purpose::Watch),
        other => Err(unknown("purpose", other)),
    }
}

fn optional_purpose(code: u8) -> Result<Option<Purpose>, DecodeError> {
    match code {
        NO_PURPOSE => Ok(None),
        other => purpose(other).map(Some),
    }
}

/// A control client is behind no window or preview.
const NO_PURPOSE: u8 = 3;

fn purpose_code(purpose: Purpose) -> u8 {
    match purpose {
        Purpose::Window => 0,
        Purpose::Preview => 1,
        Purpose::Watch => 2,
    }
}

fn standing(code: u8) -> Result<Standing, DecodeError> {
    match code {
        0 => Ok(Standing::Owner),
        1 => Ok(Standing::Watcher),
        2 => Ok(Standing::Control),
        other => Err(unknown("standing", other)),
    }
}

fn standing_code(standing: Standing) -> u8 {
    match standing {
        Standing::Owner => 0,
        Standing::Watcher => 1,
        Standing::Control => 2,
    }
}

fn put_size(out: &mut Vec<u8>, size: TermSize) {
    out.extend_from_slice(&size.rows.get().to_be_bytes());
    out.extend_from_slice(&size.cols.get().to_be_bytes());
}

fn put_client(out: &mut Vec<u8>, info: &ClientInfo) {
    out.extend_from_slice(&info.client.get().to_be_bytes());
    out.extend_from_slice(&info.pid.get().to_be_bytes());
    out.push(info.purpose.map_or(NO_PURPOSE, purpose_code));
    out.push(standing_code(info.standing));
}

fn put_colours(out: &mut Vec<u8>, colours: Option<Colours>) {
    let Some(colours) = colours else {
        out.push(0);
        return;
    };
    out.push(1);
    for rgb in [colours.foreground, colours.background] {
        out.extend_from_slice(&[rgb.r, rgb.g, rgb.b]);
    }
}

/// Out of the range of every `command_tag` and `host_tag`, so a stray `Hello`
/// after the first is an unknown tag to the host's command decoder.
mod hello_tag {
    pub const HELLO: u8 = 0x80;
}

mod role_code {
    pub const WINDOW: u8 = 0;
    pub const PREVIEW: u8 = 1;
    pub const WATCH: u8 = 2;
    pub const CONTROL: u8 = 3;
}

mod command_tag {
    pub const INPUT: u8 = 1;
    pub const INJECT: u8 = 2;
    pub const RESIZE: u8 = 3;
    pub const SNAPSHOT: u8 = 4;
    pub const CLIENTS: u8 = 5;
    pub const DETACH: u8 = 6;
    pub const KILL: u8 = 7;
    pub const BYE: u8 = 8;
    pub const CLAIM: u8 = 9;
}

mod greeting_tag {
    pub const WELCOME: u8 = 0x80;
    pub const REFUSED: u8 = 0x81;
}

mod host_tag {
    pub const OUTPUT: u8 = 1;
    pub const RESYNC: u8 = 2;
    pub const SNAPSHOT: u8 = 3;
    pub const SNAPSHOT_END: u8 = 4;
    pub const CLIENTS: u8 = 5;
    pub const PROMOTED: u8 = 6;
    pub const DEMOTED: u8 = 7;
    pub const DETACHED: u8 = 8;
    pub const EXITED: u8 = 9;
    pub const FORBIDDEN: u8 = 10;
}

impl Frame for Hello {
    fn encode_body(&self, out: &mut Vec<u8>) {
        out.push(hello_tag::HELLO);
        out.extend_from_slice(&self.version.get().to_be_bytes());
        match self.role {
            Role::Window {
                take,
                size,
                colours,
            } => {
                out.push(role_code::WINDOW);
                out.push(match take {
                    Take::Over => 0,
                    Take::Politely => 1,
                });
                put_size(out, size);
                put_colours(out, colours);
            }
            Role::Preview { size, colours } => {
                out.push(role_code::PREVIEW);
                put_size(out, size);
                put_colours(out, colours);
            }
            Role::Watch { purpose } => {
                out.push(role_code::WATCH);
                out.push(purpose_code(purpose));
            }
            Role::Control => out.push(role_code::CONTROL),
        }
    }

    fn decode_body(body: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = Reader(body);
        match reader.u8()? {
            hello_tag::HELLO => {}
            other => return Err(DecodeError::UnknownTag(other)),
        }
        let version = reader.version()?;
        let role = match reader.u8()? {
            role_code::WINDOW => Role::Window {
                take: match reader.u8()? {
                    0 => Take::Over,
                    1 => Take::Politely,
                    other => return Err(unknown("take", other)),
                },
                size: reader.size()?,
                colours: reader.colours()?,
            },
            role_code::PREVIEW => Role::Preview {
                size: reader.size()?,
                colours: reader.colours()?,
            },
            role_code::WATCH => Role::Watch {
                purpose: purpose(reader.u8()?)?,
            },
            role_code::CONTROL => Role::Control,
            other => return Err(unknown("role", other)),
        };
        reader.finish()?;
        Ok(Self { version, role })
    }
}

fn command_code(kind: CommandKind) -> u8 {
    match kind {
        CommandKind::Input => command_tag::INPUT,
        CommandKind::Inject => command_tag::INJECT,
        CommandKind::Resize => command_tag::RESIZE,
        CommandKind::Snapshot => command_tag::SNAPSHOT,
        CommandKind::Clients => command_tag::CLIENTS,
        CommandKind::Detach => command_tag::DETACH,
        CommandKind::Kill => command_tag::KILL,
        CommandKind::Bye => command_tag::BYE,
        CommandKind::Claim => command_tag::CLAIM,
    }
}

fn command_kind(code: u8) -> Result<CommandKind, DecodeError> {
    match code {
        command_tag::INPUT => Ok(CommandKind::Input),
        command_tag::INJECT => Ok(CommandKind::Inject),
        command_tag::RESIZE => Ok(CommandKind::Resize),
        command_tag::SNAPSHOT => Ok(CommandKind::Snapshot),
        command_tag::CLIENTS => Ok(CommandKind::Clients),
        command_tag::DETACH => Ok(CommandKind::Detach),
        command_tag::KILL => Ok(CommandKind::Kill),
        command_tag::BYE => Ok(CommandKind::Bye),
        command_tag::CLAIM => Ok(CommandKind::Claim),
        other => Err(unknown("command", other)),
    }
}

impl Frame for Command {
    fn encode_body(&self, out: &mut Vec<u8>) {
        out.push(command_code(self.kind()));
        match self {
            Self::Input(bytes) | Self::Inject(bytes) => out.extend_from_slice(bytes.as_slice()),
            Self::Resize(size) | Self::Claim(size) => put_size(out, *size),
            Self::Detach(client) => out.extend_from_slice(&client.get().to_be_bytes()),
            Self::Snapshot { scrollback, style } => {
                out.push(match scrollback {
                    Scrollback::Screen => 0,
                    Scrollback::Full => 1,
                });
                out.push(match style {
                    Style::Styled => 0,
                    Style::Plain => 1,
                });
            }
            Self::Clients | Self::Kill | Self::Bye => {}
        }
    }

    fn decode_body(body: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = Reader(body);
        let command = match reader.u8()? {
            command_tag::INPUT => Self::Input(reader.payload()?),
            command_tag::INJECT => Self::Inject(reader.payload()?),
            command_tag::RESIZE => Self::Resize(reader.size()?),
            command_tag::CLAIM => Self::Claim(reader.size()?),
            command_tag::DETACH => Self::Detach(ClientRef::new(reader.u32()?)),
            command_tag::SNAPSHOT => Self::Snapshot {
                scrollback: match reader.u8()? {
                    0 => Scrollback::Screen,
                    1 => Scrollback::Full,
                    other => return Err(unknown("scrollback", other)),
                },
                style: match reader.u8()? {
                    0 => Style::Styled,
                    1 => Style::Plain,
                    other => return Err(unknown("style", other)),
                },
            },
            command_tag::CLIENTS => Self::Clients,
            command_tag::KILL => Self::Kill,
            command_tag::BYE => Self::Bye,
            other => return Err(DecodeError::UnknownTag(other)),
        };
        reader.finish()?;
        Ok(command)
    }
}

impl Frame for Greeting {
    fn encode_body(&self, out: &mut Vec<u8>) {
        match self {
            Self::Welcome(welcome) => {
                out.push(greeting_tag::WELCOME);
                out.extend_from_slice(&welcome.version.get().to_be_bytes());
                put_size(out, welcome.size);
                out.push(standing_code(welcome.standing));
                out.extend_from_slice(&welcome.you.get().to_be_bytes());
            }
            Self::Refused(refusal) => {
                out.push(greeting_tag::REFUSED);
                match refusal {
                    HelloRefusal::Version { supported } => {
                        out.push(0);
                        out.extend_from_slice(&supported.min().get().to_be_bytes());
                        out.extend_from_slice(&supported.max().get().to_be_bytes());
                    }
                    HelloRefusal::Malformed => out.push(1),
                    HelloRefusal::HelloTimeout => out.push(2),
                }
            }
        }
    }

    fn decode_body(body: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = Reader(body);
        let greeting = match reader.u8()? {
            greeting_tag::WELCOME => Self::Welcome(Welcome {
                version: reader.version()?,
                size: reader.size()?,
                standing: standing(reader.u8()?)?,
                you: ClientRef::new(reader.u32()?),
            }),
            greeting_tag::REFUSED => Self::Refused(match reader.u8()? {
                0 => {
                    let min = reader.version()?;
                    let max = reader.version()?;
                    HelloRefusal::Version {
                        supported: VersionRange::new(min, max).ok_or(DecodeError::InvertedRange)?,
                    }
                }
                1 => HelloRefusal::Malformed,
                2 => HelloRefusal::HelloTimeout,
                other => return Err(unknown("refusal", other)),
            }),
            other => return Err(DecodeError::UnknownTag(other)),
        };
        reader.finish()?;
        Ok(greeting)
    }
}

impl Frame for HostFrame {
    fn encode_body(&self, out: &mut Vec<u8>) {
        match self {
            Self::Output(bytes) => {
                out.push(host_tag::OUTPUT);
                out.extend_from_slice(bytes.as_slice());
            }
            Self::Resync => out.push(host_tag::RESYNC),
            Self::Snapshot(bytes) => {
                out.push(host_tag::SNAPSHOT);
                out.extend_from_slice(bytes.as_slice());
            }
            Self::SnapshotEnd => out.push(host_tag::SNAPSHOT_END),
            Self::Clients(list) => {
                out.push(host_tag::CLIENTS);
                for info in list.as_slice() {
                    put_client(out, info);
                }
            }
            Self::Promoted => out.push(host_tag::PROMOTED),
            Self::Demoted { by } => {
                out.push(host_tag::DEMOTED);
                put_client(out, by);
            }
            Self::Detached { by } => {
                out.push(host_tag::DETACHED);
                put_client(out, by);
            }
            Self::Exited(status) => {
                out.push(host_tag::EXITED);
                match status {
                    ExitStatus::Code(code) => out.extend_from_slice(&[0, *code]),
                    ExitStatus::Signal(signal) => out.extend_from_slice(&[1, signal.get()]),
                    ExitStatus::Unknown => out.push(2),
                }
            }
            Self::Forbidden { command } => {
                out.push(host_tag::FORBIDDEN);
                out.push(command_code(*command));
            }
        }
    }

    fn decode_body(body: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = Reader(body);
        let frame = match reader.u8()? {
            host_tag::OUTPUT => Self::Output(reader.payload()?),
            host_tag::RESYNC => Self::Resync,
            host_tag::SNAPSHOT => Self::Snapshot(reader.payload()?),
            host_tag::SNAPSHOT_END => Self::SnapshotEnd,
            host_tag::CLIENTS => Self::Clients(decode_clients(&mut reader)?),
            host_tag::PROMOTED => Self::Promoted,
            host_tag::DEMOTED => Self::Demoted {
                by: reader.client()?,
            },
            host_tag::DETACHED => Self::Detached {
                by: reader.client()?,
            },
            host_tag::EXITED => Self::Exited(match reader.u8()? {
                0 => ExitStatus::Code(reader.u8()?),
                1 => ExitStatus::Signal(
                    NonZeroU8::new(reader.u8()?).ok_or(DecodeError::Zero { field: "signal" })?,
                ),
                2 => ExitStatus::Unknown,
                other => return Err(unknown("exit status", other)),
            }),
            host_tag::FORBIDDEN => Self::Forbidden {
                command: command_kind(reader.u8()?)?,
            },
            other => return Err(DecodeError::UnknownTag(other)),
        };
        reader.finish()?;
        Ok(frame)
    }
}

/// Entries run to the end of the body, so there is no announced count to
/// trust: what is collected is bounded by the body, which is bounded by
/// `MAX_FRAME`.
fn decode_clients(reader: &mut Reader) -> Result<ClientList, DecodeError> {
    if !reader.0.len().is_multiple_of(CLIENT_ENTRY) {
        return Err(DecodeError::Truncated);
    }
    let mut clients = Vec::new();
    while !reader.0.is_empty() {
        clients.push(reader.client()?);
    }
    ClientList::new(clients).map_err(|e| DecodeError::TooManyClients { count: e.count })
}
