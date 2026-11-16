use std::collections::VecDeque;
use std::fmt::Debug;
use std::io::{self, Cursor, Read};
use std::num::{NonZeroU8, NonZeroU16};

use agent_core::activity::Pid;
use agent_core::size::TermSize;
use agent_core::theme::Rgb;

use crate::*;

struct Prng(u64);

impl Prng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap_or(0)
    }

    fn byte(&mut self) -> u8 {
        self.next().to_le_bytes()[0]
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.byte()).collect()
    }
}

/// A reader with bytes queued, at most `per_read` of them per call. When
/// empty it would block, or ends if `eof` is set.
struct Pipe {
    data: VecDeque<u8>,
    per_read: usize,
    eof: bool,
}

impl Pipe {
    fn new(bytes: &[u8], per_read: usize, eof: bool) -> Self {
        Self {
            data: bytes.iter().copied().collect(),
            per_read,
            eof,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.data.extend(bytes);
    }

    fn left(&self) -> usize {
        self.data.len()
    }
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.data.is_empty() {
            return if self.eof {
                Ok(0)
            } else {
                Err(io::ErrorKind::WouldBlock.into())
            };
        }
        let n = buf.len().min(self.per_read).min(self.data.len());
        for slot in &mut buf[..n] {
            *slot = self.data.pop_front().unwrap_or_default();
        }
        Ok(n)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum End {
    Closed,
    Blocked,
    Cut,
    Refused(DecodeError),
}

/// Reads until the pipe blocks or ends, handing back the decoder.
fn drain<F: Frame>(mut decoder: Decoder<F>, pipe: &mut Pipe) -> (Vec<F>, End, Option<Decoder<F>>) {
    let mut frames = Vec::new();
    loop {
        match decoder.read_from(pipe) {
            Ok(Progress::Frame(frame, next)) => {
                frames.push(frame);
                decoder = next;
            }
            Ok(Progress::More(next)) => decoder = next,
            Ok(Progress::Blocked(next)) => return (frames, End::Blocked, Some(next)),
            Ok(Progress::Closed) => return (frames, End::Closed, None),
            Err(ReadError::Cut) => return (frames, End::Cut, None),
            Err(ReadError::Decode(error)) => return (frames, End::Refused(error), None),
            Err(ReadError::Io(error)) => panic!("a pipe never fails: {error}"),
        }
    }
}

fn decode_stream<F: Frame>(bytes: &[u8], per_read: usize) -> (Vec<F>, End) {
    let (frames, end, _) = drain(Decoder::<F>::new(), &mut Pipe::new(bytes, per_read, true));
    (frames, end)
}

fn size(rows: u16, cols: u16) -> TermSize {
    TermSize {
        rows: NonZeroU16::new(rows).expect("the fixture is nonzero"),
        cols: NonZeroU16::new(cols).expect("the fixture is nonzero"),
    }
}

fn payload(bytes: &[u8]) -> Payload {
    Payload::new(bytes.to_vec()).expect("the fixture fits a frame")
}

fn info(client: u32, pid: u32, purpose: Option<Purpose>, standing: Standing) -> ClientInfo {
    ClientInfo {
        client: ClientRef::new(client),
        pid: Pid::new(pid).expect("the fixture is nonzero"),
        purpose,
        standing,
    }
}

fn colours() -> Colours {
    Colours {
        foreground: Rgb { r: 1, g: 2, b: 3 },
        background: Rgb {
            r: 250,
            g: 251,
            b: 252,
        },
    }
}

fn hellos() -> Vec<Hello> {
    let mut roles = vec![Role::Control];
    for take in [Take::Over, Take::Politely] {
        for colours in [None, Some(colours())] {
            roles.push(Role::Window {
                take,
                size: size(24, 80),
                colours,
            });
        }
    }
    for colours in [None, Some(colours())] {
        roles.push(Role::Preview {
            size: size(crate::MAX_DIMENSION, 1),
            colours,
        });
    }
    for purpose in [Purpose::Window, Purpose::Preview, Purpose::Watch] {
        roles.push(Role::Watch { purpose });
    }
    roles
        .into_iter()
        .map(|role| Hello {
            version: PROTOCOL_VERSION,
            role,
        })
        .collect()
}

fn commands() -> Vec<Command> {
    vec![
        Command::Input(payload(b"")),
        Command::Input(payload(b"ls\r\x1b[A")),
        Command::Inject(payload(b"hello\n")),
        Command::Resize(size(1, 1)),
        Command::Resize(size(crate::MAX_DIMENSION, crate::MAX_DIMENSION)),
        Command::Claim(size(50, 200)),
        Command::Detach(ClientRef::new(u32::MAX)),
        Command::Snapshot {
            scrollback: Scrollback::Screen,
            style: Style::Styled,
        },
        Command::Snapshot {
            scrollback: Scrollback::Full,
            style: Style::Styled,
        },
        Command::Snapshot {
            scrollback: Scrollback::Screen,
            style: Style::Plain,
        },
        Command::Snapshot {
            scrollback: Scrollback::Full,
            style: Style::Plain,
        },
        Command::Clients,
        Command::Kill,
        Command::Bye,
    ]
}

fn greetings() -> Vec<Greeting> {
    let supported = VersionRange::new(Version::new(1), Version::new(3)).expect("ordered");
    let mut greetings = vec![
        Greeting::Refused(HelloRefusal::Version { supported }),
        Greeting::Refused(HelloRefusal::Malformed),
        Greeting::Refused(HelloRefusal::HelloTimeout),
    ];
    for standing in [Standing::Owner, Standing::Watcher, Standing::Control] {
        greetings.push(Greeting::Welcome(Welcome {
            version: PROTOCOL_VERSION,
            size: size(50, 200),
            standing,
            you: ClientRef::new(7),
        }));
    }
    greetings
}

fn host_frames() -> Vec<HostFrame> {
    let owner = info(9, 77, Some(Purpose::Window), Standing::Owner);
    vec![
        HostFrame::Output(payload(b"")),
        HostFrame::Output(payload(&[0, 255, 10, 13])),
        HostFrame::Output(Payload::new(vec![7; MAX_PAYLOAD]).expect("the cap fits")),
        HostFrame::Resync,
        HostFrame::Snapshot(payload(b"\x1b[H")),
        HostFrame::SnapshotEnd,
        HostFrame::Clients(ClientList::new(Vec::new()).expect("empty fits")),
        HostFrame::Clients(
            ClientList::new(vec![
                info(1, 100, Some(Purpose::Window), Standing::Owner),
                info(2, 4_000_000_000, Some(Purpose::Preview), Standing::Watcher),
                info(3, 1, None, Standing::Control),
            ])
            .expect("three fit"),
        ),
        HostFrame::Promoted,
        HostFrame::Demoted { by: owner },
        HostFrame::Detached { by: owner },
        HostFrame::Exited(ExitStatus::Code(0)),
        HostFrame::Exited(ExitStatus::Code(255)),
        HostFrame::Exited(ExitStatus::Signal(
            NonZeroU8::new(9).expect("the fixture is nonzero"),
        )),
        HostFrame::Exited(ExitStatus::Unknown),
        HostFrame::Forbidden {
            command: CommandKind::Input,
        },
        HostFrame::Forbidden {
            command: CommandKind::Claim,
        },
    ]
}

fn raw(body: &[u8]) -> Vec<u8> {
    let mut frame = u32::try_from(body.len())
        .expect("the fixture is small")
        .to_be_bytes()
        .to_vec();
    frame.extend_from_slice(body);
    frame
}

fn round_trips<F: Frame + PartialEq + Debug>(frames: Vec<F>) {
    for frame in frames {
        let (decoded, end) = decode_stream::<F>(&frame.encode(), 7);
        assert_eq!((decoded, end), (vec![frame], End::Closed));
    }
}

#[test]
fn every_frame_round_trips() {
    round_trips(hellos());
    round_trips(commands());
    round_trips(greetings());
    round_trips(host_frames());
}

#[test]
fn blocking_helpers_round_trip_and_report_a_clean_end() {
    let mut wire = Vec::new();
    for command in commands() {
        write_frame(&mut wire, &command).expect("a Vec takes any write");
    }
    let mut input = Cursor::new(wire);
    for expected in commands() {
        let read = read_frame::<Command>(&mut input).expect("the frame is whole");
        assert_eq!(read, Some(expected));
    }
    assert!(
        read_frame::<Command>(&mut input)
            .expect("a clean end")
            .is_none()
    );
}

#[test]
fn a_blocking_read_of_a_stream_cut_inside_a_frame_is_an_error_at_every_byte() {
    for command in commands() {
        let bytes = command.encode();
        for cut in 1..bytes.len() {
            let mut input = Cursor::new(&bytes[..cut]);
            assert!(
                matches!(read_frame::<Command>(&mut input), Err(ReadError::Cut)),
                "{command:?} cut at {cut}"
            );
        }
    }
}

#[test]
fn a_blocking_read_that_would_block_is_an_error_not_a_hang() {
    let mut pipe = Pipe::new(&[0, 0], 1, false);
    assert!(matches!(
        read_frame::<Command>(&mut pipe),
        Err(ReadError::Io(e)) if e.kind() == io::ErrorKind::WouldBlock
    ));
}

fn blocks_on_every_prefix<F: Frame + PartialEq + Debug>(frames: Vec<F>) {
    for frame in frames {
        let bytes = frame.encode();
        for cut in 0..bytes.len() {
            let mut pipe = Pipe::new(&bytes[..cut], 3, false);
            let (early, end, decoder) = drain(Decoder::<F>::new(), &mut pipe);
            assert_eq!((early.len(), end), (0, End::Blocked), "{frame:?} at {cut}");
            pipe.push(&bytes[cut..]);
            let (done, _, _) = drain(decoder.expect("blocked keeps its decoder"), &mut pipe);
            assert_eq!(done, vec![frame_clone(&bytes)], "{frame:?} at {cut}");
        }
    }
}

fn frame_clone<F: Frame>(bytes: &[u8]) -> F {
    F::decode_body(&bytes[HEADER_LEN..]).expect("the frame is whole")
}

const HEADER_LEN: usize = 4;

#[test]
fn a_prefix_of_any_frame_waits_for_more_until_it_is_whole() {
    blocks_on_every_prefix(hellos());
    blocks_on_every_prefix(commands());
    blocks_on_every_prefix(greetings());
    let small = |frame: &HostFrame| frame.encode().len() < 4096;
    blocks_on_every_prefix(host_frames().into_iter().filter(small).collect());
}

#[test]
fn chunking_never_changes_what_is_decoded() {
    let wire: Vec<u8> = commands().iter().flat_map(Frame::encode).collect();
    let whole = decode_stream::<Command>(&wire, usize::MAX);
    assert_eq!(whole, (commands(), End::Closed));
    for per_read in [1, 2, 3, 5, 11, 64] {
        assert_eq!(decode_stream::<Command>(&wire, per_read), whole);
    }
    let host: Vec<u8> = host_frames().iter().flat_map(Frame::encode).collect();
    let whole = decode_stream::<HostFrame>(&host, usize::MAX);
    assert_eq!(whole, (host_frames(), End::Closed));
    for per_read in [1, 7, 4096, 5000] {
        assert_eq!(decode_stream::<HostFrame>(&host, per_read), whole);
    }
}

#[test]
fn a_decoder_takes_only_the_bytes_of_the_frame_in_progress() {
    let hello = hellos()[0];
    let next = Command::Kill.encode();
    let mut pipe = Pipe::new(&[hello.encode(), next.clone()].concat(), usize::MAX, true);
    let mut decoder = Decoder::<Hello>::new();
    let found = loop {
        match decoder
            .read_from(&mut pipe)
            .expect("the hello is well formed")
        {
            Progress::Frame(found, _) => break found,
            Progress::More(again) => decoder = again,
            other => panic!("unexpected {other:?}"),
        }
    };
    assert_eq!(found, hello);
    assert_eq!(pipe.left(), next.len());
    let (commands, end, _) = drain(Decoder::<Command>::new(), &mut pipe);
    assert_eq!((commands, end), (vec![Command::Kill], End::Closed));
}

#[test]
fn a_header_announcing_the_cap_reserves_nothing_and_grows_only_with_what_arrives() {
    let mut pipe = Pipe::new(&MAX_FRAME.to_be_bytes(), usize::MAX, false);
    pipe.push(&[1; 10]);
    let (frames, end, decoder) = drain(Decoder::<Command>::new(), &mut pipe);
    assert_eq!((frames.len(), end), (0, End::Blocked));
    let capacity = decoder.expect("blocked keeps its decoder").capacity();
    assert!(
        (10..64).contains(&capacity),
        "reserved {capacity} bytes for ten"
    );
}

#[test]
fn a_decoder_gives_back_its_memory_after_a_large_frame() {
    let frame = HostFrame::Output(Payload::new(vec![1; MAX_PAYLOAD]).expect("the cap fits"));
    let mut pipe = Pipe::new(&frame.encode(), usize::MAX, false);
    let (frames, _, decoder) = drain(Decoder::<HostFrame>::new(), &mut pipe);
    assert_eq!(frames, vec![frame]);
    assert_eq!(decoder.expect("blocked keeps its decoder").capacity(), 0);
}

#[test]
fn an_oversized_length_is_refused_after_four_bytes_and_the_rest_is_left_unread() {
    for announced in [MAX_FRAME + 1, u32::MAX] {
        let mut pipe = Pipe::new(&announced.to_be_bytes(), usize::MAX, false);
        pipe.push(&[9; 100]);
        let (frames, end, decoder) = drain(Decoder::<Command>::new(), &mut pipe);
        assert_eq!(
            (frames.len(), end),
            (0, End::Refused(DecodeError::Oversized { announced }))
        );
        assert!(decoder.is_none());
        assert_eq!(pipe.left(), 100);
    }
}

#[test]
fn a_blocking_read_refuses_an_oversized_length_without_reading_the_body() {
    let mut input = Cursor::new(u32::MAX.to_be_bytes().to_vec());
    assert!(matches!(
        read_frame::<HostFrame>(&mut input),
        Err(ReadError::Decode(DecodeError::Oversized {
            announced: u32::MAX
        }))
    ));
}

#[test]
fn a_payload_that_fills_a_body_at_the_cap_is_too_large_and_one_byte_over_is_oversized() {
    let mut body = vec![1];
    body.resize(MAX_FRAME as usize, 0);
    assert_eq!(
        decode_stream::<Command>(&raw(&body), 4096).1,
        End::Refused(DecodeError::PayloadTooLarge {
            len: MAX_FRAME as usize - 1
        })
    );
    body.push(0);
    assert_eq!(
        decode_stream::<Command>(&raw(&body), 4096).1,
        End::Refused(DecodeError::Oversized {
            announced: MAX_FRAME + 1
        })
    );
}

#[test]
fn a_zero_length_frame_is_refused() {
    assert_eq!(
        decode_stream::<Command>(&raw(&[]), 4).1,
        End::Refused(DecodeError::Empty)
    );
}

#[test]
fn an_unknown_tag_is_refused_in_every_stage() {
    let refused = |end| End::Refused(end);
    for tag in [0, 10, 0x80, 200] {
        assert_eq!(
            decode_stream::<Command>(&raw(&[tag]), 4).1,
            refused(DecodeError::UnknownTag(tag))
        );
    }
    for tag in [0, 1, 9, 0x81] {
        assert_eq!(
            decode_stream::<Hello>(&raw(&[tag]), 4).1,
            refused(DecodeError::UnknownTag(tag))
        );
    }
    for tag in [0, 1, 2, 255] {
        assert_eq!(
            decode_stream::<Greeting>(&raw(&[tag]), 4).1,
            refused(DecodeError::UnknownTag(tag))
        );
    }
    for tag in [0, 11, 0x80, 255] {
        assert_eq!(
            decode_stream::<HostFrame>(&raw(&[tag]), 4).1,
            refused(DecodeError::UnknownTag(tag))
        );
    }
}

#[test]
fn a_second_hello_is_not_a_command_and_a_command_is_not_a_hello() {
    assert_eq!(
        decode_stream::<Command>(&hellos()[0].encode(), 4).1,
        End::Refused(DecodeError::UnknownTag(0x80))
    );
    assert_eq!(
        decode_stream::<Hello>(&Command::Bye.encode(), 4).1,
        End::Refused(DecodeError::UnknownTag(8))
    );
    let welcome = greetings().pop().expect("greetings");
    assert_eq!(
        decode_stream::<HostFrame>(&welcome.encode(), 4).1,
        End::Refused(DecodeError::UnknownTag(0x80))
    );
}

struct Body {
    name: String,
    bytes: Vec<u8>,
    decode: fn(&[u8]) -> Result<(), DecodeError>,
}

fn fixed_size_bodies() -> Vec<Body> {
    fn body<F: Frame + Debug>(frame: &F) -> Body {
        Body {
            name: format!("{frame:?}"),
            bytes: frame.encode()[HEADER_LEN..].to_vec(),
            decode: |body| F::decode_body(body).map(|_| ()),
        }
    }
    let mut bodies = Vec::new();
    bodies.extend(hellos().iter().map(body));
    bodies.extend(
        commands()
            .iter()
            .filter(|c| !matches!(c, Command::Input(_) | Command::Inject(_)))
            .map(body),
    );
    bodies.extend(greetings().iter().map(body));
    bodies.extend(
        host_frames()
            .iter()
            .filter(|f| {
                !matches!(
                    f,
                    HostFrame::Output(_) | HostFrame::Snapshot(_) | HostFrame::Clients(_)
                )
            })
            .map(body),
    );
    bodies
}

#[test]
fn a_body_that_ends_inside_a_field_is_truncated_for_every_fixed_size_frame() {
    for Body {
        name,
        bytes: body,
        decode,
    } in fixed_size_bodies()
    {
        for keep in 1..body.len() {
            assert_eq!(
                decode(&body[..keep]),
                Err(DecodeError::Truncated),
                "{name} kept {keep}"
            );
        }
    }
}

#[test]
fn bytes_after_the_last_field_are_refused_for_every_fixed_size_frame() {
    for Body {
        name,
        bytes: body,
        decode,
    } in fixed_size_bodies()
    {
        let mut longer = body;
        longer.extend_from_slice(&[1, 2]);
        assert_eq!(
            decode(&longer),
            Err(DecodeError::Trailing { extra: 2 }),
            "{name}"
        );
    }
}

#[test]
fn zero_dimensions_unknown_codes_and_zero_ids_are_refused() {
    let decode = |body: &[u8]| decode_stream::<Command>(&raw(body), 4).1;
    assert_eq!(
        decode(&[3, 0, 0, 0, 80]),
        End::Refused(DecodeError::Zero { field: "rows" })
    );
    assert_eq!(
        decode(&[9, 0, 24, 0, 0]),
        End::Refused(DecodeError::Zero { field: "columns" })
    );
    assert_eq!(
        decode(&[3, 0x03, 0xe9, 0, 80]),
        End::Refused(DecodeError::TooLarge {
            field: "rows",
            value: 1001
        })
    );
    assert_eq!(
        decode(&[9, 0, 24, 0x4e, 0x20]),
        End::Refused(DecodeError::TooLarge {
            field: "columns",
            value: 20000
        })
    );
    let (frames, _) = decode_stream::<Command>(&raw(&[3, 0x03, 0xe8, 0x03, 0xe8]), 4);
    assert_eq!(frames, [Command::Resize(size(1000, 1000))]);
    assert_eq!(
        decode(&[4, 2]),
        End::Refused(DecodeError::Unknown {
            field: "scrollback",
            value: 2
        })
    );
    let hello = |body: &[u8]| decode_stream::<Hello>(&raw(body), 4).1;
    assert_eq!(
        hello(&[0x80, 0, 1, 4]),
        End::Refused(DecodeError::Unknown {
            field: "role",
            value: 4
        })
    );
    assert_eq!(
        hello(&[0x80, 0, 1, 0, 7]),
        End::Refused(DecodeError::Unknown {
            field: "take",
            value: 7
        })
    );
    assert_eq!(
        hello(&[0x80, 0, 1, 2, 5]),
        End::Refused(DecodeError::Unknown {
            field: "purpose",
            value: 5
        })
    );
    assert_eq!(
        hello(&[0x80, 0, 1, 1, 0, 24, 0, 80, 3]),
        End::Refused(DecodeError::Unknown {
            field: "colours",
            value: 3
        })
    );
    let host = |body: &[u8]| decode_stream::<HostFrame>(&raw(body), 4).1;
    assert_eq!(
        host(&[9, 1, 0]),
        End::Refused(DecodeError::Zero { field: "signal" })
    );
    assert_eq!(
        host(&[7, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]),
        End::Refused(DecodeError::Zero { field: "pid" })
    );
    assert_eq!(
        host(&[10, 99]),
        End::Refused(DecodeError::Unknown {
            field: "command",
            value: 99
        })
    );
    assert_eq!(
        decode_stream::<Greeting>(&raw(&[0x81, 0, 0, 3, 0, 2]), 4).1,
        End::Refused(DecodeError::InvertedRange)
    );
}

fn entry(client: u32) -> Vec<u8> {
    let mut bytes = client.to_be_bytes().to_vec();
    bytes.extend_from_slice(&[0, 0, 0, 1, 0, 0]);
    bytes
}

fn clients_body(count: usize) -> Vec<u8> {
    let mut body = vec![5];
    for client in 0..count {
        body.extend(entry(u32::try_from(client).expect("the fixture is small")));
    }
    body
}

#[test]
fn a_client_listing_over_the_limit_is_refused_and_one_at_it_is_read() {
    let at_limit = decode_stream::<HostFrame>(&raw(&clients_body(MAX_CLIENTS)), 4096);
    assert!(matches!(
        at_limit.0.as_slice(),
        [HostFrame::Clients(list)] if list.as_slice().len() == MAX_CLIENTS
    ));
    assert_eq!(
        decode_stream::<HostFrame>(&raw(&clients_body(MAX_CLIENTS + 1)), 4096).1,
        End::Refused(DecodeError::TooManyClients {
            count: MAX_CLIENTS + 1
        })
    );
}

#[test]
fn a_client_listing_whose_entries_are_cut_is_truncated() {
    let mut body = clients_body(2);
    body.pop();
    assert_eq!(
        decode_stream::<HostFrame>(&raw(&body), 4).1,
        End::Refused(DecodeError::Truncated)
    );
    assert_eq!(
        decode_stream::<HostFrame>(&raw(&[5, 1, 2, 3]), 4).1,
        End::Refused(DecodeError::Truncated)
    );
}

#[test]
fn a_payload_over_the_frame_budget_is_refused_and_splits_into_frames_that_fit() {
    assert!(Payload::new(vec![0; MAX_PAYLOAD + 1]).is_err());
    assert!(Payload::new(vec![0; MAX_PAYLOAD]).is_ok());
    let bytes = vec![5; MAX_PAYLOAD * 2 + 3];
    let parts: Vec<_> = Payload::split(&bytes).collect();
    assert_eq!(parts.len(), 3);
    for part in parts {
        assert!(HostFrame::Output(part).encode().len() <= 4 + MAX_FRAME as usize);
    }
}

#[test]
fn random_input_never_panics_and_chunking_never_changes_the_outcome() {
    let mut rng = Prng(42);
    let seeds: Vec<Vec<u8>> = commands()
        .iter()
        .map(Frame::encode)
        .chain(hellos().iter().map(Frame::encode))
        .chain(greetings().iter().map(Frame::encode))
        .chain(
            host_frames()
                .iter()
                .filter(|f| f.encode().len() < 4096)
                .map(Frame::encode),
        )
        .collect();
    let mut decoded = 0;
    let mut refused = 0;
    for round in 0..10_000 {
        let bytes = match round % 3 {
            0 => {
                let len = rng.below(64);
                rng.bytes(len)
            }
            1 => {
                let len = 1 + rng.below(48);
                let mut body = rng.bytes(len);
                body[0] = 1 + u8::try_from(rng.below(10)).expect("below ten");
                raw(&body)
            }
            _ => {
                let mut bytes = seeds[rng.below(seeds.len())].clone();
                for _ in 0..=rng.below(3) {
                    let at = rng.below(bytes.len());
                    bytes[at] = rng.byte();
                }
                bytes
            }
        };
        let per_read = 1 + rng.below(9);
        let (frames, end) = decode_stream::<Command>(&bytes, per_read);
        let whole = decode_stream::<Command>(&bytes, usize::MAX);
        assert_eq!((&frames, &end), (&whole.0, &whole.1), "{bytes:?}");
        decoded += frames.len();
        refused += usize::from(matches!(end, End::Refused(_)));
        assert_eq!(
            decode_stream::<HostFrame>(&bytes, per_read),
            decode_stream::<HostFrame>(&bytes, usize::MAX),
            "{bytes:?}"
        );
        let _ = decode_stream::<Hello>(&bytes, per_read);
        let _ = decode_stream::<Greeting>(&bytes, per_read);
    }
    assert!(decoded > 500, "only {decoded} frames decoded");
    assert!(refused > 500, "only {refused} inputs refused");
}

#[test]
fn versions_are_judged_against_a_range_that_is_never_empty() {
    assert!(VersionRange::new(Version::new(2), Version::new(1)).is_none());
    let range = VersionRange::new(Version::new(2), Version::new(4)).expect("ordered");
    assert_eq!(range.admits(Version::new(1)), Admission::TooOld);
    assert_eq!(range.admits(Version::new(2)), Admission::Speaks);
    assert_eq!(range.admits(Version::new(4)), Admission::Speaks);
    assert_eq!(range.admits(Version::new(5)), Admission::TooNew);
    assert_eq!(SUPPORTED.admits(PROTOCOL_VERSION), Admission::Speaks);
}

fn allowed(standing: Standing) -> Vec<CommandKind> {
    commands()
        .into_iter()
        .filter(|command| standing.admit(command.clone()).is_ok())
        .map(|command| command.kind())
        .collect()
}

#[test]
fn each_standing_admits_exactly_the_commands_the_design_gives_it() {
    use CommandKind::*;
    let shared = [Inject, Snapshot, Snapshot, Clients, Kill, Bye];
    let expect = |extra: &[CommandKind]| {
        let mut kinds: Vec<_> = commands()
            .iter()
            .map(Command::kind)
            .filter(|kind| shared.contains(kind) || extra.contains(kind))
            .collect();
        kinds.dedup();
        kinds
    };
    let dedup = |mut kinds: Vec<CommandKind>| {
        kinds.dedup();
        kinds
    };
    assert_eq!(
        dedup(allowed(Standing::Owner)),
        expect(&[Input, Resize, Detach])
    );
    assert_eq!(dedup(allowed(Standing::Watcher)), expect(&[Claim]));
    assert_eq!(dedup(allowed(Standing::Control)), expect(&[Detach]));
}

#[test]
fn a_refused_command_names_itself_and_an_admitted_one_keeps_its_fields_in_its_standings_type() {
    assert_eq!(
        Standing::Watcher.admit(Command::Input(payload(b"x"))),
        Err(Forbidden {
            command: CommandKind::Input
        })
    );
    assert_eq!(
        Standing::Control.admit(Command::Claim(size(1, 1))),
        Err(Forbidden {
            command: CommandKind::Claim
        })
    );
    assert_eq!(
        Standing::Owner.admit(Command::Resize(size(3, 4))),
        Ok(Admitted::Owner(OwnerCall::Resize(size(3, 4))))
    );
    assert_eq!(
        Standing::Watcher.admit(Command::Claim(size(3, 4))),
        Ok(Admitted::Watcher(WatcherCall::Claim(size(3, 4))))
    );
    assert_eq!(
        Standing::Control.admit(Command::Detach(ClientRef::new(2))),
        Ok(Admitted::Control(ControlCall::Detach(ClientRef::new(2))))
    );
    assert_eq!(
        Standing::Watcher.admit(Command::Bye),
        Ok(Admitted::Watcher(WatcherCall::Shared(Shared::Bye)))
    );
}

#[test]
fn a_role_names_the_purpose_behind_it() {
    let window = Role::Window {
        take: Take::Over,
        size: size(1, 1),
        colours: None,
    };
    let preview = Role::Preview {
        size: size(1, 1),
        colours: None,
    };
    assert_eq!(window.purpose(), Some(Purpose::Window));
    assert_eq!(preview.purpose(), Some(Purpose::Preview));
    assert_eq!(
        Role::Watch {
            purpose: Purpose::Watch
        }
        .purpose(),
        Some(Purpose::Watch)
    );
    assert_eq!(
        Role::Watch {
            purpose: Purpose::Preview
        }
        .purpose(),
        Some(Purpose::Preview)
    );
    assert_eq!(Role::Control.purpose(), None);
}

#[test]
fn a_pty_exit_becomes_a_code_a_signal_number_or_unknown() {
    let signal = |n: u8| ExitStatus::Signal(NonZeroU8::new(n).expect("nonzero"));
    assert_eq!(ExitStatus::from_pty(0, None), ExitStatus::Code(0));
    assert_eq!(ExitStatus::from_pty(255, None), ExitStatus::Code(255));
    assert_eq!(ExitStatus::from_pty(256, None), ExitStatus::Unknown);
    assert_eq!(ExitStatus::from_pty(1, Some("Killed")), signal(9));
    assert_eq!(ExitStatus::from_pty(1, Some("Terminated")), signal(15));
    assert_eq!(ExitStatus::from_pty(1, Some("Abort trap")), signal(6));
    assert_eq!(ExitStatus::from_pty(1, Some("Aborted")), signal(6));
    assert_eq!(ExitStatus::from_pty(1, Some("Signal 31")), signal(31));
    assert_eq!(ExitStatus::from_pty(1, Some("Killed: 9")), signal(9));
    assert_eq!(ExitStatus::from_pty(1, Some("Hangup: 1")), signal(1));
    assert_eq!(ExitStatus::from_pty(1, Some("Terminated: 15")), signal(15));
    assert_eq!(
        ExitStatus::from_pty(1, Some("Killed: 0")),
        ExitStatus::Unknown
    );
    assert_eq!(
        ExitStatus::from_pty(1, Some("Unknown signal: 12")),
        signal(12)
    );
    assert_eq!(
        ExitStatus::from_pty(1, Some("Signal 0")),
        ExitStatus::Unknown
    );
    assert_eq!(
        ExitStatus::from_pty(1, Some("Signal 300")),
        ExitStatus::Unknown
    );
    assert_eq!(
        ExitStatus::from_pty(1, Some("Bad news")),
        ExitStatus::Unknown
    );
}
