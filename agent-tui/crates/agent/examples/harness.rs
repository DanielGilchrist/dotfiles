use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn main() -> anyhow::Result<()> {
    let bin = std::env::args()
        .nth(1)
        .expect("usage: harness <bin> [steps...]");
    let steps: Vec<String> = std::env::args().skip(2).collect();
    let rows = std::env::var("ROWS").map_or(45, |v| v.parse().unwrap());
    let cols = std::env::var("COLS").map_or(160, |v| v.parse().unwrap());

    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(bin);
    cmd.cwd(std::env::current_dir()?);
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
    let raw = Arc::new(Mutex::new(Vec::new()));
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    {
        let parser = parser.clone();
        let raw = raw.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                parser.lock().unwrap().process(&buf[..n]);
                raw.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
    }

    for step in steps {
        match step.split_once(':') {
            Some(("wait", ms)) => thread::sleep(Duration::from_millis(ms.parse()?)),
            Some(("keys", keys)) => {
                writer.write_all(&unescape(keys))?;
                writer.flush()?;
            }
            Some(("dump", label)) => {
                println!("===== {label} =====");
                println!("{}", parser.lock().unwrap().screen().contents());
            }
            Some(("raw", path)) => std::fs::write(path, &*raw.lock().unwrap())?,
            _ => panic!("bad step {step}"),
        }
    }
    child.kill()?;
    Ok(())
}

/// `\t`, `\r` and `\xNN` escapes, as typed in a step.
fn unescape(keys: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = keys.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        rest = match (b, tail) {
            (b'\\', [b't', tail @ ..]) => {
                out.push(b'\t');
                tail
            }
            (b'\\', [b'r', tail @ ..]) => {
                out.push(b'\r');
                tail
            }
            (b'\\', [b'x', hi, lo, tail @ ..]) => {
                let hex = std::str::from_utf8(&[*hi, *lo])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                out.push(hex.expect("bad \\x escape"));
                tail
            }
            _ => {
                out.push(b);
                tail
            }
        };
    }
    out
}
