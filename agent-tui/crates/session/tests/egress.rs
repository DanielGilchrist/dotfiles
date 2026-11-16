//! The host as the program's terminal: queries it answers itself and the
//! allowlist between the program's output and its clients, through a real
//! host and clients in a temporary directory.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use agent_core::config::Osc52;
use agent_core::theme::Rgb;
use agent_host::{Colours, Command, Purpose, Role, Scrollback, Take};
use agent_session::testing::{Conn, Sandbox, WAIT, eventually, session, size};

fn window(colours: Option<Colours>) -> Role {
    Role::Window {
        take: Take::Over,
        size: size(24, 80),
        colours,
    }
}

fn watcher() -> Role {
    Role::Watch {
        purpose: Purpose::Window,
    }
}

/// `bytes` as a `printf` that prints them, in single quotes. Anything that
/// isn't plain ASCII is written as an octal escape of three digits.
fn printf_of(bytes: &[u8]) -> String {
    let mut format = String::new();
    for byte in bytes {
        match byte {
            b'\\' => format.push_str("\\\\"),
            b'%' => format.push_str("%%"),
            b'\'' => format.push_str("'\\''"),
            0x20..=0x7e => format.push(char::from(*byte)),
            byte => format.extend(format!("\\{byte:03o}").chars()),
        }
    }
    format!("printf '{format}'")
}

fn read_file(path: &PathBuf) -> Vec<u8> {
    std::fs::read(path).unwrap_or_default()
}

/// A program that prints each query, reads exactly the length of the
/// reply it expects from its own input into a file of its own, and ends
/// with `DONE`.
struct Asking {
    script: String,
    files: Vec<(PathBuf, Vec<u8>)>,
}

impl Asking {
    fn new(sandbox: &Sandbox, preamble: &str, asked: &[(&[u8], Vec<u8>)]) -> Self {
        let mut parts = vec![format!("stty raw -echo; {preamble}; ")];
        let mut files = Vec::new();
        for (at, (print, reply)) in asked.iter().enumerate() {
            let file = sandbox.state.join(format!("reply{at}"));
            parts.push(format!(
                "{}; dd bs=1 count={} of={} 2>/dev/null; ",
                printf_of(print),
                reply.len(),
                file.display()
            ));
            files.push((file, reply.clone()));
        }
        parts.push("echo DONE; cat".to_owned());
        Self {
            script: parts.concat(),
            files,
        }
    }

    fn replied(&self) -> bool {
        eventually(WAIT, || {
            self.files
                .iter()
                .all(|(file, reply)| read_file(file).len() >= reply.len())
        })
    }

    fn replies(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.files
            .iter()
            .map(|(file, reply)| (read_file(file), reply.clone()))
            .collect()
    }
}

fn version_reply() -> Vec<u8> {
    format!("\x1bP>|agent-host({})\x1b\\", env!("CARGO_PKG_VERSION")).into_bytes()
}

#[test]
fn queries_are_answered_with_the_exact_bytes_while_no_client_is_attached() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(&sandbox.state).expect("state dir");
    let asked: Vec<(&[u8], Vec<u8>)> = vec![
        (b"\x1b[c", b"\x1b[?62;22c".to_vec()),
        (b"\x1b[0c", b"\x1b[?62;22c".to_vec()),
        (b"\x1b[>c", b"\x1b[>1;10;0c".to_vec()),
        (b"\x1b[5n", b"\x1b[0n".to_vec()),
        (b"\x1b[2J\x1b[Habc\x1b[6n", b"\x1b[1;4R".to_vec()),
        (b"\x1b[10;20H\x1b[6n", b"\x1b[10;20R".to_vec()),
        (b"\x1b[?6n", b"\x1b[?10;20;1R".to_vec()),
        (b"\x1b[>0q", version_reply()),
        (b"\x1b[?2026$p", b"\x1b[?2026;2$y".to_vec()),
        (b"\x1b[?2004h\x1b[?2004$p", b"\x1b[?2004;1$y".to_vec()),
        (b"\x1b[?9999$p", b"\x1b[?9999;0$y".to_vec()),
        (b"\x1b[18t", b"\x1b[8;24;80t".to_vec()),
        (b"\x1b[14t", b"\x1b[4;384;640t".to_vec()),
        (b"\x1b[?u", b"\x1b[?0u".to_vec()),
        (b"\x1b[>1u\x1b[>5u\x1b[?u", b"\x1b[?5u".to_vec()),
        (b"\x1b[<u\x1b[?u", b"\x1b[?1u".to_vec()),
    ];
    let program = Asking::new(&sandbox, "echo READY", &asked);
    let host = sandbox.host("noclient", &program.script);
    assert!(program.replied(), "{:?}", program.replies());
    for (got, expected) in program.replies() {
        assert_eq!(
            String::from_utf8_lossy(&got),
            String::from_utf8_lossy(&expected)
        );
    }
    let mut late = Conn::open(&host.socket, watcher()).expect("watcher");
    assert!(late.wait_screen(WAIT, |text| text.contains("DONE")));
}

#[test]
fn a_query_is_answered_once_and_never_reaches_the_clients_terminals() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(&sandbox.state).expect("state dir");
    let gate = sandbox.gate("go");
    let reply = sandbox.state.join("reply");
    let script = format!(
        "{}; stty raw -echo min 0 time 10; printf before; printf '\\033[6n\\033[c'; dd bs=1 count=100 of={} 2>/dev/null; printf after; stty min 1 time 0; cat",
        gate.wait(),
        reply.display()
    );
    let host = sandbox.host("attached", &script);
    let mut owner = Conn::open(&host.socket, window(None)).expect("owner");
    let mut watching = Conn::open(&host.socket, watcher()).expect("watcher");
    gate.open();
    for conn in [&mut owner, &mut watching] {
        assert!(conn.wait_screen(WAIT, |text| text.contains("beforeafter")));
        for query in [&b"\x1b[6n"[..], b"\x1b[c"] {
            assert!(
                !conn
                    .output
                    .windows(query.len())
                    .any(|window| window == query),
                "{query:?} reached a client"
            );
        }
    }
    assert_eq!(
        String::from_utf8_lossy(&read_file(&reply)),
        "\x1b[1;7R\x1b[?62;22c",
        "the program is read everything it was sent until a second of quiet"
    );
    owner.send(&Command::Bye);
}

fn colours() -> Colours {
    Colours {
        foreground: Rgb {
            r: 0xe0,
            g: 0x01,
            b: 0xff,
        },
        background: Rgb {
            r: 0x00,
            g: 0x10,
            b: 0x20,
        },
    }
}

fn colour_query_program(sandbox: &Sandbox, gate: &str) -> (String, Vec<PathBuf>) {
    let files: Vec<PathBuf> = ["bg", "fg"]
        .iter()
        .map(|name| sandbox.state.join(name))
        .collect();
    let script = format!(
        "{gate}; stty raw -echo min 0 time 10; {}; dd bs=1 count=24 of={} 2>/dev/null; {}; dd bs=1 count=25 of={} 2>/dev/null; echo DONE; cat",
        printf_of(b"\x1b]11;?\x07"),
        files[0].display(),
        printf_of(b"\x1b]10;?\x1b\\"),
        files[1].display(),
    );
    (script, files)
}

#[test]
fn colour_queries_are_answered_from_the_owners_colours_and_not_at_all_without_them() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(&sandbox.state).expect("state dir");
    let gate = sandbox.gate("owned");
    let (script, files) = colour_query_program(&sandbox, &gate.wait());
    let host = sandbox.host("colours", &script);
    let mut owner = Conn::open(&host.socket, window(Some(colours()))).expect("owner");
    gate.open();
    assert!(owner.wait_screen(WAIT, |text| text.contains("DONE")));
    assert_eq!(
        String::from_utf8_lossy(&read_file(&files[0])),
        "\x1b]11;rgb:0000/1010/2020\x07"
    );
    assert_eq!(
        String::from_utf8_lossy(&read_file(&files[1])),
        "\x1b]10;rgb:e0e0/0101/ffff\x1b\\"
    );
    for query in [&b"\x1b]11;?"[..], b"\x1b]10;?"] {
        assert!(
            !owner
                .output
                .windows(query.len())
                .any(|window| window == query),
            "{query:?} reached the client"
        );
    }
}

#[test]
fn colour_queries_get_no_reply_from_an_owner_that_gave_no_colours_or_from_no_owner() {
    for with_owner in [true, false] {
        let sandbox = Sandbox::new();
        std::fs::create_dir_all(&sandbox.state).expect("state dir");
        let gate = sandbox.gate("owned");
        let (script, files) = colour_query_program(&sandbox, &gate.wait());
        let host = sandbox.host("nocolours", &script);
        let mut owner = with_owner.then(|| Conn::open(&host.socket, window(None)).expect("owner"));
        gate.open();
        let mut watching = Conn::open(&host.socket, watcher()).expect("watcher");
        assert!(watching.wait_screen(WAIT, |text| text.contains("DONE")));
        for file in &files {
            assert_eq!(read_file(file), b"", "owner: {with_owner}");
        }
        for conn in owner.iter_mut().chain(std::iter::once(&mut watching)) {
            assert!(
                !conn
                    .output
                    .windows(7)
                    .any(|window| window == b"\x1b]11;?\x07"),
                "the query reached a client"
            );
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Everything a hostile program might print, each followed by a marker that
/// only arrives if the output after it was read again from the start.
fn hostile_script(wait: &str) -> String {
    let step = |bytes: &[u8], marker: u32| format!("{}; printf '[m{marker}]'; ", printf_of(bytes));
    [
        format!("{wait}; printf '[m0]'; "),
        step(b"\x1b]1337;SetUserVar=foo=YmFy\x07", 1),
        step(b"\x1b]52;c;?\x07", 2),
        step(b"\x1b_Gf=100,a=T;AAAA\x1b\\", 3),
        step(b"\x1bPq#0;2;0;0;0~-\x1b\\", 4),
        step(b"\x1b]8;;javascript:alert(1)\x07text\x1b]8;;\x07", 5),
        step(b"\x1b]8;;https://example.com/ok\x07link\x1b]8;;\x07", 6),
        step(b"\x1bPtmux;\x1b\x1b]1337;SetUserVar=x=YQ==\x07\x1b\\", 7),
        "printf '\\033]0;'; head -c 1048576 /dev/zero | tr '\\0' A; printf '\\007[m8]'; "
            .to_owned(),
        "printf '\\033]0;never ends'; head -c 307200 /dev/zero | tr '\\0' B; printf '\\030[m9]'; "
            .to_owned(),
        step(b"\x1b]0;the title\x07", 10),
        step(b"\x1b]52;c;aGk=\x07", 11),
        step(b"\x9b31m\xc2\x9b31m\x9d0;x\x9c", 12),
        step(
            b"\x1b]9;notify\x07\x1b]777;notify;a;b\x07\x1b]133;A\x07\x1b]7;file:///tmp\x07",
            13,
        ),
        step(b"\x1b[8;5;5t\x1b[21t\x1b[?1001h\x1b[?3h", 14),
        "printf 'END'; cat".to_owned(),
    ]
    .concat()
}

const DANGEROUS: &[&[u8]] = &[
    b"\x1b]1337",
    b"SetUserVar",
    b"\x1b_",
    b"\x1bP",
    b"\x1b^",
    b"\x1bX",
    b"javascript:",
    b"\x1b]52;c;?",
    b"\x1b]9;",
    b"\x1b]777",
    b"\x1b]133",
    b"\x1b]7;",
    b"\x1b[8;5;5t",
    b"\x1b[21t",
    b"?1001h",
    b"?3h",
    b"\x9b",
    b"\xc2\x9b",
    b"\x9d",
];

fn marker(number: u32) -> Vec<u8> {
    format!("[m{number}]").into_bytes()
}

fn hostile_run(osc52: Osc52) -> (Conn, Conn) {
    let sandbox = Sandbox::new();
    let name = session("hostile");
    std::fs::create_dir_all(&sandbox.state).expect("state dir");
    let gate = sandbox.gate("clients");
    let mut config = sandbox.config(&name, &hostile_script(&gate.wait()));
    config.osc52 = osc52;
    let host = sandbox.start(config);
    let mut owner = Conn::open(&host.socket, window(None)).expect("owner");
    let mut watching = Conn::open(&host.socket, watcher()).expect("watcher");
    gate.open();
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline
        && [&owner, &watching]
            .iter()
            .any(|conn| !contains(&conn.output, b"END"))
    {
        owner.pump(Duration::from_millis(2));
        watching.pump(Duration::from_millis(2));
    }
    for conn in [&owner, &watching] {
        assert!(
            contains(&conn.output, b"END"),
            "the text after the hostile output never arrived, closed: {}",
            conn.closed
        );
    }
    (owner, watching)
}

#[test]
fn nothing_dangerous_reaches_a_client_the_text_around_it_flows_and_only_the_owner_gets_the_title_and_clipboard()
 {
    let (owner, watching) = hostile_run(Osc52::Owner);
    for (who, conn) in [("owner", &owner), ("watcher", &watching)] {
        for needle in DANGEROUS {
            assert!(
                !contains(&conn.output, needle),
                "{who} got {:?}",
                String::from_utf8_lossy(needle)
            );
        }
        for number in 0..=14 {
            assert!(
                contains(&conn.output, &marker(number)),
                "{who} lost the text after item {number}"
            );
        }
        assert!(
            contains(
                &conn.output,
                b"\x1b]8;;https://example.com/ok\x07link\x1b]8;;\x07"
            ),
            "{who} lost the allowed link"
        );
    }
    let write: &[u8] = b"\x1b]52;c;aGk=\x07";
    let title: &[u8] = b"\x1b]0;the title\x07";
    assert!(contains(&owner.output, write));
    assert!(contains(&owner.output, title));
    assert!(!contains(&watching.output, write));
    assert!(!contains(&watching.output, title));
}

#[test]
fn a_clipboard_write_reaches_no_client_unless_the_host_was_configured_to_pass_it_to_the_owner() {
    for (osc52, passed) in [(Osc52::Off, false), (Osc52::Owner, true)] {
        let sandbox = Sandbox::new();
        std::fs::create_dir_all(&sandbox.state).expect("state dir");
        let gate = sandbox.gate("clients");
        let name = session("clipboard");
        let script = format!(
            "{}; {}; echo END; cat",
            gate.wait(),
            printf_of(b"\x1b]52;c;aGk=\x07\x1b]52;c;aGk=\x1b\\")
        );
        let mut config = sandbox.config(&name, &script);
        config.osc52 = osc52;
        let host = sandbox.start(config);
        let mut owner = Conn::open(&host.socket, window(None)).expect("owner");
        let mut watching = Conn::open(&host.socket, watcher()).expect("watcher");
        gate.open();
        for conn in [&mut owner, &mut watching] {
            assert!(conn.wait_output(WAIT, |output| contains(output, b"END")));
        }
        assert_eq!(contains(&owner.output, b"\x1b]52;c;aGk=\x07"), passed);
        assert_eq!(contains(&owner.output, b"\x1b]52;c;aGk=\x1b\\"), passed);
        assert!(!contains(&watching.output, b"]52;"));
    }
}

#[test]
fn a_replay_holds_nothing_the_live_output_was_not_allowed_to_and_only_the_owners_has_the_title() {
    let sandbox = Sandbox::new();
    let script = format!(
        "{}; echo hello READY; cat",
        printf_of(b"\x1b]2;secret title\x07\x1b[?9999h\x1b[?1001h\x1b]1337;SetUserVar=a=Yg==\x07\x1b[?2004h")
    );
    let host = sandbox.host("replays", &script);
    let mut first = Conn::open(&host.socket, window(None)).expect("owner");
    assert!(first.wait_screen(WAIT, |text| text.contains("READY")));
    let mut late_watcher = Conn::open(&host.socket, watcher()).expect("watcher");
    assert!(late_watcher.wait_screen(WAIT, |text| text.contains("READY")));
    let mut late_owner = Conn::open(&host.socket, window(None)).expect("owner");
    assert!(late_owner.wait_screen(WAIT, |text| text.contains("READY")));
    for conn in [&late_watcher, &late_owner] {
        for needle in [&b"?9999"[..], b"?1001", b"1337"] {
            assert!(!contains(&conn.output, needle), "{needle:?}");
        }
        assert!(contains(&conn.output, b"\x1b[?2004h"));
    }
    assert!(contains(&late_owner.output, b"secret title"));
    assert!(!contains(&late_watcher.output, b"secret title"));
}

#[test]
fn a_snapshot_is_text_the_allowlist_would_forward() {
    let sandbox = Sandbox::new();
    let host = sandbox.host(
        "snapshots",
        "printf '\\033[1;31mred\\033[0m \\033]1337;x\\007plain\\033_G\\033\\\\'; echo READY; cat",
    );
    let mut control = Conn::open(&host.socket, Role::Control).expect("control");
    let ready = eventually(WAIT, || {
        control.snapshot_text(Scrollback::Screen).contains("READY")
    });
    assert!(ready);
    assert!(
        control
            .snapshot_text(Scrollback::Screen)
            .contains("red plain")
    );
    assert!(!contains(&control.snapshot, b"1337"));
}

const QUERIES: usize = 300_000;
const REPLY_LEN: usize = b"\x1b[?62;22c".len();
/// The host's reply budget and what a pty holds besides, which is far less
/// than the 2.7 MB the program asked for.
const REPLY_QUEUE_LIMIT: usize = 1024 * 1024;

#[test]
fn a_program_that_asks_and_reads_no_answer_cannot_make_the_host_grow_or_stall() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(&sandbox.state).expect("state dir");
    let received = sandbox.state.join("received");
    let script = format!(
        "stty raw -echo; yes \"$(printf '\\033[c')\" | head -n {QUERIES}; echo FLOODED; stty min 0 time 10; dd bs=1024 of={} 2>/dev/null; echo DRAINED; sleep 30",
        received.display()
    );
    let name = session("noreader");
    let host = sandbox.start(sandbox.config(&name, &script));
    let mut watching = Conn::open(&host.socket, watcher()).expect("watcher");
    assert!(
        watching.wait_output(Duration::from_secs(30), |output| contains(
            output, b"FLOODED"
        )),
        "the host stopped reading the program's output"
    );
    let log = sandbox.files.log(&name);
    assert!(eventually(WAIT, || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("replies to the program's queries were dropped")
    }));
    assert!(
        watching.wait_output(Duration::from_secs(30), |output| contains(
            output, b"DRAINED"
        )),
        "the program never finished reading what it was sent"
    );
    let bytes = read_file(&received).len();
    assert!(bytes > 0, "no reply was delivered at all");
    assert!(
        bytes <= REPLY_QUEUE_LIMIT,
        "{bytes} bytes of replies reached a program that read none, of {} asked for",
        QUERIES * REPLY_LEN
    );
    assert!(
        watching
            .snapshot_text(Scrollback::Screen)
            .contains("FLOODED")
    );
}
