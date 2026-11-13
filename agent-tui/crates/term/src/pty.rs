use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::thread;

use agent_core::process::LaunchSpec;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::escapes::{Sequence, consume};
use crate::{Emulator, Modes, Screen, TermSize};

/// Output chunks buffered before the reader waits for the UI to catch up.
const OUTPUT_BACKLOG: usize = 64;
/// How much of the end of a program's output is kept to say why it ended.
const TAIL_KEPT: usize = 512;
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

/// The program's pty no longer takes input, so what was sent is lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

impl std::fmt::Display for Closed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("its terminal no longer takes input")
    }
}

/// A program running in a pty, rendered through an [`Emulator`]. Reading and
/// writing happen on background threads so the owner never blocks on the
/// program. Output is fed to the emulator by [`Terminal::pump`].
pub struct Terminal<E> {
    emulator: E,
    master: Box<dyn MasterPty + Send>,
    child: Option<Box<dyn Child + Send + Sync>>,
    pid: u32,
    tail: Vec<u8>,
    input: Sender<Vec<u8>>,
    write_failed: Arc<AtomicBool>,
    output: Receiver<Vec<u8>>,
    heard: bool,
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
        let pid = child
            .process_id()
            .ok_or_else(|| io::Error::other("the program has no process id"))?;

        let (output_tx, output) = mpsc::sync_channel(OUTPUT_BACKLOG);
        let reader = pair.master.try_clone_reader().map_err(to_io)?;
        thread::spawn(move || read_output(reader, &output_tx));

        let (input, input_rx) = mpsc::channel::<Vec<u8>>();
        let mut writer = pair.master.take_writer().map_err(to_io)?;
        let write_failed = Arc::new(AtomicBool::new(false));
        let failed = Arc::clone(&write_failed);
        thread::spawn(move || {
            for bytes in input_rx {
                if writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    failed.store(true, Ordering::Release);
                    break;
                }
            }
        });

        Ok(Self {
            emulator,
            master: pair.master,
            pid,
            tail: Vec::new(),
            child: Some(child),
            input,
            write_failed,
            output,
            heard: false,
            size,
            liveness: Liveness::Running,
        })
    }

    /// Feeds pending output to the emulator. Reports whether the program is
    /// still running.
    pub fn pump(&mut self) -> Liveness {
        let mut fed = 0;
        while fed < PUMP_BUDGET {
            match self.output.try_recv() {
                Ok(bytes) => {
                    self.heard = true;
                    fed += bytes.len();
                    self.emulator.feed(&bytes);
                    if self.emulator.modes().screen == Screen::Alternate {
                        self.tail.clear();
                    } else {
                        keep_tail(&mut self.tail, &bytes);
                    }
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

    pub fn size(&self) -> TermSize {
        self.size
    }

    /// Queues `bytes` for the program. A write that failed on the writer
    /// thread is only known to the next send, which says so, so the bytes
    /// that failed themselves are lost without a word.
    pub fn send(&self, bytes: Vec<u8>) -> Result<(), Closed> {
        if bytes.is_empty() {
            return Ok(());
        }
        if self.write_failed.load(Ordering::Acquire) {
            return Err(Closed);
        }
        self.input.send(bytes).map_err(|_| Closed)
    }

    /// Whether the program has printed anything yet, so the emulator's modes
    /// are the ones it set and not the defaults.
    pub fn heard(&self) -> bool {
        self.heard
    }

    pub fn modes(&self) -> Modes {
        self.emulator.modes()
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Why the program ended: the last thing it printed, or else its exit
    /// status when that wasn't success.
    pub fn exit_reason(&mut self) -> Option<String> {
        if let Some(said) = last_line(&self.tail) {
            return Some(said);
        }
        let status = self.child.as_mut()?.try_wait().ok().flatten()?;
        (!status.success()).then(|| format!("exited with status {}", status.exit_code()))
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, show_cursor: bool) {
        self.emulator.render(area, buf, show_cursor);
    }
}

/// Keeps the end of `tail`. A cut lands mid-sequence or mid-character, so
/// what follows it up to the next line break or escape is dropped too.
fn keep_tail(tail: &mut Vec<u8>, bytes: &[u8]) {
    tail.extend_from_slice(bytes);
    let excess = tail.len().saturating_sub(TAIL_KEPT);
    if excess == 0 {
        return;
    }
    let resume = tail[excess..]
        .iter()
        .position(|b| matches!(b, b'\n' | b'\r' | 0x1b))
        .map_or(tail.len(), |at| excess + at);
    tail.drain(..resume);
}

/// The last non-blank line of terminal output as a person would read it:
/// escape sequences are left out, and only a carriage return, a newline or a
/// cursor-moving sequence ends a line.
fn last_line(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = vec![String::new()];
    let mut chars = text.chars();
    let break_line = |lines: &mut Vec<String>| lines.push(String::new());
    while let Some(c) = chars.next() {
        match (c, consume(c, &mut chars)) {
            (_, Some(Sequence::Moves)) | ('\r' | '\n', None) => break_line(&mut lines),
            (_, Some(Sequence::Other)) => {}
            (c, None) if c.is_control() => {}
            (c, None) => lines.last_mut().expect("a line").push(c),
        }
    }
    lines
        .iter()
        .map(|line| line.trim())
        .rfind(|line| !line.is_empty())
        .map(String::from)
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
    /// Killing is immediate. Reaping happens off-thread so dropping a
    /// terminal never blocks.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            thread::spawn(move || child.wait());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TAIL_KEPT, keep_tail, last_line};

    #[test]
    fn the_last_words_of_a_program_are_its_last_non_blank_line_without_escapes() {
        let out = b"\x1b[?1049l\x1b[m\x1b[?25h\x1b[20;1HLost connection to the Zellij server\r\n";
        assert_eq!(
            last_line(out).as_deref(),
            Some("Lost connection to the Zellij server")
        );
    }

    #[test]
    fn output_of_only_escapes_says_nothing() {
        assert_eq!(last_line(b"\x1b[2J\x1b]11;?\x1b\\\x1bP1\x1b\\"), None);
    }

    #[test]
    fn colour_inside_a_line_does_not_split_it() {
        assert_eq!(
            last_line(b"\x1b[31mlost\x1b[0m connection\n").as_deref(),
            Some("lost connection")
        );
    }

    #[test]
    fn a_charset_selection_leaves_no_stray_character() {
        assert_eq!(last_line(b"\x1b(Bgone\x1b(0\n").as_deref(), Some("gone"));
    }

    #[test]
    fn a_carriage_return_starts_a_new_line() {
        assert_eq!(last_line(b"Loading\rDone\r\n").as_deref(), Some("Done"));
    }

    #[test]
    fn other_control_characters_are_dropped() {
        assert_eq!(last_line(b"a\x08b\x07c").as_deref(), Some("abc"));
    }

    #[test]
    fn a_tail_cut_mid_sequence_resumes_at_the_next_line_break() {
        let mut tail = Vec::new();
        keep_tail(&mut tail, &vec![b'x'; TAIL_KEPT]);
        keep_tail(&mut tail, b"\x1b[3");
        keep_tail(&mut tail, b"1mred\r\nlast words");
        assert_eq!(last_line(&tail).as_deref(), Some("last words"));
        assert!(tail.len() <= TAIL_KEPT);
    }

    #[test]
    fn a_tail_with_no_boundary_after_the_cut_is_dropped() {
        let mut tail = Vec::new();
        keep_tail(&mut tail, &vec![b'x'; TAIL_KEPT + 10]);
        assert_eq!(tail, Vec::<u8>::new());
    }
}
