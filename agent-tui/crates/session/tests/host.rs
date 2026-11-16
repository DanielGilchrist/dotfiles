use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use agent_host::{
    ClientInfo, Command, ExitStatus, Greeting, HelloRefusal, HostFrame, Payload, Purpose, Role,
    Scrollback, Standing, Style, Take, Version,
};
use agent_session::testing::{
    Conn, Heard, Sandbox, WAIT, eventually, screen_differences, session, size,
};
use agent_session::{HostError, Launch, Liveness, LockError, probe};

const SLEEPS: &str = "while :; do sleep 0.05; done";

fn window(take: Take, rows: u16, cols: u16) -> Role {
    Role::Window {
        take,
        size: size(rows, cols),
        colours: None,
    }
}

fn preview(rows: u16, cols: u16) -> Role {
    Role::Preview {
        size: size(rows, cols),
        colours: None,
    }
}

fn watch() -> Role {
    Role::Watch {
        purpose: Purpose::Window,
    }
}

fn open(host: &agent_session::testing::RunningHost, role: Role) -> Conn {
    Conn::open(&host.socket, role).expect("the host welcomes it")
}

const SECOND: Duration = Duration::from_secs(1);

#[test]
fn a_client_that_attaches_mid_stream_replays_to_the_same_screen_cell_for_cell() {
    let sandbox = Sandbox::new();
    let script = r"
        printf '\033[?1049h\033[2J'
        i=0
        while [ $i -lt 40 ]; do
            printf '\033[%d;%dH\033[3%dm\033[1mframe %d\033[0m' $((i % 20 + 1)) $((i % 30 + 1)) $((i % 7 + 1)) $i
            i=$((i + 1)); sleep 0.03
        done
        printf '\033[1;1H\033[4mREADY\033[0m'
        read x
    ";
    let host = sandbox.host("replay", script);
    let mut early = open(&host, watch());
    assert!(early.wait_screen(WAIT, |text| text.contains("frame 5")));
    let mut late = open(&host, watch());
    assert!(
        !late.mirror.screen().contents().contains("READY"),
        "the late client should have attached mid stream"
    );
    assert!(early.wait_screen(WAIT, |text| text.contains("READY")));
    assert!(late.wait_screen(WAIT, |text| text.contains("READY")));
    assert_eq!(
        screen_differences(early.mirror.screen(), late.mirror.screen()),
        Vec::<String>::new()
    );
}

#[test]
fn a_client_that_attaches_during_an_alternate_screen_program_sees_the_main_screen_after_it_quits() {
    let sandbox = Sandbox::new();
    let flag = sandbox.state.join("leave");
    let script = format!(
        r"
        printf 'main line one\r\nmain line two\r\n'
        printf '\033[?1049h\033[2J\033[Halt screen'
        while [ ! -f {flag} ]; do sleep 0.02; done
        printf '\033[?1049l'
        printf 'after\r\nREADY'
        read x
        ",
        flag = flag.display()
    );
    std::fs::create_dir_all(&sandbox.state).expect("state dir");
    let host = sandbox.host("mainscreen", &script);
    let mut early = open(&host, watch());
    assert!(early.wait_screen(WAIT, |text| text.contains("alt screen")));
    let mut late = open(&host, watch());
    assert!(late.wait_screen(WAIT, |text| text.contains("alt screen")));
    assert!(late.mirror.screen().alternate_screen());
    std::fs::write(&flag, "").expect("flag");
    assert!(early.wait_screen(WAIT, |text| text.contains("READY")));
    assert!(late.wait_screen(WAIT, |text| text.contains("READY")));
    assert!(late.mirror.screen().contents().contains("main line two"));
    assert_eq!(
        screen_differences(early.mirror.screen(), late.mirror.screen()),
        Vec::<String>::new()
    );
}

#[test]
fn the_first_window_owns_and_a_window_takes_ownership_from_a_preview() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("seats", SLEEPS);
    let mut preview = open(&host, preview(20, 60));
    assert_eq!(preview.welcome.standing, Standing::Owner);
    let window = open(&host, window(Take::Politely, 30, 100));
    assert_eq!(window.welcome.standing, Standing::Owner);
    assert_eq!(window.welcome.size, size(30, 100));
    let Some(HostFrame::Demoted { by }) = preview.next_event(WAIT) else {
        panic!("the preview should be told");
    };
    assert_eq!(by.purpose, Some(Purpose::Window));
    assert_eq!(by.standing, Standing::Owner);
    assert_eq!(by.client, window.welcome.you);
}

#[test]
fn a_preview_does_not_displace_a_window_and_a_polite_window_waits() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("polite", SLEEPS);
    let first = open(&host, window(Take::Over, 30, 100));
    let preview = open(&host, preview(20, 60));
    let polite = open(&host, window(Take::Politely, 25, 90));
    assert_eq!(first.welcome.standing, Standing::Owner);
    assert_eq!(preview.welcome.standing, Standing::Watcher);
    assert_eq!(polite.welcome.standing, Standing::Watcher);
    assert_eq!(polite.welcome.size, size(30, 100));
}

#[test]
fn a_window_taking_over_demotes_the_owner_who_claims_back_with_a_keypress_equivalent() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("takeover", SLEEPS);
    let mut first = open(&host, window(Take::Over, 30, 100));
    let mut second = open(&host, window(Take::Over, 40, 120));
    assert_eq!(second.welcome.standing, Standing::Owner);
    let Some(HostFrame::Demoted { by }) = first.next_event(WAIT) else {
        panic!("the old owner should be told");
    };
    assert_eq!(by.client, second.welcome.you);
    first.send(&Command::Claim(size(30, 100)));
    assert_eq!(first.next_event(WAIT), Some(HostFrame::Promoted));
    let Some(HostFrame::Demoted { by }) = second.next_event(WAIT) else {
        panic!("the one that took over should be told");
    };
    assert_eq!(by.client, first.welcome.you);
}

#[test]
fn a_preview_cannot_claim_from_a_window() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("previewclaim", SLEEPS);
    let _window = open(&host, window(Take::Over, 30, 100));
    let mut preview = open(&host, preview(20, 60));
    preview.send(&Command::Claim(size(20, 60)));
    assert_eq!(
        preview.next_event(WAIT),
        Some(HostFrame::Forbidden {
            command: agent_host::CommandKind::Claim
        })
    );
}

#[test]
fn when_the_owner_leaves_a_waiting_preview_is_promoted_and_sizes_the_session() {
    let sandbox = Sandbox::new();
    let script = r"trap 'echo SIZE $(stty size)' WINCH; echo READY; while :; do sleep 0.05; done";
    let host = sandbox.host("promote", script);
    let mut ready = open(&host, watch());
    assert!(ready.wait_screen(WAIT, |text| text.contains("READY")));
    let mut owner = open(&host, window(Take::Over, 30, 100));
    let mut preview = open(&host, preview(20, 60));
    assert!(owner.wait_screen(WAIT, |text| text.contains("SIZE 30 100")));
    owner.send(&Command::Bye);
    assert_eq!(preview.next_event(WAIT), Some(HostFrame::Promoted));
    assert!(preview.wait_screen(WAIT, |text| text.contains("SIZE 20 60")));
}

#[test]
fn a_watcher_cannot_resize_or_type_and_the_owner_can() {
    let sandbox = Sandbox::new();
    let script = r"trap 'echo SIZE $(stty size)' WINCH; echo READY; while :; do sleep 0.05; done";
    let host = sandbox.host("watcher", script);
    let mut ready = open(&host, watch());
    assert!(ready.wait_screen(WAIT, |text| text.contains("READY")));
    let mut owner = open(&host, window(Take::Over, 30, 100));
    let mut watcher = open(&host, watch());
    assert!(owner.wait_screen(WAIT, |text| text.contains("SIZE 30 100")));
    watcher.send(&Command::Resize(size(10, 10)));
    watcher.send(&Command::Input(
        Payload::new(b"typed".to_vec()).expect("fits"),
    ));
    assert_eq!(
        watcher.next_event(WAIT),
        Some(HostFrame::Forbidden {
            command: agent_host::CommandKind::Resize
        })
    );
    assert_eq!(
        watcher.next_event(WAIT),
        Some(HostFrame::Forbidden {
            command: agent_host::CommandKind::Input
        })
    );
    owner.send(&Command::Resize(size(31, 101)));
    assert!(watcher.wait_screen(WAIT, |text| text.contains("SIZE 31 101")));
    assert!(!watcher.mirror.screen().contents().contains("SIZE 10 10"));
}

#[test]
fn only_the_owner_types() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("typing", "stty raw -echo; echo READY; cat");
    let mut owner = open(&host, window(Take::Over, 24, 80));
    let mut watcher = open(&host, watch());
    assert!(owner.wait_screen(WAIT, |text| text.contains("READY")));
    watcher.send(&Command::Input(
        Payload::new(b"nope".to_vec()).expect("fits"),
    ));
    assert!(matches!(
        watcher.next_event(WAIT),
        Some(HostFrame::Forbidden { .. })
    ));
    owner.send(&Command::Input(
        Payload::new(b"typed".to_vec()).expect("fits"),
    ));
    assert!(watcher.wait_screen(WAIT, |text| text.contains("typed")));
    assert!(!watcher.mirror.screen().contents().contains("nope"));
}

#[test]
fn input_can_be_injected_without_attaching() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("inject", "stty raw -echo; echo READY; cat");
    let mut control = open(&host, Role::Control);
    assert_eq!(control.mirror.screen().contents(), "");
    assert!(eventually(WAIT, || control
        .snapshot_text(Scrollback::Screen)
        .contains("READY")));
    control.send(&Command::Inject(
        Payload::new(b"injected text".to_vec()).expect("fits"),
    ));
    let seen = eventually(WAIT, || {
        control
            .snapshot_text(Scrollback::Screen)
            .contains("injected text")
    });
    assert!(seen);
    assert!(
        control.mirror.screen().contents().is_empty(),
        "a control client is sent no output"
    );
}

#[test]
fn a_watcher_can_inject_too() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("watchinject", "stty raw -echo; echo READY; cat");
    let mut watcher = open(&host, watch());
    assert!(watcher.wait_screen(WAIT, |text| text.contains("READY")));
    watcher.send(&Command::Inject(
        Payload::new(b"from a watcher".to_vec()).expect("fits"),
    ));
    assert!(watcher.wait_screen(WAIT, |text| text.contains("from a watcher")));
}

#[test]
fn clients_lists_everyone_including_the_asker_and_the_owner_can_detach_another() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("clients", SLEEPS);
    let mut owner = open(&host, window(Take::Over, 24, 80));
    let mut watcher = open(&host, watch());
    watcher.send(&Command::Clients);
    let Some(HostFrame::Clients(list)) = watcher.next_event(WAIT) else {
        panic!("a listing");
    };
    let ours: Vec<&ClientInfo> = list
        .as_slice()
        .iter()
        .filter(|info| info.client == watcher.welcome.you)
        .collect();
    assert_eq!(ours.len(), 1);
    assert_eq!(list.as_slice().len(), 2);
    assert_eq!(ours[0].standing, Standing::Watcher);

    owner.send(&Command::Detach(owner.welcome.you));
    assert!(matches!(
        owner.next_event(WAIT),
        Some(HostFrame::Forbidden { .. })
    ));
    owner.send(&Command::Detach(watcher.welcome.you));
    let Some(HostFrame::Detached { by }) = watcher.next_event(WAIT) else {
        panic!("the detached client is told");
    };
    assert_eq!(by.client, owner.welcome.you);
    assert!(watcher.wait_closed(WAIT));
}

#[test]
fn a_control_client_can_detach_the_owner_and_a_watcher_cannot() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("detach", SLEEPS);
    let mut owner = open(&host, window(Take::Over, 24, 80));
    let mut watcher = open(&host, watch());
    watcher.send(&Command::Detach(owner.welcome.you));
    assert!(matches!(
        watcher.next_event(WAIT),
        Some(HostFrame::Forbidden { .. })
    ));
    let mut control = open(&host, Role::Control);
    control.send(&Command::Detach(owner.welcome.you));
    assert!(matches!(
        owner.next_event(WAIT),
        Some(HostFrame::Detached { .. })
    ));
}

#[test]
fn the_harness_exiting_tells_clients_then_takes_the_socket_and_the_lock_with_it() {
    let sandbox = Sandbox::new();
    let name = session("exits");
    let gate = sandbox.gate("exit");
    let host = sandbox.host("exits", &format!("{}; echo bye; exit 7", gate.wait()));
    let mut client = open(&host, watch());
    gate.open();
    assert_eq!(
        client.wait_event(WAIT, |frame| matches!(frame, HostFrame::Exited(_))),
        Some(HostFrame::Exited(ExitStatus::Code(7)))
    );
    assert!(client.mirror.screen().contents().contains("bye"));
    let socket = host.socket.clone();
    assert_eq!(host.finish().expect("ends cleanly"), ExitStatus::Code(7));
    assert!(!socket.as_path().exists());
    assert_eq!(
        probe(&sandbox.files.record(&name)).expect("probes"),
        Liveness::Dead
    );
}

#[test]
fn a_client_still_connected_can_take_a_snapshot_after_the_exit() {
    let sandbox = Sandbox::new();
    let gate = sandbox.gate("exit");
    let script = format!("{}; echo last words; exit 0", gate.wait());
    let mut config = sandbox.config(&session("late"), &script);
    config.timing.drain = Duration::from_secs(2);
    let host = sandbox.start(config);
    let mut control = open(&host, Role::Control);
    gate.open();
    assert_eq!(
        control.next_event(WAIT),
        Some(HostFrame::Exited(ExitStatus::Code(0)))
    );
    assert!(
        control
            .snapshot_text(Scrollback::Screen)
            .contains("last words")
    );
}

#[test]
fn the_session_can_be_started_again_while_the_old_host_still_drains() {
    let sandbox = Sandbox::new();
    let name = session("resume");
    let gate = sandbox.gate("exit");
    let script = format!("{}; echo first run; exit 0", gate.wait());
    let mut config = sandbox.config(&name, &script);
    config.timing.drain = Duration::from_secs(3);
    let first = sandbox.start(config);
    let mut control = open(&first, Role::Control);
    gate.open();
    assert!(
        control
            .wait_event(WAIT, |frame| matches!(frame, HostFrame::Exited(_)))
            .is_some()
    );
    let started = Instant::now();
    assert!(
        eventually(WAIT, || probe(&sandbox.files.record(&name))
            .is_ok_and(|liveness| liveness == Liveness::Dead)),
        "the lock was kept for the drain"
    );
    let second = sandbox.host("resume", "echo second run; read x");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "resume waited {:?}",
        started.elapsed()
    );
    let mut watcher = open(&second, watch());
    assert!(watcher.wait_screen(WAIT, |text| text.contains("second run")));
    assert!(
        control
            .snapshot_text(Scrollback::Screen)
            .contains("first run"),
        "the old host still serves the clients it has"
    );
    assert!(
        second.socket.as_path().exists(),
        "the old host took the new host's socket away"
    );
    assert!(first.finish().is_ok());
    assert!(second.socket.as_path().exists());
    assert!(Conn::open(&second.socket, Role::Control).is_ok());
}

#[test]
fn kill_ends_a_harness_that_ignores_hangup_after_the_grace() {
    let sandbox = Sandbox::new();
    let host = sandbox.host(
        "stubborn",
        "trap '' HUP; echo ready; while :; do sleep 0.05; done",
    );
    let mut watcher = open(&host, watch());
    assert!(watcher.wait_screen(WAIT, |text| text.contains("ready")));
    let started = Instant::now();
    let mut control = open(&host, Role::Control);
    control.send(&Command::Kill);
    let Some(HostFrame::Exited(status)) =
        watcher.wait_event(WAIT, |frame| matches!(frame, HostFrame::Exited(_)))
    else {
        panic!("the harness should end");
    };
    assert!(
        matches!(status, ExitStatus::Signal(signal) if signal.get() == 9),
        "{status:?}"
    );
    assert!(started.elapsed() >= Duration::from_millis(350));
    assert!(host.finish().is_ok());
}

#[test]
fn kill_ends_a_harness_that_obeys_hangup_at_once() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("obedient", "sleep 60");
    let mut control = open(&host, Role::Control);
    let started = Instant::now();
    control.send(&Command::Kill);
    let Some(HostFrame::Exited(status)) =
        control.wait_event(WAIT, |frame| matches!(frame, HostFrame::Exited(_)))
    else {
        panic!("the harness should end");
    };
    assert!(
        matches!(status, ExitStatus::Signal(signal) if signal.get() == 1),
        "{status:?}"
    );
    assert!(started.elapsed() < Duration::from_millis(350));
}

#[test]
fn a_socket_file_that_is_deleted_is_bound_again() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("rebound", SLEEPS);
    std::fs::remove_file(host.socket.as_path()).expect("removes the socket");
    assert!(
        eventually(WAIT, || Conn::open(&host.socket, Role::Control).is_ok()),
        "the host never made its socket reachable again"
    );
}

#[test]
fn a_connection_that_never_says_hello_is_refused_after_the_timeout() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("quiet", SLEEPS);
    let mut stream = UnixStream::connect(host.socket.as_path()).expect("connects");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let started = Instant::now();
    let greeting = agent_host::read_frame::<Greeting>(&mut stream).expect("a frame");
    assert_eq!(
        greeting,
        Some(Greeting::Refused(HelloRefusal::HelloTimeout))
    );
    assert!(started.elapsed() >= Duration::from_millis(300));
}

#[test]
fn a_malformed_hello_is_refused_and_a_malformed_command_closes_the_connection() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("garbage", SLEEPS);
    let mut stream = UnixStream::connect(host.socket.as_path()).expect("connects");
    stream.set_read_timeout(Some(WAIT)).expect("timeout");
    std::io::Write::write_all(&mut stream, &[0, 0, 0, 1, 0x7f]).expect("writes");
    assert_eq!(
        agent_host::read_frame::<Greeting>(&mut stream).expect("a frame"),
        Some(Greeting::Refused(HelloRefusal::Malformed))
    );

    let mut control = open(&host, Role::Control);
    control.send_bytes(&[0, 0, 0, 1, 0x7e]);
    assert!(control.wait_closed(WAIT));
    let mut oversized = open(&host, Role::Control);
    oversized.send_bytes(&[0xff, 0xff, 0xff, 0xff]);
    assert!(oversized.wait_closed(WAIT));
    assert!(
        Conn::open(&host.socket, Role::Control).is_ok(),
        "the host still serves"
    );
}

#[test]
fn a_hello_for_another_protocol_version_is_refused_with_the_versions_the_host_speaks() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("version", SLEEPS);
    let hello = agent_host::Hello {
        version: Version::new(9),
        role: Role::Control,
    };
    let Err(agent_session::testing::OpenError::Refused(HelloRefusal::Version { supported })) =
        Conn::open_with(&host.socket, &hello)
    else {
        panic!("refused for its version");
    };
    assert_eq!(supported, agent_host::SUPPORTED);
}

#[test]
fn a_second_host_for_a_running_session_is_refused() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("twice", SLEEPS);
    let again = sandbox.config(&session("twice"), "true");
    assert!(matches!(
        agent_session::serve(again),
        Err(HostError::Lock(LockError::Held))
    ));
    assert!(Conn::open(&host.socket, Role::Control).is_ok());
}

#[test]
fn the_program_gets_the_declared_environment_and_not_the_spawners_terminal() {
    let sandbox = Sandbox::new();
    let mut config = sandbox.config(
        &session("envs"),
        r#"echo "T=$TERM C=$COLORTERM S=$AGENT_SESSION F=$FOO"; echo "SSH=[${SSH_CONNECTION-unset}] TMUX=[${TMUX-unset}] KITTY=[${KITTY_PID-unset}] WEZ=[${WEZTERM_PANE-unset}]"; read x"#,
    );
    config.inherited.extend(
        [
            ("TERM", "dumb"),
            ("SSH_CONNECTION", "1.2.3.4"),
            ("TMUX", "/tmp/tmux"),
            ("KITTY_PID", "9"),
            ("WEZTERM_PANE", "3"),
            ("FOO", "bar"),
        ]
        .map(|(name, value)| (name.into(), value.into())),
    );
    let host = sandbox.start(config);
    let mut client = open(&host, watch());
    assert!(client.wait_screen(WAIT, |text| text.contains("KITTY=")));
    let text = client.mirror.screen().contents();
    assert!(
        text.contains("T=xterm-256color C=truecolor S=envs F=bar"),
        "{text}"
    );
    assert!(
        text.contains("SSH=[unset] TMUX=[unset] KITTY=[unset] WEZ=[unset]"),
        "{text}"
    );
}

#[test]
fn a_client_that_stops_reading_is_closed_and_the_others_keep_getting_output() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("flood", "yes abcdefghijklmnopqrstuvwxyz0123456789");
    let mut stalled = open(&host, watch());
    let mut reading = open(&host, watch());
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(4) {
        for _ in 0..50 {
            reading.pump(Duration::from_millis(2));
        }
    }
    assert_eq!(reading.pump(SECOND), Heard::Frame);
    assert!(stalled.wait_closed(WAIT));
}

#[test]
fn the_socket_is_private_to_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    let host = sandbox.host("private", SLEEPS);
    let mode = |path: &std::path::Path| {
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
    };
    assert_eq!(mode(host.socket.as_path()), 0o600);
    assert_eq!(mode(&sandbox.files.record(&session("private"))), 0o600);
    assert_eq!(mode(&sandbox.files.log(&session("private"))), 0o600);
}

fn wait_for_flag_script(go: &std::path::Path, then: &str) -> String {
    format!(
        r"
        while [ ! -f {go} ]; do sleep 0.02; done
        {then}
        ",
        go = go.display()
    )
}

#[test]
fn a_client_that_arrives_during_a_synchronized_update_gets_the_finished_frame() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(&sandbox.state).expect("state dir");
    let (go, finish) = (sandbox.state.join("go"), sandbox.state.join("finish"));
    let script = wait_for_flag_script(
        &go,
        &format!(
            r"printf '\033[?2026hhalf'; while [ ! -f {finish} ]; do sleep 0.02; done; printf ' a frame\033[?2026l'; read x",
            finish = finish.display()
        ),
    );
    let mut config = sandbox.config(&session("sync"), &script);
    config.timing.update_hold = Duration::from_secs(8);
    let host = sandbox.start(config);
    let mut before = open(&host, watch());
    std::fs::write(&go, "").expect("go");
    assert!(before.wait_screen(WAIT, |text| text.contains("half")));
    let finisher = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(finish, "").expect("finish");
    });
    let started = Instant::now();
    let mut client = open(&host, watch());
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "the client wasn't held"
    );
    assert!(client.wait_screen(WAIT, |text| text.contains("half a frame")));
    finisher.join().expect("joins");
}

#[test]
fn a_client_held_for_a_synchronized_update_that_never_ends_is_let_in_after_the_hold() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(&sandbox.state).expect("state dir");
    let go = sandbox.state.join("go");
    let script = wait_for_flag_script(&go, r"printf '\033[?2026hhalf'; sleep 30");
    let mut config = sandbox.config(&session("stuck"), &script);
    config.timing.update_hold = Duration::from_millis(300);
    let host = sandbox.start(config);
    let mut before = open(&host, watch());
    std::fs::write(&go, "").expect("go");
    assert!(before.wait_screen(WAIT, |text| text.contains("half")));
    let started = Instant::now();
    let mut client = open(&host, watch());
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(250), "{waited:?}");
    assert!(waited < Duration::from_secs(5), "{waited:?}");
    assert!(client.wait_screen(WAIT, |text| text.contains("half")));
}

#[test]
fn a_size_over_the_cap_is_refused_at_every_entry_point_and_the_host_stays_small_and_up() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("huge", SLEEPS);
    let huge = Role::Window {
        take: Take::Over,
        size: size(20000, 20000),
        colours: None,
    };
    let hello = agent_host::Hello {
        version: agent_host::PROTOCOL_VERSION,
        role: huge,
    };
    assert!(matches!(
        Conn::open_with(&host.socket, &hello),
        Err(agent_session::testing::OpenError::Refused(
            HelloRefusal::Malformed
        ))
    ));
    let mut owner = open(&host, window(Take::Over, 24, 80));
    owner.send(&Command::Resize(size(20000, 20000)));
    assert!(owner.wait_closed(WAIT));
    let mut watcher = open(&host, watch());
    watcher.send(&Command::Claim(size(1001, 80)));
    assert!(watcher.wait_closed(WAIT));
    let started = Instant::now();
    let mut biggest = open(
        &host,
        window(
            Take::Over,
            agent_host::MAX_DIMENSION,
            agent_host::MAX_DIMENSION,
        ),
    );
    assert_eq!(biggest.welcome.size, size(1000, 1000));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_ne!(biggest.pump(SECOND), Heard::Closed);
}

#[test]
fn a_host_never_unlinks_a_socket_it_did_not_bind_and_a_second_state_directory_cannot_share_one() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("shared", SLEEPS);
    let other = sandbox.beside();
    let twin = other.config(&session("shared"), "true");
    let refused = agent_session::serve(twin);
    assert!(
        matches!(&refused, Err(error) if error.already_running()),
        "{refused:?}"
    );
    assert!(
        Conn::open(&host.socket, Role::Control).is_ok(),
        "the first host is untouched"
    );
    assert!(
        !other.files.record(&session("shared")).exists(),
        "the refused start left a record"
    );

    let planted = host.socket.as_path().with_extension("planted");
    let listener = std::os::unix::net::UnixListener::bind(&planted).expect("binds");
    std::fs::rename(&planted, host.socket.as_path()).expect("replaces the socket");
    std::thread::sleep(Duration::from_millis(400));
    let log = std::fs::read_to_string(sandbox.files.log(&session("shared"))).expect("log");
    assert!(log.contains("replaced the socket"), "{log}");
    listener.set_nonblocking(true).expect("nonblocking");
    let accepting = std::thread::spawn(move || {
        eventually(Duration::from_secs(5), || listener.accept().is_ok())
    });
    let _ = UnixStream::connect(host.socket.as_path());
    assert!(
        accepting.join().expect("joins"),
        "the planted socket was unlinked or rebound"
    );
}

#[test]
fn readiness_comes_from_the_host_and_a_failed_start_leaves_nothing_behind() {
    use std::io::Read;
    let sandbox = Sandbox::new();
    let (mut told, pipe) = std::io::pipe().expect("a pipe");
    let mut config = sandbox.config(&session("readies"), SLEEPS);
    config.ready = Some(std::fs::File::from(std::os::fd::OwnedFd::from(pipe)));
    let socket = sandbox.socket(&session("readies"));
    let thread = std::thread::spawn(move || agent_session::serve(config));
    let mut said = [0u8; 1];
    told.read_exact(&mut said)
        .expect("the host says it is ready");
    assert_eq!(&said, b"R");
    assert!(
        UnixStream::connect(socket.as_path()).is_ok(),
        "ready before the socket was up"
    );
    let mut control = Conn::open(&socket, Role::Control).expect("control");
    control.send(&Command::Kill);
    assert!(thread.join().expect("joins").is_ok());

    let (mut told, pipe) = std::io::pipe().expect("a pipe");
    let mut config = sandbox.config(&session("broken"), "true");
    if let Launch::Command(record) = &mut config.launch {
        record.program = "/no/such/program".into();
    }
    config.ready = Some(std::fs::File::from(std::os::fd::OwnedFd::from(pipe)));
    let socket = sandbox.socket(&session("broken"));
    assert!(agent_session::serve(config).is_err());
    let mut said = String::new();
    told.read_to_string(&mut said).expect("reads");
    assert!(
        said.starts_with('E') && said.contains("/no/such/program"),
        "{said}"
    );
    assert!(!socket.as_path().exists(), "the socket it bound was left");
    assert_eq!(
        probe(&sandbox.files.record(&session("broken"))).expect("probes"),
        Liveness::Unrecorded
    );
}

#[test]
fn starting_a_session_that_has_no_record_makes_none() {
    let sandbox = Sandbox::new();
    let mut config = sandbox.config(&session("nothing"), "true");
    config.launch = agent_session::Launch::Recorded;
    assert!(matches!(
        agent_session::serve(config),
        Err(HostError::Unrecorded)
    ));
    assert_eq!(
        probe(&sandbox.files.record(&session("nothing"))).expect("probes"),
        Liveness::Unrecorded
    );
}

#[test]
fn a_large_paste_reaches_the_program_whole_because_the_sender_waits_instead_of_being_dropped() {
    let sandbox = Sandbox::new();
    let out = sandbox.state.join("pasted");
    std::fs::create_dir_all(&sandbox.state).expect("state dir");
    let host = sandbox.host(
        "paste",
        &format!("stty raw -echo; echo READY; cat > {}", out.display()),
    );
    let mut owner = open(&host, window(Take::Over, 24, 80));
    assert!(owner.wait_screen(WAIT, |text| text.contains("READY")));
    let chunk = 60 * 1024;
    let total = 48 * chunk;
    let data: Vec<u8> = (0..total)
        .map(|n| b'a' + u8::try_from(n % 26).unwrap_or(0))
        .collect();
    for part in data.chunks(chunk) {
        owner.send(&Command::Input(Payload::new(part.to_vec()).expect("fits")));
    }
    assert!(
        eventually(Duration::from_secs(60), || std::fs::metadata(&out)
            .is_ok_and(|m| m.len() >= total as u64)),
        "the program got {:?} of {total} bytes",
        std::fs::metadata(&out).map(|m| m.len())
    );
    assert_eq!(std::fs::read(&out).expect("reads"), data);
}

const BIG_SCROLLBACK: &str = "yes abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz0123456 | head -n 60000; echo scrollback-complete; read x";

fn with_big_scrollback(sandbox: &Sandbox, name: &str) -> agent_session::HostConfig {
    let mut config = sandbox.config(&session(name), BIG_SCROLLBACK);
    config.scrollback = agent_session::ScrollbackCap::bytes(
        std::num::NonZeroU64::new(64 * 1024 * 1024).expect("non-zero"),
    );
    config
}

fn wait_for_scrollback(control: &mut Conn) {
    assert!(eventually(WAIT, || control
        .snapshot_text(Scrollback::Screen)
        .contains("scrollback-complete")));
}

#[test]
fn exactly_two_snapshots_are_in_flight_at_once() {
    let sandbox = Sandbox::new();
    let mut config = with_big_scrollback(&sandbox, "snaps");
    config.timing.write_stall = Duration::from_secs(20);
    let host = sandbox.start(config);
    let mut first = open(&host, Role::Control);
    wait_for_scrollback(&mut first);
    let mut deaf: Vec<Conn> = (0..2).map(|_| open(&host, Role::Control)).collect();
    for conn in &mut deaf {
        conn.send(&Command::Snapshot {
            scrollback: Scrollback::Full,
            style: Style::Styled,
        });
    }
    std::thread::sleep(Duration::from_millis(300));
    for _ in 0..3 {
        let mut refused = open(&host, Role::Control);
        refused.send(&Command::Snapshot {
            scrollback: Scrollback::Full,
            style: Style::Styled,
        });
        assert_eq!(
            refused.next_event(WAIT),
            Some(HostFrame::Forbidden {
                command: agent_host::CommandKind::Snapshot
            }),
            "two permits are held by clients that read nothing"
        );
    }
    for conn in &mut deaf {
        assert!(
            conn.wait_event(WAIT, |frame| matches!(frame, HostFrame::SnapshotEnd))
                .is_some(),
            "a snapshot in flight finishes once it is read"
        );
    }
}

#[test]
fn clients_that_never_read_their_snapshots_are_closed_and_free_the_permits() {
    let sandbox = Sandbox::new();
    let host = sandbox.start(with_big_scrollback(&sandbox, "snapstall"));
    let mut first = open(&host, Role::Control);
    wait_for_scrollback(&mut first);
    let mut deaf: Vec<Conn> = (0..2).map(|_| open(&host, Role::Control)).collect();
    for conn in &mut deaf {
        conn.send(&Command::Snapshot {
            scrollback: Scrollback::Full,
            style: Style::Styled,
        });
    }
    std::thread::sleep(Duration::from_millis(300));
    let mut third = open(&host, Role::Control);
    third.send(&Command::Snapshot {
        scrollback: Scrollback::Full,
        style: Style::Styled,
    });
    assert_eq!(
        third.next_event(WAIT),
        Some(HostFrame::Forbidden {
            command: agent_host::CommandKind::Snapshot
        }),
        "both permits are held while the clients don't read"
    );
    let freed = eventually(WAIT, || {
        third.send(&Command::Snapshot {
            scrollback: Scrollback::Full,
            style: Style::Styled,
        });
        matches!(
            third.wait_event(Duration::from_secs(1), |frame| matches!(
                frame,
                HostFrame::SnapshotEnd | HostFrame::Forbidden { .. }
            )),
            Some(HostFrame::SnapshotEnd)
        )
    });
    assert!(
        freed,
        "the permits of clients that read nothing were never given back"
    );
    let closed = deaf
        .iter_mut()
        .filter_map(|conn| conn.wait_closed(Duration::from_secs(3)).then_some(()))
        .count();
    assert!(closed >= 1, "no client that read nothing was closed");
}

#[test]
fn a_control_client_is_not_held_for_a_synchronized_update_and_a_command_sent_with_its_hello_is_handled()
 {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(&sandbox.state).expect("state dir");
    let go = sandbox.state.join("go");
    let script = format!(
        r"stty raw -echo; while [ ! -f {go} ]; do sleep 0.02; done; printf '\033[?2026hhalf'; cat",
        go = go.display()
    );
    let mut config = sandbox.config(&session("pipelined"), &script);
    config.timing.update_hold = Duration::from_secs(5);
    let host = sandbox.start(config);
    let mut before = open(&host, watch());
    std::fs::write(&go, "").expect("go");
    assert!(before.wait_screen(WAIT, |text| text.contains("half")));

    let started = Instant::now();
    let _control = open(&host, Role::Control);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a control client was held"
    );

    let mut stream = UnixStream::connect(host.socket.as_path()).expect("connects");
    stream.set_read_timeout(Some(WAIT)).expect("timeout");
    let mut bytes = agent_host::Frame::encode(&agent_host::Hello {
        version: agent_host::PROTOCOL_VERSION,
        role: watch(),
    });
    bytes.extend(agent_host::Frame::encode(&Command::Inject(
        Payload::new(b"pipelined text".to_vec()).expect("fits"),
    )));
    std::io::Write::write_all(&mut stream, &bytes).expect("writes");
    let greeting = agent_host::read_frame::<Greeting>(&mut stream).expect("a frame");
    assert!(matches!(greeting, Some(Greeting::Welcome(_))));
    let mut mirror = vt100::Parser::new(24, 80, 0);
    let seen = eventually(WAIT, || {
        match agent_host::read_frame::<HostFrame>(&mut stream) {
            Ok(Some(HostFrame::Output(bytes))) => mirror.process(bytes.as_slice()),
            Ok(_) | Err(_) => return false,
        }
        mirror.screen().contents().contains("pipelined text")
    });
    assert!(seen, "{}", mirror.screen().contents());
}

#[test]
fn clients_that_send_input_to_a_program_that_reads_nothing_and_leave_cannot_lock_the_host() {
    let sandbox = Sandbox::new();
    let mut config = sandbox.config(&session("clog"), "stty raw -echo; echo READY; sleep 600");
    config.max_connections = Some(3);
    let host = sandbox.start(config);
    let mut ready = open(&host, watch());
    assert!(ready.wait_screen(WAIT, |text| text.contains("READY")));
    drop(ready);
    let megabyte = Payload::new(vec![b'z'; 1000 * 1000]).expect("fits");
    for _ in 0..4 {
        let mut leaver = open(&host, watch());
        leaver.send(&Command::Inject(megabyte.clone()));
    }
    assert!(
        Conn::open(&host.socket, watch()).is_err(),
        "the host is full of clients waiting on a program that reads nothing"
    );
    let mut control = Conn::open(&host.socket, Role::Control)
        .expect("a control client can always reach a host that is full");
    assert!(
        eventually(WAIT, || Conn::open(&host.socket, watch()).is_ok()),
        "the slots of clients that left were never given back"
    );
    control.send(&Command::Kill);
    assert!(host.finish().is_ok());
}

#[test]
fn the_sender_of_a_paste_is_not_read_ahead_of_a_program_that_takes_none_of_it() {
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let sandbox = Sandbox::new();
    let mut config = sandbox.config(&session("backlog"), "stty raw -echo; echo READY; sleep 600");
    config.timing.input_wait = Duration::from_secs(60);
    let host = sandbox.start(config);
    let mut owner = open(&host, window(Take::Over, 24, 80));
    assert!(owner.wait_screen(WAIT, |text| text.contains("READY")));
    let mut stream = UnixStream::connect(host.socket.as_path()).expect("connects");
    let hello = agent_host::Frame::encode(&agent_host::Hello {
        version: agent_host::PROTOCOL_VERSION,
        role: watch(),
    });
    stream.write_all(&hello).expect("hello");
    let frame = agent_host::Frame::encode(&Command::Inject(
        Payload::new(vec![b'q'; 60 * 1024]).expect("fits"),
    ));
    let accepted = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&accepted);
    let mut writing = stream.try_clone().expect("clones");
    let sender = std::thread::spawn(move || {
        for _ in 0..200 {
            if writing.write_all(&frame).is_err() {
                return;
            }
            counted.fetch_add(frame.len(), Ordering::AcqRel);
        }
    });
    std::thread::sleep(Duration::from_millis(1500));
    let taken = accepted.load(Ordering::Acquire);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    sender.join().expect("joins");
    assert!(
        taken < 4 * 1024 * 1024,
        "the host read {taken} bytes of input for a program that takes none"
    );
}

#[test]
fn probing_a_socket_by_connecting_and_leaving_does_not_fill_the_log() {
    let sandbox = Sandbox::new();
    let host = sandbox.host("probed", SLEEPS);
    for _ in 0..30 {
        drop(UnixStream::connect(host.socket.as_path()).expect("connects"));
    }
    assert!(Conn::open(&host.socket, Role::Control).is_ok());
    let log = std::fs::read_to_string(sandbox.files.log(&session("probed"))).expect("log");
    assert!(!log.contains("could not be identified"), "{log}");
}

#[test]
fn two_hosts_starting_together_on_a_dead_socket_cannot_both_win() {
    for round in 0..6 {
        let first = Sandbox::new();
        let second = first.beside();
        let name = session(&format!("race{round}"));
        let stale = first.socket(&name);
        drop(std::os::unix::net::UnixListener::bind(stale.as_path()).expect("binds"));
        assert!(stale.as_path().exists());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let start = |sandbox: &Sandbox, barrier: std::sync::Arc<std::sync::Barrier>| {
            let config = sandbox.config(&name, "sleep 600");
            std::thread::spawn(move || {
                barrier.wait();
                agent_session::serve(config)
            })
        };
        let one = start(&first, std::sync::Arc::clone(&barrier));
        let two = start(&second, barrier);
        let mut control = None;
        assert!(eventually(WAIT, || {
            control = Conn::open(&stale, Role::Control).ok();
            control.is_some()
        }));
        if let Some(control) = &mut control {
            control.send(&Command::Kill);
        }
        let results = [one.join().expect("joins"), two.join().expect("joins")];
        let started = results.iter().filter(|result| result.is_ok()).count();
        let refused = results
            .iter()
            .filter(|result| matches!(result, Err(error) if error.already_running()))
            .count();
        assert_eq!((started, refused), (1, 1), "round {round}: {results:?}");
    }
}

#[test]
fn a_screen_whose_replay_is_larger_than_a_queue_still_replays_to_a_new_client() {
    let sandbox = Sandbox::new();
    let gate = sandbox.gate("done");
    let script = format!(
        r#"awk 'BEGIN {{ for (r = 0; r < 700; r++) {{ s = ""; for (c = 0; c < 700; c++) s = s sprintf("\033[38;5;%dm.", (r * 7 + c) % 256); print s }} }}'; echo FINISHED; {}"#,
        gate.wait()
    );
    let mut config = sandbox.config(&session("big"), &script);
    config.size = size(700, 700);
    let host = sandbox.start(config);
    let mut control = open(&host, Role::Control);
    assert!(eventually(Duration::from_secs(60), || control
        .snapshot_text(Scrollback::Screen)
        .contains("FINISHED")));
    let mut first = open(&host, watch());
    let mut second = open(&host, watch());
    for client in [&mut first, &mut second] {
        assert!(client.wait_screen(Duration::from_secs(60), |text| text.contains("FINISHED")));
    }
    assert!(
        first.output_bytes > 4 * 1024 * 1024,
        "the replay was only {} bytes, so it didn't test the queue's cap",
        first.output_bytes
    );
    assert_eq!(
        screen_differences(first.mirror.screen(), second.mirror.screen()),
        Vec::<String>::new()
    );
    gate.open();
}
