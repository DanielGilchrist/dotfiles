//! The `Native` adapter against real hosts started by the built `agent`.
//! Every run has a scratch runtime and state directory under `/tmp`.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use agent_core::config::{HostKind, Osc52};
use agent_core::model::Running;
use agent_core::names::SessionName;
use agent_core::ports::{
    ClientControl, ClientId, HostError, Liveness, PaneCommand, Place, SessionHost, SessionStatus,
    Window, WindowHandle,
};
use agent_host::{HostFrame, Role, Take};
use agent_native::Native;
use agent_session::testing::{Conn, WAIT, eventually, size};
use agent_session::{Creation, HostFiles, RuntimeDir};
use nix::unistd::Uid;

const AGENT: &str = env!("CARGO_BIN_EXE_agent");

const ECHO: &str = "printf READY; while read line; do printf 'got:%s;' \"$line\"; done";

fn name(text: &str) -> SessionName {
    text.parse().expect("a session name")
}

struct Fixture {
    dir: tempfile::TempDir,
    native: Native,
    started: Mutex<Vec<SessionName>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let started = self.started.lock().map(|s| s.clone()).unwrap_or_default();
        for session in started {
            let _ = self.native.kill(&Running::for_testing(session));
        }
    }
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("n")
            .tempdir_in("/tmp")
            .expect("a temp dir");
        let native = Native::new(
            dir.path().join("s/agent"),
            RuntimeDir::locate(Some(dir.path().join("r")), None, Uid::current()),
            PathBuf::from(AGENT),
            Osc52::Off,
            agent_core::config::Detach::default(),
            |_| false,
        );
        Self {
            dir,
            native,
            started: Mutex::new(Vec::new()),
        }
    }

    fn files(&self) -> HostFiles {
        HostFiles::under(&self.dir.path().join("s/agent"))
    }

    fn socket(&self, session: &SessionName) -> agent_session::SocketPath {
        RuntimeDir::locate(Some(self.dir.path().join("r")), None, Uid::current())
            .open(Creation::Existing)
            .expect("a runtime directory")
            .socket(session)
            .expect("a socket path")
    }

    fn pane(&self, script: &str) -> PaneCommand {
        PaneCommand {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: self.dir.path().to_path_buf(),
        }
    }

    fn create(&self, session: &str, script: &str) -> agent_core::process::LaunchSpec {
        let session = name(session);
        self.started.lock().expect("lock").push(session.clone());
        let admitted = self
            .native
            .name_limit()
            .admit(session)
            .expect("within the limit");
        self.native
            .create(
                &admitted,
                &self.pane(script),
                agent_core::ports::Attachment::Window,
            )
            .expect("creates")
    }

    fn liveness(&self, session: &SessionName) -> Option<Liveness> {
        self.native
            .liveness(Duration::from_secs(1))
            .expect("lists")
            .into_iter()
            .find_map(|(listed, liveness)| (&listed == session).then_some(liveness))
    }

    fn screen_shows(&self, session: &SessionName, text: &str) -> bool {
        eventually(WAIT, || {
            self.native
                .screen(
                    session,
                    agent_core::ports::Extent::Visible,
                    agent_core::ports::Rendering::Styled,
                )
                .is_ok_and(|screen| screen.contains(text))
        })
    }

    fn kill(&self, session: &SessionName) {
        self.native
            .kill(&Running::for_testing(session.clone()))
            .expect("kills");
    }
}

#[test]
fn a_created_session_lists_live_where_it_runs_and_shows_what_it_printed() {
    let f = Fixture::new();
    let spec = f.create("fresh", ECHO);
    assert_eq!(spec.working_dir(), Some(f.dir.path()));
    let fresh = name("fresh");
    assert_eq!(f.liveness(&fresh), Some(Liveness::Live));
    let listed = f.native.sessions().expect("lists");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].status,
        SessionStatus::Live {
            cwd: Some(f.dir.path().to_path_buf())
        }
    );
    assert!(f.screen_shows(&fresh, "READY"));
}

#[test]
fn typing_into_a_session_reaches_its_program() {
    let f = Fixture::new();
    f.create("typed", ECHO);
    let typed = name("typed");
    assert!(f.screen_shows(&typed, "READY"));
    f.native.write(&typed, b"hello\n").expect("writes");
    assert!(f.screen_shows(&typed, "got:hello;"));
    let big = "x".repeat(3_000_000);
    f.native
        .write(&typed, format!("{big}\n").as_bytes())
        .expect("writes more than a frame holds");
}

#[test]
fn a_name_over_zellijs_twenty_bytes_creates_lists_and_runs() {
    let f = Fixture::new();
    let long = "a-native-session-name-of-thirty";
    f.create(long, ECHO);
    let listed = f.native.sessions().expect("lists");
    assert_eq!(listed[0].name.as_str(), long);
    assert!(f.screen_shows(&name(long), "READY"));
}

#[test]
fn only_windows_are_listed_and_closing_one_tells_it_and_leaves_a_preview_of_its_process_alone() {
    let f = Fixture::new();
    f.create("shown", ECHO);
    let shown = name("shown");
    assert!(f.screen_shows(&shown, "READY"));
    let socket = f.socket(&shown);
    let mut window = Conn::open(
        &socket,
        Role::Window {
            take: Take::Over,
            size: size(24, 80),
            colours: None,
        },
    )
    .expect("a window");
    let mut preview = Conn::open(
        &socket,
        Role::Preview {
            size: size(24, 80),
            colours: None,
        },
    )
    .expect("a preview");
    let _phone = Conn::open(
        &socket,
        Role::Watch {
            purpose: agent_host::Purpose::Watch,
        },
    )
    .expect("a watcher");
    let windows = f.native.windows(None).expect("lists windows");
    assert_eq!(windows.len(), 1, "a watcher is no window: {windows:?}");
    assert_eq!(windows[0].session, shown);
    assert_eq!(windows[0].client.get(), std::process::id());
    assert!(
        f.native
            .windows(Some(ClientId::new(std::process::id())))
            .expect("lists windows")
            .is_empty(),
        "the caller's own client is not another window"
    );
    f.native.detach(&windows).expect("detaches");
    assert!(
        window
            .wait_event(WAIT, |frame| matches!(frame, HostFrame::Detached { .. }))
            .is_some()
    );
    assert!(
        preview
            .wait_event(Duration::from_millis(300), |frame| matches!(
                frame,
                HostFrame::Detached { .. }
            ))
            .is_none(),
        "a preview of the same process is not the window that was closed"
    );
}

#[test]
fn detaching_a_window_of_a_host_that_has_gone_is_not_an_error_and_of_another_host_is() {
    let f = Fixture::new();
    f.create("quiet", ECHO);
    let quiet = name("quiet");
    let window = |host| Window {
        client: ClientId::new(1),
        session: quiet.clone(),
        place: Place::Detached,
        handle: WindowHandle::new(host, 1),
    };
    assert!(matches!(
        f.native.detach(&[window(HostKind::Zellij)]),
        Err(HostError::Detach { .. })
    ));
    f.kill(&quiet);
    f.native
        .detach(&[window(HostKind::Native)])
        .expect("the window went with its host");
}

#[test]
fn a_killed_session_lists_exited_and_resurrects_from_its_record() {
    let f = Fixture::new();
    f.create("again", ECHO);
    let again = name("again");
    assert!(f.screen_shows(&again, "READY"));
    f.kill(&again);
    assert_eq!(f.liveness(&again), Some(Liveness::Exited));
    assert!(
        f.native
            .screen(
                &again,
                agent_core::ports::Extent::Visible,
                agent_core::ports::Rendering::Styled
            )
            .is_err()
    );
    f.kill(&again);
    let listed = f.native.sessions().expect("lists");
    assert_eq!(
        listed[0].status,
        SessionStatus::Exited {
            cwd: Some(f.dir.path().to_path_buf())
        }
    );
    f.native.start_detached(&again).expect("resurrects");
    assert_eq!(f.liveness(&again), Some(Liveness::Live));
    assert!(f.screen_shows(&again, "READY"));
    f.native
        .start_detached(&again)
        .expect("a live session is left alone");
}

#[test]
fn attaching_asks_for_the_session_to_be_started_if_it_is_not_running() {
    let f = Fixture::new();
    let spec = f
        .native
        .attach(&name("any"), agent_core::ports::Attachment::Window);
    let argv: Vec<String> = spec
        .argv()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(&argv[1..4], ["session", "attach", "any"]);
    assert!(argv.contains(&"--start".to_owned()), "{argv:?}");
}

#[test]
fn a_window_attaches_with_the_detach_key_and_a_preview_without_one() {
    let f = Fixture::new();
    let argv = |how| -> Vec<String> {
        f.native
            .attach(&name("any"), how)
            .argv()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    };
    let window = argv(agent_core::ports::Attachment::Window);
    assert!(
        window.join(" ").contains("--detach-key ctrl-\\"),
        "{window:?}"
    );
    assert!(!window.contains(&"--as".to_owned()), "{window:?}");
    let preview = argv(agent_core::ports::Attachment::Preview);
    assert!(
        preview.join(" ").contains("--as preview --detach-key none"),
        "{preview:?}"
    );
}

#[test]
fn a_session_with_no_record_cannot_be_started() {
    let f = Fixture::new();
    assert!(f.native.start_detached(&name("never")).is_err());
    assert_eq!(f.native.sessions().expect("lists"), Vec::new());
}

#[test]
fn a_record_whose_host_is_gone_is_an_exited_session_and_other_files_are_not_sessions() {
    let f = Fixture::new();
    let files = f.files();
    files.create_dir().expect("a host directory");
    let stale = r#"{"program":"true","args":[],"cwd":"/old/place"}"#;
    std::fs::write(files.record(&name("stale")), stale).expect("writes");
    std::fs::write(files.dir().join("stale.log"), "x").expect("writes");
    std::fs::write(files.dir().join("not a name.json"), "{}").expect("writes");
    let listed = f.native.sessions().expect("lists");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].status,
        SessionStatus::Exited {
            cwd: Some("/old/place".into())
        }
    );
    f.native.delete(&name("stale")).expect("deletes");
    assert_eq!(f.native.sessions().expect("lists"), Vec::new());
    assert!(!files.log(&name("stale")).exists());
}

#[test]
fn a_session_with_no_state_directory_has_no_sessions() {
    let f = Fixture::new();
    assert_eq!(f.native.sessions().expect("lists"), Vec::new());
    assert_eq!(
        f.native.liveness(Duration::from_secs(1)).expect("lists"),
        Vec::new()
    );
    assert_eq!(f.native.windows(None).expect("lists"), Vec::new());
}

#[test]
fn deleting_a_live_session_stops_it_and_forgets_it() {
    let f = Fixture::new();
    f.create("gone", ECHO);
    let gone = name("gone");
    assert!(f.screen_shows(&gone, "READY"));
    f.native.delete(&gone).expect("deletes");
    assert_eq!(f.liveness(&gone), None);
    assert!(!f.files().record(&gone).exists());
}

#[test]
fn creating_over_a_dead_session_replaces_its_command_and_over_a_live_one_fails() {
    let f = Fixture::new();
    f.create("swap", ECHO);
    let swap = name("swap");
    assert!(f.screen_shows(&swap, "READY"));
    let admitted = f.native.name_limit().admit(swap.clone()).expect("admitted");
    assert!(
        f.native
            .create(
                &admitted,
                &f.pane(ECHO),
                agent_core::ports::Attachment::Window
            )
            .is_err()
    );
    f.kill(&swap);
    f.native
        .create(
            &admitted,
            &f.pane("printf SECOND; sleep 30"),
            agent_core::ports::Attachment::Window,
        )
        .expect("replaces");
    assert!(f.screen_shows(&swap, "SECOND"));
}

#[test]
fn watching_a_session_is_a_peek_at_the_size_it_has() {
    let f = Fixture::new();
    f.create("peeked", ECHO);
    let peeked = name("peeked");
    let observation = f.native.observe(&peeked).expect("observes");
    assert_eq!(
        (observation.size.cols.get(), observation.size.rows.get()),
        (80, 24)
    );
    let argv: Vec<String> = observation
        .spec
        .argv()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        argv.windows(2).any(|pair| pair == ["--as", "peek"]),
        "{argv:?}"
    );
    assert!(argv.contains(&"none".to_owned()));
    assert!(argv.windows(2).any(|pair| pair == ["--size", "80x24"]));
    f.kill(&peeked);
    assert!(f.native.observe(&peeked).is_err());
}

#[test]
fn the_name_limit_is_forty_eight_or_what_the_runtime_directory_leaves() {
    let limit = |dir: &str| {
        Native::new(
            "/state/agent".into(),
            RuntimeDir::locate(Some(dir.into()), None, Uid::current()),
            PathBuf::from(AGENT),
            Osc52::Off,
            agent_core::config::Detach::default(),
            |_| false,
        )
        .name_limit()
        .bytes()
    };
    assert_eq!(limit("/tmp/agent-501"), 48);
    let long = format!("/tmp/{}", "d".repeat(70));
    let budget = limit(&long);
    assert!(budget < 48, "{budget}");
    assert_eq!(budget, 103 - long.len() - "/.sock".len());
}

#[test]
fn a_name_over_the_limit_is_refused_before_a_host_is_started() {
    let f = Fixture::new();
    let wide =
        agent_core::names::NameLimit::new(std::num::NonZeroUsize::new(200).expect("nonzero"));
    let too_long = wide
        .admit(name(&"n".repeat(100)))
        .expect("admitted by a wider limit");
    let refused = f
        .native
        .create(
            &too_long,
            &f.pane(ECHO),
            agent_core::ports::Attachment::Window,
        )
        .expect_err("over this host's limit");
    assert!(matches!(refused, HostError::Name(_)), "{refused}");
    assert_eq!(f.native.sessions().expect("lists"), Vec::new());
}

#[test]
fn the_attach_command_starts_an_exited_session_from_its_record() {
    let f = Fixture::new();
    f.create("revive", ECHO);
    let revive = name("revive");
    f.kill(&revive);
    assert_eq!(f.liveness(&revive), Some(Liveness::Exited));
    let spec = f
        .native
        .attach(&revive, agent_core::ports::Attachment::Preview);
    let mut argv = spec.argv();
    let program = argv.next().expect("a program");
    let mut attach = std::process::Command::new(program);
    attach
        .args(argv)
        .env("AGENT_RUNTIME_DIR", f.dir.path().join("r"))
        .env("XDG_STATE_HOME", f.dir.path().join("s"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = attach.spawn().expect("starts the client");
    let live = eventually(WAIT, || f.liveness(&revive) == Some(Liveness::Live));
    let _ = child.kill();
    let _ = child.wait();
    assert!(live, "the client started the host");
    assert!(f.screen_shows(&revive, "READY"));
}

#[test]
fn a_panes_environment_loses_the_session_name_a_native_host_sets() {
    let f = Fixture::new();
    assert_eq!(f.native.nesting_env(), ["AGENT_SESSION"]);
}
