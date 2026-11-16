use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use agent_host::{
    ClientRef, Command, ExitStatus, Greeting, Hello, HelloRefusal, HostFrame, Payload, Purpose,
    Role, Standing, Take, VersionRange, Welcome, read_frame, write_frame,
};
use agent_session::testing::{Conn, RunningHost, Sandbox, WAIT, eventually, size};
use agent_session::{ClientError, DetachKey, Ended, External, Options, Outer, Wiring, attach};

const CTRL_BACKSLASH: &[u8] = b"\x1c";

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().map(|bytes| bytes.clone()).unwrap_or_default()
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Running {
    typing: UnixStream,
    external: Sender<External>,
    screen: Capture,
    outer: Arc<Outer>,
    ended: Option<JoinHandle<Result<Ended, ClientError>>>,
}

impl Running {
    fn start(
        socket: &agent_session::SocketPath,
        role: Role,
        detach: impl Into<Option<DetachKey>>,
    ) -> Self {
        let detach = detach.into();
        let (typing, input) = UnixStream::pair().expect("a pair");
        let (external, outside) = mpsc::channel();
        let screen = Capture::default();
        let outer = Arc::new(Outer::new(screen.clone()));
        let wiring = Wiring {
            input: Box::new(input),
            outer: Arc::clone(&outer),
            external: outside,
        };
        let socket = socket.clone();
        let ended = thread::spawn(move || attach(&socket, Options { role, detach }, wiring));
        Self {
            typing,
            external,
            screen,
            outer,
            ended: Some(ended),
        }
    }

    fn type_bytes(&mut self, bytes: &[u8]) {
        self.typing.write_all(bytes).expect("types");
    }

    fn shows(&self, text: &str) -> bool {
        eventually(WAIT, || self.screen.text().contains(text))
    }

    fn end(mut self) -> (Result<Ended, ClientError>, Capture) {
        let handle = self.ended.take().expect("running");
        assert!(
            eventually(WAIT, || handle.is_finished()),
            "the client didn't end"
        );
        let result = handle.join().expect("no panic");
        self.outer.restore();
        (result, self.screen.clone())
    }
}

fn ctrl_backslash() -> DetachKey {
    DetachKey::standard()
}

fn owner() -> Role {
    Role::Window {
        take: Take::Over,
        size: size(24, 80),
        colours: None,
    }
}

fn echoing(sandbox: &Sandbox, name: &str) -> RunningHost {
    sandbox.host(name, "stty raw -echo; echo READY; cat")
}

#[test]
fn what_is_typed_reaches_the_program_and_what_it_prints_reaches_the_terminal() {
    let sandbox = Sandbox::new();
    let host = echoing(&sandbox, "copies");
    let mut client = Running::start(&host.socket, owner(), ctrl_backslash());
    assert!(client.shows("READY"));
    client.type_bytes(b"hello there");
    assert!(client.shows("hello there"));
    client.type_bytes(CTRL_BACKSLASH);
    let (ended, _) = client.end();
    assert_eq!(ended.expect("attached"), Ended::Detached);
}

#[test]
fn the_detach_key_in_either_encoding_leaves_without_typing_it() {
    for key in [CTRL_BACKSLASH, b"\x1b[92;5u"] {
        let sandbox = Sandbox::new();
        let host = echoing(&sandbox, "detaches");
        let mut client = Running::start(&host.socket, owner(), ctrl_backslash());
        assert!(client.shows("READY"));
        client.type_bytes(b"ab");
        assert!(client.shows("ab"));
        client.type_bytes(key);
        client.type_bytes(b"never typed");
        let (ended, screen) = client.end();
        assert_eq!(ended.expect("attached"), Ended::Detached);
        assert!(!screen.text().contains("never typed"));
    }
}

#[test]
fn an_embedding_client_with_no_detach_key_passes_it_through() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("embedded", "stty raw -echo; echo READY; cat -v");
    let mut client = Running::start(&host.socket, owner(), None);
    assert!(client.shows("READY"));
    client.type_bytes(b"a\x1cb");
    assert!(client.shows("a^\\b"));
    let mut control = Conn::open(&host.socket, Role::Control).expect("control");
    control.send(&Command::Kill);
    let (ended, _) = client.end();
    assert!(matches!(ended.expect("attached"), Ended::Exited(_)));
}

#[test]
fn a_key_held_back_as_a_possible_detach_key_is_typed_after_the_wait() {
    let sandbox = Sandbox::new();
    let host = echoing(&sandbox, "esc");
    let mut client = Running::start(&host.socket, owner(), ctrl_backslash());
    assert!(client.shows("READY"));
    client.type_bytes(b"\x1b");
    assert!(eventually(WAIT, || client.screen.bytes().contains(&0x1b)));
    client.type_bytes(CTRL_BACKSLASH);
    client.end().0.expect("attached");
}

fn terminal_taker(sandbox: &Sandbox, name: &str) -> RunningHost {
    let script = r"
        printf '\033[?1049h\033[2J\033[Hup'
        printf '\033[?1000h\033[?1006h\033[?2004h\033[?25l\033[>1u\033[?1004h'
        read x
    ";
    sandbox.host(name, script)
}

fn outer_after(screen: &Capture) -> vt100::Parser {
    let mut outer = vt100::Parser::new(24, 80, 0);
    outer.process(&screen.bytes());
    outer
}

#[test]
fn the_outer_terminal_is_put_back_after_a_detach() {
    let sandbox = Sandbox::new();
    let host = terminal_taker(&sandbox, "tidy");
    let mut client = Running::start(&host.socket, owner(), ctrl_backslash());
    assert!(client.shows("up"));
    assert!(eventually(WAIT, || client
        .screen
        .text()
        .contains("\x1b[?1004h")));
    let before = outer_after(&client.screen);
    assert!(before.screen().alternate_screen());
    assert!(before.screen().hide_cursor());
    assert!(before.screen().bracketed_paste());
    client.type_bytes(CTRL_BACKSLASH);
    let (ended, screen) = client.end();
    assert_eq!(ended.expect("attached"), Ended::Detached);
    let outer = outer_after(&screen);
    assert!(!outer.screen().alternate_screen());
    assert!(!outer.screen().hide_cursor());
    assert!(!outer.screen().bracketed_paste());
    assert_eq!(
        outer.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::None
    );
    let text = screen.text();
    for undo in ["\x1b[?1000l", "\x1b[?1006l", "\x1b[?1004l", "\x1b[<1u"] {
        assert!(text.contains(undo), "{undo:?} missing from {text:?}");
    }
}

#[test]
fn a_client_that_attached_after_the_program_set_modes_restores_what_the_replay_set() {
    let sandbox = Sandbox::new();
    let host = terminal_taker(&sandbox, "replayed");
    let mut first = Running::start(&host.socket, owner(), ctrl_backslash());
    assert!(first.shows("\x1b[?1004h"));
    let mut late = Running::start(
        &host.socket,
        Role::Watch {
            purpose: Purpose::Window,
        },
        ctrl_backslash(),
    );
    assert!(late.shows("up"));
    late.type_bytes(CTRL_BACKSLASH);
    let (_, screen) = late.end();
    let text = screen.text();
    assert!(
        text.contains("\x1b[?1004h"),
        "the replay turned focus reports on"
    );
    assert!(
        text.contains("\x1b[?1004l"),
        "and they are turned off again"
    );
    assert!(text.contains("\x1b[<1u"));
    assert!(!outer_after(&screen).screen().alternate_screen());
    first.type_bytes(CTRL_BACKSLASH);
    first.end().0.expect("attached");
}

#[test]
fn the_outer_terminal_is_put_back_when_the_harness_exits() {
    let sandbox = Sandbox::new();
    let gate = sandbox.gate("exit");
    let host = sandbox.host(
        "ends",
        &format!(
            r"printf '\033[?1049h\033[?1000h\033[?25lbye'; {}; exit 3",
            gate.wait()
        ),
    );
    let client = Running::start(&host.socket, owner(), ctrl_backslash());
    assert!(client.shows("bye"));
    gate.open();
    let (ended, screen) = client.end();
    assert_eq!(ended.expect("attached"), Ended::Exited(ExitStatus::Code(3)));
    let outer = outer_after(&screen);
    assert!(!outer.screen().alternate_screen());
    assert!(!outer.screen().hide_cursor());
}

fn fake_host(
    then: impl FnOnce(UnixStream) + Send + 'static,
) -> (tempfile::TempDir, agent_session::SocketPath) {
    let sandbox = Sandbox::new();
    let name = agent_session::testing::session("fake");
    let socket = sandbox.socket(&name);
    let listener = UnixListener::bind(socket.as_path()).expect("binds");
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let hello = read_frame::<Hello>(&mut stream).expect("a frame");
        assert!(hello.is_some());
        then(stream);
    });
    let keep = tempfile::tempdir().expect("tempdir");
    std::mem::forget(sandbox);
    (keep, socket)
}

fn welcome(standing: Standing) -> Greeting {
    Greeting::Welcome(Welcome {
        version: agent_host::PROTOCOL_VERSION,
        size: size(24, 80),
        standing,
        you: ClientRef::new(1),
    })
}

#[test]
fn the_outer_terminal_is_put_back_when_the_host_goes_away() {
    let (_keep, socket) = fake_host(|mut stream| {
        write_frame(&mut stream, &welcome(Standing::Owner)).expect("welcome");
        let output = HostFrame::Output(
            Payload::new(b"\x1b[?1049h\x1b[?25l\x1b[?2004hscreen".to_vec()).expect("fits"),
        );
        write_frame(&mut stream, &output).expect("output");
        thread::sleep(Duration::from_millis(100));
    });
    let client = Running::start(&socket, owner(), ctrl_backslash());
    let (ended, screen) = client.end();
    assert_eq!(ended.expect("attached"), Ended::HostGone);
    let outer = outer_after(&screen);
    assert!(!outer.screen().alternate_screen());
    assert!(!outer.screen().hide_cursor());
    assert!(!outer.screen().bracketed_paste());
}

#[test]
fn a_host_that_speaks_another_version_is_reported_with_how_to_upgrade_it() {
    let (_keep, socket) = fake_host(|mut stream| {
        let supported = VersionRange::new(agent_host::Version::new(7), agent_host::Version::new(8))
            .expect("a range");
        write_frame(
            &mut stream,
            &Greeting::Refused(HelloRefusal::Version { supported }),
        )
        .expect("refuses");
    });
    let client = Running::start(&socket, owner(), ctrl_backslash());
    let (ended, _) = client.end();
    let Err(ClientError::Refused(text)) = ended else {
        panic!("refused");
    };
    assert!(text.contains("restart the session"), "{text}");
    assert!(text.contains("7 to 8"), "{text}");
}

#[test]
fn no_host_at_the_socket_is_an_error_not_a_hang() {
    let sandbox = Sandbox::new();
    let socket = sandbox.socket(&agent_session::testing::session("absent"));
    let client = Running::start(&socket, owner(), ctrl_backslash());
    let (ended, _) = client.end();
    assert!(matches!(ended, Err(ClientError::Connect(_))));
}

#[test]
fn a_demoted_window_claims_ownership_with_its_first_key_and_that_key_is_not_typed() {
    let sandbox = Sandbox::new();
    let host = echoing(&sandbox, "claims");
    let mut first = Running::start(&host.socket, owner(), ctrl_backslash());
    assert!(first.shows("READY"));
    first.type_bytes(b"one");
    assert!(first.shows("one"));
    let mut second = Conn::open(&host.socket, owner()).expect("takes over");
    assert_eq!(second.welcome.standing, Standing::Owner);
    let mut claimed = false;
    for _ in 0..100 {
        first.type_bytes(b"Q");
        claimed = matches!(
            second.next_event(Duration::from_millis(100)),
            Some(HostFrame::Demoted { .. })
        );
        if claimed {
            break;
        }
    }
    assert!(claimed, "the first client claimed ownership back");
    first.type_bytes(b"two");
    assert!(first.shows("two"));
    assert!(!first.screen.text().contains('Q'));
    first.type_bytes(CTRL_BACKSLASH);
    first.end().0.expect("attached");
}

#[test]
fn a_read_only_client_types_nothing_and_cannot_claim() {
    let sandbox = Sandbox::new();
    let host = echoing(&sandbox, "readonly");
    let mut owner_conn = Conn::open(&host.socket, owner()).expect("owner");
    assert!(owner_conn.wait_screen(WAIT, |text| text.contains("READY")));
    let mut watcher = Running::start(
        &host.socket,
        Role::Watch {
            purpose: Purpose::Window,
        },
        ctrl_backslash(),
    );
    watcher.type_bytes(b"ignored");
    owner_conn.send(&Command::Input(
        Payload::new(b"seen".to_vec()).expect("fits"),
    ));
    assert!(watcher.shows("seen"));
    assert!(!watcher.screen.text().contains("ignored"));
    watcher.type_bytes(CTRL_BACKSLASH);
    watcher.end().0.expect("attached");
    owner_conn.send(&Command::Clients);
    let Some(HostFrame::Clients(list)) =
        owner_conn.wait_event(WAIT, |frame| matches!(frame, HostFrame::Clients(_)))
    else {
        panic!("a listing");
    };
    assert!(
        list.as_slice()
            .iter()
            .all(|info| info.standing == Standing::Owner)
    );
}

#[test]
fn the_owner_resizing_its_terminal_resizes_the_session() {
    let sandbox = Sandbox::new();
    let host = sandbox.host(
        "resizes",
        r"trap 'echo SIZE $(stty size)' WINCH; echo READY; while :; do sleep 0.05; done",
    );
    let mut client = Running::start(&host.socket, owner(), ctrl_backslash());
    assert!(client.shows("READY"));
    client
        .external
        .send(External::Resized(size(33, 111)))
        .expect("resizes");
    assert!(client.shows("SIZE 33 111"));
    client.type_bytes(CTRL_BACKSLASH);
    client.end().0.expect("attached");
}

#[test]
fn a_lone_escape_reaches_the_program_promptly_however_much_it_keeps_printing() {
    let sandbox = Sandbox::new();
    let host = sandbox.host(
        "escape",
        r"stty raw -echo; echo READY; (while :; do printf .; sleep 0.01; done) & cat -v",
    );
    let mut client = Running::start(&host.socket, owner(), ctrl_backslash());
    assert!(client.shows("READY"));
    let escapes = |client: &Running| client.screen.text().matches("^[").count();
    let before = escapes(&client);
    let started = Instant::now();
    client.type_bytes(b"\x1b");
    assert!(eventually(WAIT, || escapes(&client) > before));
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_millis(400),
        "the Esc took {waited:?}"
    );
    client.type_bytes(CTRL_BACKSLASH);
    client.end().0.expect("attached");
}

#[test]
fn a_polite_window_that_watches_does_not_take_the_session_on_a_key() {
    let sandbox = Sandbox::new();
    let host = echoing(&sandbox, "politekeys");
    let mut owner_conn = Conn::open(&host.socket, owner()).expect("owner");
    assert!(owner_conn.wait_screen(WAIT, |text| text.contains("READY")));
    let polite = Role::Window {
        take: Take::Politely,
        size: size(24, 80),
        colours: None,
    };
    let mut client = Running::start(&host.socket, polite, ctrl_backslash());
    assert!(client.shows("READY"));
    client.type_bytes(b"keys that must not claim");
    owner_conn.send(&Command::Clients);
    let Some(HostFrame::Clients(list)) =
        owner_conn.wait_event(WAIT, |frame| matches!(frame, HostFrame::Clients(_)))
    else {
        panic!("a listing");
    };
    let owners = list
        .as_slice()
        .iter()
        .filter(|info| info.standing == Standing::Owner)
        .count();
    assert_eq!(owners, 1);
    assert!(
        owner_conn.next_event(Duration::from_millis(300)).is_none(),
        "the owner was demoted"
    );
    client.type_bytes(CTRL_BACKSLASH);
    client.end().0.expect("attached");
}

#[test]
fn a_waiting_window_that_is_promoted_sizes_the_session_to_where_its_terminal_is_now() {
    let sandbox = Sandbox::new();
    let host = sandbox.host(
        "promoted",
        r"trap 'echo SIZE $(stty size)' WINCH; echo READY; while :; do sleep 0.05; done",
    );
    let mut owner_conn = Conn::open(&host.socket, owner()).expect("owner");
    assert!(owner_conn.wait_screen(WAIT, |text| text.contains("READY")));
    let polite = Role::Window {
        take: Take::Politely,
        size: size(30, 100),
        colours: None,
    };
    let mut client = Running::start(&host.socket, polite, ctrl_backslash());
    assert!(client.shows("READY"));
    client
        .external
        .send(External::Resized(size(50, 150)))
        .expect("resizes");
    owner_conn.send(&Command::Bye);
    assert!(client.shows("SIZE 50 150"), "{}", client.screen.text());
    client.type_bytes(CTRL_BACKSLASH);
    client.end().0.expect("attached");
}

#[test]
fn the_detach_key_inside_a_bracketed_paste_is_typed_and_not_a_request_to_leave() {
    let sandbox = Sandbox::new();
    let host = echoing(&sandbox, "paste");
    let mut client = Running::start(&host.socket, owner(), ctrl_backslash());
    assert!(client.shows("READY"));
    client.type_bytes(b"\x1b[200~before\x1cafter\x1b[201~");
    assert!(client.shows("after"));
    client.type_bytes(CTRL_BACKSLASH);
    assert_eq!(client.end().0.expect("attached"), Ended::Detached);
}

#[test]
fn a_client_told_to_end_puts_the_outer_terminal_back() {
    let sandbox = Sandbox::new();
    let host = terminal_taker(&sandbox, "signalled");
    let client = Running::start(&host.socket, owner(), ctrl_backslash());
    assert!(client.shows("\x1b[?1004h"));
    client
        .external
        .send(External::Terminated(15))
        .expect("signals");
    let (ended, screen) = client.end();
    assert_eq!(ended.expect("attached"), Ended::Terminated(15));
    let outer = outer_after(&screen);
    assert!(!outer.screen().alternate_screen());
    assert!(!outer.screen().hide_cursor());
}

#[test]
fn a_host_that_stopped_reading_does_not_stop_the_client_from_detaching_or_ending() {
    for leave in [Leave::DetachKey, Leave::Signal] {
        let sandbox = Sandbox::new();
        let mut config = sandbox.config(
            &agent_session::testing::session("deaf"),
            "stty raw -echo; echo READY; sleep 600",
        );
        config.timing.input_wait = Duration::from_secs(120);
        config.timing.write_stall = Duration::from_secs(120);
        let host = sandbox.start(config);
        let mut client = Running::start(&host.socket, owner(), ctrl_backslash());
        assert!(client.shows("READY"));
        let typed = vec![b'x'; 3 * 1024 * 1024];
        client.type_bytes(&typed);
        let started = Instant::now();
        match leave {
            Leave::DetachKey => client.type_bytes(CTRL_BACKSLASH),
            Leave::Signal => client
                .external
                .send(External::Terminated(15))
                .expect("signals"),
        }
        let (ended, screen) = client.end();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{leave:?} took {:?}",
            started.elapsed()
        );
        let expected = match leave {
            Leave::DetachKey => Ended::Detached,
            Leave::Signal => Ended::Terminated(15),
        };
        assert_eq!(ended.expect("attached"), expected);
        assert_ne!(screen.text(), "");
    }
}

#[derive(Debug, Clone, Copy)]
enum Leave {
    DetachKey,
    Signal,
}

#[test]
fn a_paste_without_its_end_does_not_leave_the_detach_key_dead_for_good() {
    let sandbox = Sandbox::new();
    let host = echoing(&sandbox, "openpaste");
    let mut client = Running::start(&host.socket, owner(), ctrl_backslash());
    assert!(client.shows("READY"));
    client.type_bytes(b"\x1b[200~never ended");
    assert!(client.shows("never ended"));
    let chunk = vec![b'y'; 1024 * 1024];
    for _ in 0..9 {
        client.type_bytes(&chunk);
    }
    client.type_bytes(CTRL_BACKSLASH);
    assert_eq!(client.end().0.expect("attached"), Ended::Detached);
}
