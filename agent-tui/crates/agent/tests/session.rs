//! The hidden `agent session` commands as real processes. Every run has a
//! scratch runtime and state directory, and never touches a user's sessions.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use agent_host::{Command as HostCommand, Role};
use agent_session::testing::{Conn, WAIT, eventually};
use agent_session::{Creation, Liveness, RuntimeDir, probe};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::{Pid, Uid};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const AGENT: &str = env!("CARGO_BIN_EXE_agent");

struct Scratch {
    dir: tempfile::TempDir,
    sessions: Mutex<Vec<String>>,
}

impl Drop for Scratch {
    /// Ends the hosts a test started, so none outlives it.
    fn drop(&mut self) {
        let runtime = RuntimeDir::locate(Some(self.path("r")), None, Uid::current());
        let Ok(runtime) = runtime.open(Creation::Existing) else {
            return;
        };
        let sessions = self.sessions.lock().map(|s| s.clone()).unwrap_or_default();
        for session in sessions {
            let Ok(name) = session.parse() else { continue };
            let Ok(socket) = runtime.socket(&name) else {
                continue;
            };
            if let Ok(mut control) = Conn::open(&socket, Role::Control) {
                control.send(&HostCommand::Kill);
            }
        }
    }
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("a")
            .tempdir_in("/tmp")
            .expect("a temp dir");
        Self {
            dir,
            sessions: Mutex::new(Vec::new()),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn record(&self, session: &str) -> PathBuf {
        self.path("s")
            .join("agent/hosts")
            .join(format!("{session}.json"))
    }

    fn command(&self) -> Command {
        let mut command = Command::new(AGENT);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.path("h"))
            .env("XDG_STATE_HOME", self.path("s"))
            .env("AGENT_RUNTIME_DIR", self.path("r"))
            .env("AGENT_WORKTREES", self.path("w"))
            .env("ZELLIJ_SOCKET_DIR", self.path("z"))
            .env("TERM", "xterm-256color");
        command
    }

    fn host(&self, session: &str, script: &str) -> std::process::Output {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.push(session.to_owned());
        }
        self.command()
            .args([
                "session", "host", session, "--spawn", "--size", "80x24", "--",
            ])
            .args(["sh", "-c", script])
            .stdin(Stdio::null())
            .output()
            .expect("runs")
    }

    fn pid_in(&self, file: &str) -> Pid {
        let path = self.path(file);
        assert!(
            eventually(WAIT, || std::fs::read_to_string(&path)
                .is_ok_and(|text| text.trim().parse::<i32>().is_ok())),
            "{file} was never written"
        );
        let text = std::fs::read_to_string(&path).expect("reads");
        Pid::from_raw(text.trim().parse().expect("a pid"))
    }
}

/// Kills what is left behind by a test that failed or killed its host.
struct Cleanup(Vec<Pid>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for pid in &self.0 {
            let _ = kill(*pid, Signal::SIGKILL);
        }
    }
}

fn alive(pid: Pid) -> bool {
    kill(pid, None) != Err(Errno::ESRCH)
}

fn attach_piped(scratch: &Scratch, session: &str, flags: &[&str]) -> Child {
    scratch
        .command()
        .args(["session", "attach", session, "--size", "80x24"])
        .args(flags)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("runs")
}

fn read_until(child: &mut Child, marker: &str) -> String {
    let mut stdout = child.stdout.take().expect("piped");
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&heard);
    thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        while let Ok(n @ 1..) = stdout.read(&mut buffer) {
            if let Ok(mut heard) = sink.lock() {
                heard.extend_from_slice(&buffer[..n]);
            }
        }
    });
    let text = || String::from_utf8_lossy(&heard.lock().expect("lock")).into_owned();
    assert!(
        eventually(WAIT, || text().contains(marker)),
        "{marker:?} never came, got {:?}",
        text()
    );
    text()
}

#[test]
fn a_spawned_host_outlives_its_spawner_serves_a_client_and_leaves_no_zombie() {
    let scratch = Scratch::new();
    let started = scratch.host(
        "zombie",
        &format!(
            "echo $PPID > {}; echo marker; sleep 1; exit 0",
            scratch.path("host.pid").display()
        ),
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let host = scratch.pid_in("host.pid");
    let _cleanup = Cleanup(vec![host]);
    assert!(alive(host));
    assert_eq!(
        nix::unistd::getsid(Some(host)),
        Ok(host),
        "the host must lead a session of its own, with no controlling terminal"
    );
    assert_eq!(
        probe(&scratch.record("zombie")).expect("probes"),
        Liveness::Live
    );
    let mut client = attach_piped(&scratch, "zombie", &["--watch"]);
    let text = read_until(&mut client, "marker");
    assert!(text.contains("marker"));
    assert!(
        eventually(WAIT, || !alive(host)),
        "the host is still there or a zombie of something"
    );
    assert_eq!(
        probe(&scratch.record("zombie")).expect("probes"),
        Liveness::Dead
    );
    let status = client.wait().expect("ends");
    assert_eq!(status.code(), Some(0));
}

#[test]
fn a_host_killed_with_sigkill_leaves_a_lock_that_reads_dead_and_a_new_host_starts() {
    let scratch = Scratch::new();
    let script = format!(
        "echo $PPID > {host}; echo $$ > {child}; trap '' HUP; echo up; while :; do sleep 0.1; done",
        host = scratch.path("host.pid").display(),
        child = scratch.path("child.pid").display()
    );
    let started = scratch.host("crash", &script);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let host = scratch.pid_in("host.pid");
    let child = scratch.pid_in("child.pid");
    let _cleanup = Cleanup(vec![host, child]);
    let record = scratch.record("crash");
    assert_eq!(probe(&record).expect("probes"), Liveness::Live);

    kill(host, Signal::SIGKILL).expect("kills the host");
    assert!(
        eventually(WAIT, || probe(&record).is_ok_and(|l| l == Liveness::Dead)),
        "the lock outlived its host, so the program must have inherited it"
    );
    assert!(alive(child), "the program was meant to survive its host");
    assert!(
        scratch.path("r").join("crash.sock").exists(),
        "a killed host can't remove its socket"
    );

    let again = scratch
        .command()
        .args(["session", "host", "crash", "--spawn"])
        .stdin(Stdio::null())
        .output()
        .expect("runs");
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_eq!(probe(&record).expect("probes"), Liveness::Live);
    let mut client = attach_piped(&scratch, "crash", &["--watch"]);
    read_until(&mut client, "up");
    let _ = client.kill();
}

#[test]
fn a_session_with_a_live_host_is_not_started_twice() {
    let scratch = Scratch::new();
    let first = scratch.host("twice", "sleep 30");
    assert!(first.status.success());
    let again = scratch.host("twice", "sleep 30");
    assert!(!again.status.success());
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("already runs"),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
}

#[test]
fn a_socket_path_over_the_platform_limit_is_refused_with_the_lengths() {
    let scratch = Scratch::new();
    let long = scratch.path(&"d".repeat(100));
    let out = scratch
        .command()
        .env("AGENT_RUNTIME_DIR", long)
        .args(["session", "host", "toolong", "--spawn", "--", "true"])
        .stdin(Stdio::null())
        .output()
        .expect("runs");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("socket path"), "{stderr}");
    assert!(stderr.contains("platform allows"), "{stderr}");
}

#[test]
fn a_symlinked_runtime_directory_is_refused() {
    let scratch = Scratch::new();
    std::fs::create_dir(scratch.path("real")).expect("mkdir");
    std::os::unix::fs::symlink(scratch.path("real"), scratch.path("link")).expect("symlink");
    let out = scratch
        .command()
        .env("AGENT_RUNTIME_DIR", scratch.path("link"))
        .args(["session", "host", "linked", "--spawn", "--", "true"])
        .stdin(Stdio::null())
        .output()
        .expect("runs");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("symbolic link"));
}

#[test]
fn the_exit_status_of_the_harness_is_the_clients() {
    let scratch = Scratch::new();
    let started = scratch.host("status", "echo going; sleep 0.3; exit 5");
    assert!(started.status.success());
    let mut client = attach_piped(&scratch, "status", &[]);
    let status = client.wait().expect("ends");
    assert_eq!(status.code(), Some(5));
}

#[test]
fn the_program_runs_in_the_declared_environment_when_started_from_a_terminal_session() {
    let scratch = Scratch::new();
    scratch.sessions.lock().expect("lock").push("env".into());
    let out = scratch
        .command()
        .env("SSH_CONNECTION", "1.2.3.4")
        .env("TMUX", "/tmp/tmux")
        .env("WEZTERM_PANE", "4")
        .env("KEEP_ME", "yes")
        .args(["session", "host", "env", "--spawn", "--size", "80x24", "--", "sh", "-c"])
        .arg(r#"echo "T=$TERM S=$AGENT_SESSION K=$KEEP_ME SSH=${SSH_CONNECTION-unset} TMUX=${TMUX-unset} WEZ=${WEZTERM_PANE-unset}"; read x"#)
        .stdin(Stdio::null())
        .output()
        .expect("runs");
    assert!(out.status.success());
    let mut client = attach_piped(&scratch, "env", &["--watch"]);
    let text = read_until(&mut client, "WEZ=");
    assert!(
        text.contains("T=xterm-256color S=env K=yes SSH=unset TMUX=unset WEZ=unset"),
        "{text}"
    );
    let _ = client.kill();
}

struct OnPty {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    outer: Arc<Mutex<vt100::Parser>>,
}

fn attach_on_pty(scratch: &Scratch, session: &str, flags: &[&str]) -> OnPty {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("a pty");
    let mut command = CommandBuilder::new(AGENT);
    command.args(["session", "attach", session]);
    command.args(flags);
    command.cwd(scratch.path(""));
    command.env_clear();
    for (name, value) in [
        ("PATH", std::env::var_os("PATH").unwrap_or_default()),
        ("HOME", scratch.path("h").into()),
        ("XDG_STATE_HOME", scratch.path("s").into()),
        ("AGENT_RUNTIME_DIR", scratch.path("r").into()),
        ("TERM", "xterm-256color".into()),
    ] {
        command.env(name, value);
    }
    let child = pair.slave.spawn_command(command).expect("spawns");
    drop(pair.slave);
    let outer = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
    let mut reader = pair.master.try_clone_reader().expect("a reader");
    let sink = Arc::clone(&outer);
    thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        while let Ok(n @ 1..) = reader.read(&mut buffer) {
            if let Ok(mut outer) = sink.lock() {
                outer.process(&buffer[..n]);
            }
        }
    });
    OnPty {
        writer: pair.master.take_writer().expect("a writer"),
        master: pair.master,
        child,
        outer,
    }
}

impl OnPty {
    fn shows(&self, text: &str) -> bool {
        eventually(WAIT, || {
            self.outer
                .lock()
                .is_ok_and(|outer| outer.screen().contents().contains(text))
        })
    }

    fn outer<T>(&self, read: impl Fn(&vt100::Screen) -> T) -> T {
        read(self.outer.lock().expect("lock").screen())
    }
}

#[test]
fn on_a_terminal_the_client_replays_follows_resizes_and_puts_the_terminal_back_on_the_detach_key() {
    let scratch = Scratch::new();
    let script = r"
        printf '\033[?1049h\033[2J\033[Hscreen up\033[?25l\033[?1000h'
        trap 'echo SIZE $(stty size)' WINCH
        echo READY
        while :; do sleep 0.05; done
    ";
    let started = scratch.host("tty", script);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let mut client = attach_on_pty(&scratch, "tty", &[]);
    assert!(client.shows("READY"));
    assert!(client.outer(|screen| screen.alternate_screen() && screen.hide_cursor()));
    client
        .master
        .resize(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("resizes");
    assert!(client.shows("SIZE 30 100"));
    client
        .writer
        .write_all(b"\x1c")
        .expect("types the detach key");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = client.child.try_wait().expect("polls") {
            break status;
        }
        assert!(started.elapsed() < WAIT, "the client didn't leave");
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    assert!(
        client.outer(|screen| !screen.alternate_screen() && !screen.hide_cursor()),
        "the terminal wasn't put back"
    );
    assert_eq!(
        client.outer(vt100::Screen::mouse_protocol_mode),
        vt100::MouseProtocolMode::None
    );
}

#[test]
fn the_sandbox_the_tests_use_is_not_the_users() {
    let scratch = Scratch::new();
    let command = scratch.command();
    let envs: Vec<_> = command.get_envs().collect();
    assert!(envs.iter().any(|(name, _)| *name == "AGENT_RUNTIME_DIR"));
    assert!(
        !Path::new("/tmp")
            .join("agent-501")
            .starts_with(scratch.path("r"))
    );
}

#[test]
fn a_host_that_cannot_start_says_why_and_leaves_no_record_and_no_socket() {
    let scratch = Scratch::new();
    let broken = scratch
        .command()
        .args([
            "session",
            "host",
            "broken",
            "--spawn",
            "--",
            "/no/such/program",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("runs");
    assert!(!broken.status.success());
    let stderr = String::from_utf8_lossy(&broken.stderr);
    assert!(stderr.contains("/no/such/program"), "{stderr}");
    assert_eq!(
        probe(&scratch.record("broken")).expect("probes"),
        Liveness::Unrecorded
    );
    assert!(!scratch.path("r").join("broken.sock").exists());

    let nothing = scratch
        .command()
        .args(["session", "host", "nothing", "--spawn"])
        .stdin(Stdio::null())
        .output()
        .expect("runs");
    assert!(!nothing.status.success());
    assert!(String::from_utf8_lossy(&nothing.stderr).contains("no record"));
    assert_eq!(
        probe(&scratch.record("nothing")).expect("probes"),
        Liveness::Unrecorded
    );
}

#[test]
fn a_relative_runtime_directory_is_an_error() {
    let scratch = Scratch::new();
    let out = scratch
        .command()
        .env("AGENT_RUNTIME_DIR", "relative/dir")
        .args(["session", "host", "rel", "--spawn", "--", "true"])
        .stdin(Stdio::null())
        .output()
        .expect("runs");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("absolute"));
}

#[test]
fn a_sigterm_to_the_client_on_a_terminal_puts_the_terminal_back() {
    let scratch = Scratch::new();
    let script =
        r"printf '\033[?1049h\033[2J\033[Hscreen up\033[?25l\033[?1000h'; echo READY; read x";
    let started = scratch.host("term", script);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let mut client = attach_on_pty(&scratch, "term", &[]);
    assert!(client.shows("READY"));
    let pid = client.child.process_id().expect("a pid");
    kill(
        Pid::from_raw(i32::try_from(pid).expect("small")),
        Signal::SIGTERM,
    )
    .expect("signals");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = client.child.try_wait().expect("polls") {
            break status;
        }
        assert!(started.elapsed() < WAIT, "the client didn't end");
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.exit_code(), 143);
    assert!(
        client.outer(|screen| !screen.alternate_screen() && !screen.hide_cursor()),
        "the terminal wasn't put back"
    );
}

#[test]
fn a_host_stays_small_when_clients_state_huge_terminal_sizes() {
    let scratch = Scratch::new();
    let script = format!(
        "echo $PPID > {}; sleep 120",
        scratch.path("host.pid").display()
    );
    let started = scratch.host("small", &script);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let host = scratch.pid_in("host.pid");
    let _cleanup = Cleanup(vec![host]);
    let runtime = RuntimeDir::locate(Some(scratch.path("r")), None, Uid::current())
        .open(Creation::Existing)
        .expect("opens");
    let socket = runtime
        .socket(&"small".parse().expect("a name"))
        .expect("fits");
    let huge = agent_session::testing::size(20000, 20000);
    let hello = agent_host::Hello {
        version: agent_host::PROTOCOL_VERSION,
        role: Role::Window {
            take: agent_host::Take::Over,
            size: huge,
            colours: None,
        },
    };
    assert!(matches!(
        Conn::open_with(&socket, &hello),
        Err(agent_session::testing::OpenError::Refused(_))
    ));
    let mut owner = Conn::open(
        &socket,
        Role::Window {
            take: agent_host::Take::Over,
            size: agent_session::testing::size(24, 80),
            colours: None,
        },
    )
    .expect("a window");
    owner.send(&HostCommand::Resize(huge));
    assert!(owner.wait_closed(WAIT));
    let rss = Command::new("ps")
        .args(["-o", "rss=", "-p", &host.as_raw().to_string()])
        .output()
        .expect("ps");
    let kilobytes: u64 = String::from_utf8_lossy(&rss.stdout)
        .trim()
        .parse()
        .expect("a number");
    assert!(kilobytes < 200 * 1024, "the host grew to {kilobytes} KiB");
}

#[test]
fn attaching_to_a_session_that_isnt_there_writes_nothing_to_the_terminal() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.path("r")).expect("mkdir");
    let out = scratch
        .command()
        .args(["session", "attach", "absent", "--size", "80x24"])
        .stdin(Stdio::null())
        .output()
        .expect("runs");
    assert!(!out.status.success());
    assert_eq!(out.stdout, Vec::<u8>::new());
}

#[test]
fn a_sigterm_reaches_a_client_whose_host_has_stopped_reading() {
    let scratch = Scratch::new();
    let started = scratch.host("deafhost", "stty raw -echo; echo READY; sleep 600");
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let mut client = attach_on_pty(&scratch, "deafhost", &[]);
    assert!(client.shows("READY"));
    let mut typing = client.writer;
    thread::spawn(move || {
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..48 {
            if typing.write_all(&chunk).is_err() {
                return;
            }
        }
    });
    thread::sleep(Duration::from_millis(500));
    let pid = client.child.process_id().expect("a pid");
    kill(
        Pid::from_raw(i32::try_from(pid).expect("small")),
        Signal::SIGTERM,
    )
    .expect("signals");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = client.child.try_wait().expect("polls") {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the client didn't end"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.exit_code(), 143);
}

impl Scratch {
    fn run(&self, args: &[&str]) -> std::process::Output {
        self.command()
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("runs")
    }

    fn stdout(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "agent {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn dump_shows(&self, session: &str, flags: &[&str], text: &str) -> bool {
        eventually(WAIT, || {
            let mut args = vec!["dump", session];
            args.extend_from_slice(flags);
            let output = self.run(&args);
            String::from_utf8_lossy(&output.stdout).contains(text)
        })
    }
}

#[test]
fn a_plain_dump_keeps_the_gaps_a_program_drew_with_tabs_and_cursor_moves() {
    let scratch = Scratch::new();
    scratch.host("gaps", "printf 'a\\tb  c\\033[5Cd'; read x");
    assert!(scratch.dump_shows("gaps", &["--plain"], "d"));
    let plain = scratch.stdout(&["dump", "gaps", "--plain"]);
    assert_eq!(
        plain.lines().next(),
        Some("a       b  c     d"),
        "{plain:?}"
    );
}

#[test]
fn a_scrollback_dump_has_the_rows_that_scrolled_off_and_the_visible_one_does_not() {
    let scratch = Scratch::new();
    scratch.host("long", "seq 1 60 | sed s/^/line/; read x");
    assert!(scratch.dump_shows("long", &[], "line60"));
    let has_first = |text: &str| text.lines().any(|line| line == "line1");
    assert!(!has_first(&scratch.stdout(&["dump", "long", "--plain"])));
    let full = scratch.stdout(&["dump", "long", "--scrollback", "--plain"]);
    assert!(has_first(&full), "{full}");
    assert!(!full.contains('\x1b'), "plain text has no escapes");
}

#[test]
fn the_plugin_commands_list_names_attach_and_answer_menus_through_whichever_host_owns_the_session()
{
    let scratch = Scratch::new();
    scratch.host("menu", "printf READY; read x; echo got:$x; read y");
    assert!(scratch.dump_shows("menu", &[], "READY"));
    assert_eq!(scratch.stdout(&["plugin", "live"]), "menu\n");
    let with_directory = scratch.stdout(&["plugin", "live", "--cwd"]);
    assert!(with_directory.starts_with("menu\t"), "{with_directory:?}");
    let argv = scratch.stdout(&["plugin", "argv", "menu"]);
    let words: Vec<&str> = argv.lines().collect();
    assert!(
        words.contains(&"attach") && words.contains(&"menu"),
        "{words:?}"
    );
    assert!(words.contains(&"--detach-key"), "{words:?}");
    scratch.stdout(&["plugin", "choice", "menu", "3"]);
    assert!(scratch.dump_shows("menu", &[], "got:3"));
    for refused in ["0", "10", "x"] {
        assert!(
            !scratch
                .run(&["plugin", "choice", "menu", refused])
                .status
                .success(),
            "{refused}"
        );
    }
}

#[test]
fn a_prompt_that_starts_with_hyphens_is_typed_as_text_and_a_file_still_works() {
    let scratch = Scratch::new();
    scratch.host("typed", "stty -echo; printf READY; cat -v");
    assert!(scratch.dump_shows("typed", &[], "READY"));
    scratch.stdout(&["prompt", "typed", "-- a lua comment"]);
    assert!(scratch.dump_shows("typed", &["--plain"], "-- a lua comment"));
    scratch.stdout(&["prompt", "typed", "--", "- a list item"]);
    assert!(scratch.dump_shows("typed", &["--plain"], "- a list item"));
    let file = scratch.path("message.txt");
    std::fs::write(&file, "from a file").expect("writes");
    scratch.stdout(&["prompt", "typed", "--file", file.to_str().expect("utf8")]);
    assert!(scratch.dump_shows("typed", &["--plain"], "from a file"));
}

#[test]
fn piped_input_holding_the_detach_byte_reaches_the_program_and_does_not_detach() {
    let scratch = Scratch::new();
    let ready = scratch.path("ready");
    let heard = scratch.path("heard");
    scratch.host(
        "bytes",
        &format!(
            "stty raw -echo; touch {ready}; dd bs=1 count=3 2>/dev/null | od -An -c > {heard}; read x",
            ready = ready.display(),
            heard = heard.display()
        ),
    );
    assert!(eventually(WAIT, || ready.exists()));
    let mut client = attach_piped(&scratch, "bytes", &[]);
    client
        .stdin
        .as_mut()
        .expect("piped")
        .write_all(b"a\x1cb")
        .expect("writes");
    assert!(
        eventually(WAIT, || std::fs::read_to_string(&heard)
            .is_ok_and(|text| text.contains("034") && text.contains('b'))),
        "the program never got the byte: {:?}",
        std::fs::read_to_string(&heard)
    );
    assert_eq!(client.try_wait().expect("polls"), None, "still attached");
    let _ = client.kill();
    let _ = client.wait();
}

#[test]
fn a_host_starts_whatever_the_users_config_file_says() {
    let scratch = Scratch::new();
    let config = scratch.path("h/.config/agent");
    std::fs::create_dir_all(&config).expect("creates the config dir");
    std::fs::write(
        config.join("config.toml"),
        "[keys]\nprev_agent = \"alt-;\"\nnot toml at all {",
    )
    .expect("writes the config");
    let started = scratch.host("broken", "echo marker; sleep 1; exit 0");
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    assert_eq!(
        probe(&scratch.record("broken")).expect("probes"),
        Liveness::Live
    );
}
