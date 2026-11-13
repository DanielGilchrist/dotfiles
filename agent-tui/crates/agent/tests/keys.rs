//! `agent keys` in a pty of its own, answering its terminal queries the way
//! a legacy or a kitty terminal would. Nothing here touches the user's
//! terminal, config or state.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const PRIMARY_ATTRIBUTES: &[u8] = b"\x1b[?62;c";
const KITTY_FLAGS: &[u8] = b"\x1b[?0u";
const DEADLINE: Duration = Duration::from_secs(10);

struct Probe {
    _scratch: tempfile::TempDir,
    _master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    heard: Arc<Mutex<Vec<u8>>>,
}

impl Probe {
    fn start(kitty: bool) -> Self {
        let scratch = tempfile::tempdir().expect("a temp dir");
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 30,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("a pty");
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_agent"));
        command.arg("keys");
        command.env_clear();
        command.env("TERM", "xterm-256color");
        command.env("PATH", std::env::var_os("PATH").unwrap_or_default());
        command.env("HOME", scratch.path());
        command.env("XDG_CONFIG_HOME", scratch.path().join("config"));
        command.env("XDG_STATE_HOME", scratch.path().join("state"));
        command.env("AGENT_WORKTREES", scratch.path().join("worktrees"));
        let child = pair.slave.spawn_command(command).expect("spawns");
        drop(pair.slave);
        let heard = Arc::new(Mutex::new(Vec::new()));
        let mut reader = pair.master.try_clone_reader().expect("a reader");
        let sink = Arc::clone(&heard);
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(read @ 1..) = reader.read(&mut buffer) {
                sink.lock()
                    .expect("lock")
                    .extend_from_slice(&buffer[..read]);
            }
        });
        let mut probe = Self {
            writer: pair.master.take_writer().expect("a writer"),
            _master: pair.master,
            _scratch: scratch,
            child,
            heard,
        };
        probe.wait_for("\x1b[?u");
        if kitty {
            probe.send(KITTY_FLAGS);
        }
        probe.send(PRIMARY_ATTRIBUTES);
        probe
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("writes");
        self.writer.flush().expect("flushes");
    }

    fn wait_for(&self, needle: &str) {
        let started = Instant::now();
        while started.elapsed() < DEADLINE {
            let text = String::from_utf8_lossy(&self.heard.lock().expect("lock")).into_owned();
            if text.contains(needle) {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let text = String::from_utf8_lossy(&self.heard.lock().expect("lock")).into_owned();
        panic!("never heard {needle:?} in {text:?}");
    }

    fn wait_for_exit(&mut self) {
        let started = Instant::now();
        while started.elapsed() < DEADLINE {
            if let Ok(Some(status)) = self.child.try_wait() {
                assert!(status.success(), "{status:?}");
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("agent keys never quit");
    }
}

#[test]
fn a_legacy_terminal_is_told_so_and_a_session_of_keys_prints_each_verdict_and_quits_on_escape_twice()
 {
    let mut probe = Probe::start(false);
    probe.wait_for("legacy key reporting");

    probe.send(b"2");
    probe.wait_for("press the key for prev_agent");
    probe.send(b"\x1b,");
    probe.wait_for("bytes 1b 2c");
    probe.wait_for("prev_agent = \"alt-,\"");

    probe.send(b"1");
    probe.wait_for("press the key for nav");
    probe.send(&[0x1c]);
    probe.wait_for("nav = 'ctrl-\\'");

    probe.send(b"1");
    probe.wait_for("press the key for nav");
    probe.send(&[0x12]);
    probe.wait_for("readline and shells: reverse history search");

    probe.send(b"1");
    probe.wait_for("press the key for nav");
    probe.send(&[0x09]);
    probe.wait_for("the same bytes as Tab");

    probe.send(b"\x1b");
    probe.wait_for("Esc again to quit");
    probe.send(b"\x1b");
    probe.wait_for_exit();
}

#[test]
fn a_kitty_terminal_is_told_so_and_a_kitty_only_key_is_usable_on_it() {
    let mut probe = Probe::start(true);
    probe.wait_for("this terminal reports the kitty keyboard protocol");
    probe.wait_for("\x1b[>");

    probe.send(b"1");
    probe.wait_for("press the key for nav");
    probe.send(b"\x1b[105;5u");
    probe.wait_for("only the kitty keyboard protocol sends it");
    probe.wait_for("nav = \"ctrl-i\"");

    probe.send(b"1");
    probe.wait_for("press the key for nav");
    probe.send(b"\x1b[P");
    probe.wait_for("event F(1)");
    probe.wait_for("nav = \"f1\"");

    probe.send(b"\x1b[99;5u");
    probe.wait_for("Ctrl-C again to quit");
    probe.send(b"\x1b[99;5u");
    probe.wait_for_exit();
    probe.wait_for("\x1b[<1u");
}
