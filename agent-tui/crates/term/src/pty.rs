use std::io::{self, Read, Write};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::thread;

use agent_core::process::LaunchSpec;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::{Emulator, Modes, TermSize};

/// Output chunks buffered before the reader waits for the UI to catch up.
const OUTPUT_BACKLOG: usize = 64;
/// Output parsed per frame, so a flood can't keep the UI from drawing.
const PUMP_BUDGET: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Running,
    Exited,
}

fn pty_size(size: TermSize) -> PtySize {
    PtySize {
        rows: size.rows.get(),
        cols: size.cols.get(),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn to_io(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// A program running in a pty, rendered through an [`Emulator`]. Reading and
/// writing happen on background threads so the owner never blocks on the
/// program; output is fed to the emulator by [`Terminal::pump`].
pub struct Terminal<E> {
    emulator: E,
    master: Box<dyn MasterPty + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    pid: Option<u32>,
    input: Sender<Vec<u8>>,
    output: Receiver<Vec<u8>>,
    size: TermSize,
    liveness: Liveness,
}

impl<E: Emulator> Terminal<E> {
    pub fn spawn(spec: &LaunchSpec, size: TermSize, emulator: E) -> io::Result<Self> {
        let pair = native_pty_system().openpty(pty_size(size)).map_err(to_io)?;

        let mut cmd = CommandBuilder::new(spec.program());
        cmd.args(spec.arguments());
        if let Some(cwd) = spec.working_dir() {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for key in spec.removed_environment() {
            cmd.env_remove(key);
        }
        for (key, value) in spec.environment() {
            cmd.env(key, value);
        }
        let child = pair.slave.spawn_command(cmd).map_err(to_io)?;
        drop(pair.slave);

        let (output_tx, output) = mpsc::sync_channel(OUTPUT_BACKLOG);
        let reader = pair.master.try_clone_reader().map_err(to_io)?;
        thread::spawn(move || read_output(reader, &output_tx));

        let (input, input_rx) = mpsc::channel::<Vec<u8>>();
        let mut writer = pair.master.take_writer().map_err(to_io)?;
        thread::spawn(move || {
            for bytes in input_rx {
                if writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });

        Ok(Self {
            emulator,
            master: pair.master,
            pid: child.process_id(),
            child: Some(child),
            input,
            output,
            size,
            liveness: Liveness::Running,
        })
    }

    /// Feeds pending output to the emulator; reports whether the program is
    /// still running.
    pub fn pump(&mut self) -> Liveness {
        let mut fed = 0;
        while fed < PUMP_BUDGET {
            match self.output.try_recv() {
                Ok(bytes) => {
                    fed += bytes.len();
                    self.emulator.feed(&bytes);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.liveness = Liveness::Exited;
                    break;
                }
            }
        }
        self.liveness
    }

    pub fn resize(&mut self, size: TermSize) -> io::Result<()> {
        if self.size == size {
            return Ok(());
        }
        self.master.resize(pty_size(size)).map_err(to_io)?;
        self.size = size;
        self.emulator.resize(size);
        Ok(())
    }

    pub fn send(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.input.send(bytes);
        }
    }

    pub fn modes(&self) -> Modes {
        self.emulator.modes()
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, show_cursor: bool) {
        self.emulator.render(area, buf, show_cursor);
    }
}

fn read_output(mut reader: Box<dyn Read + Send>, output: &SyncSender<Vec<u8>>) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                if output.send(buf[..n].to_vec()).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

impl<E> Drop for Terminal<E> {
    /// Killing is immediate; reaping happens off-thread so dropping a
    /// terminal never blocks.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            thread::spawn(move || child.wait());
        }
    }
}
