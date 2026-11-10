use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::activity::{Join, Pid};

/// Long enough for a slow `git fetch`-free query or `ls-remote` over ssh.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_THREADS: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("couldn't run {command}: {source}")]
    Spawn {
        command: String,
        #[source]
        source: io::Error,
    },
    #[error("{command} failed ({status}): {stderr}")]
    Failed {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("{command} timed out after {}s", .after.as_secs())]
    TimedOut { command: String, after: Duration },
    #[error("unexpected output from {command}: {detail}")]
    Parse { command: String, detail: String },
}

pub fn describe(cmd: &Command) -> String {
    std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A command whose output is parsed: C locale so messages are stable, and
/// no stdin, so nothing can prompt.
pub fn quiet(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("LC_ALL", "C").stdin(Stdio::null());
    cmd
}

/// What a pipe held: at most the cap's worth of bytes, and whether the
/// writer had more.
struct Drained {
    bytes: Vec<u8>,
    more: bool,
}

/// Reads up to `cap` bytes of a pipe on its own thread. Anything past the
/// cap is read and thrown away, so a chatty child never blocks on a full
/// pipe. A grandchild that inherits the pipe can keep it open past the
/// child's exit, so the result is collected with a deadline rather than
/// joined.
fn drain(mut pipe: impl Read + Send + 'static, cap: u64) -> mpsc::Receiver<Drained> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.by_ref().take(cap).read_to_end(&mut bytes);
        let mut probe = [0u8; 1];
        let more = pipe.read(&mut probe).is_ok_and(|n| n > 0);
        let _ = tx.send(Drained { bytes, more });
        if more {
            let _ = io::copy(&mut pipe, &mut io::sink());
        }
    });
    rx
}

/// The pipe's contents, or `None` if something still holds it open past the
/// deadline: partial output must never pass for complete output.
fn collect(pipe: &mpsc::Receiver<Drained>, deadline: Instant) -> Option<Drained> {
    pipe.recv_timeout(
        deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(100)),
    )
    .ok()
}

enum Waited {
    Exited(ExitStatus),
    Stopped,
    Overran,
}

/// Waits for `child` until `deadline`, killing it if it overruns or if
/// `stop` says to.
fn wait(
    child: &mut Child,
    deadline: Instant,
    mut stop: impl FnMut() -> bool,
) -> io::Result<Waited> {
    loop {
        if stop() {
            let _ = child.kill();
            child.wait()?;
            return Ok(Waited::Stopped);
        }
        match child.try_wait()? {
            Some(status) => return Ok(Waited::Exited(status)),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            None => {
                let _ = child.kill();
                child.wait()?;
                return Ok(Waited::Overran);
            }
        }
    }
}

/// Stderr is only read for error messages.
const STDERR_BYTES: u64 = 256 * 1024;

/// What [`output_capped`] read.
#[derive(Debug)]
pub enum Capped {
    Whole(Output),
    /// The command had more than the cap and was killed, so there is no
    /// exit status to report.
    Cut {
        stdout: Vec<u8>,
    },
}

fn collect_output(
    cmd: &mut Command,
    timeout: Duration,
    stdout_cap: u64,
) -> Result<Capped, CommandError> {
    let command = describe(cmd);
    let started = Instant::now();
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| CommandError::Spawn {
            command: command.clone(),
            source,
        })?;
    let stdout = drain(child.stdout.take().expect("stdout is piped"), stdout_cap);
    let stderr = drain(child.stderr.take().expect("stderr is piped"), STDERR_BYTES);
    let deadline = started + timeout;
    let timed_out = |after| CommandError::TimedOut {
        command: command.clone(),
        after,
    };
    let mut early: Option<Drained> = None;
    let waited = wait(&mut child, deadline, || {
        if early.is_none() {
            early = stdout.try_recv().ok();
        }
        early.as_ref().is_some_and(|d| d.more)
    })
    .map_err(|source| CommandError::Spawn {
        command: command.clone(),
        source,
    })?;
    let status = match waited {
        Waited::Exited(status) => status,
        Waited::Overran => return Err(timed_out(timeout)),
        Waited::Stopped => {
            let stdout = early.expect("only a read stdout stops the wait").bytes;
            return Ok(Capped::Cut { stdout });
        }
    };
    let stdout = early.or_else(|| collect(&stdout, deadline));
    let stderr = collect(&stderr, deadline);
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        return Err(timed_out(started.elapsed()));
    };
    Ok(if stdout.more {
        Capped::Cut {
            stdout: stdout.bytes,
        }
    } else {
        Capped::Whole(Output {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        })
    })
}

/// Runs `cmd` to completion within `timeout`, killing it if it overruns.
/// Non-zero exits are returned, not errors.
pub fn output_within(cmd: &mut Command, timeout: Duration) -> Result<Output, CommandError> {
    match collect_output(cmd, timeout, u64::MAX)? {
        Capped::Whole(output) => Ok(output),
        Capped::Cut { .. } => unreachable!("a read with no cap is never cut"),
    }
}

pub fn output(cmd: &mut Command) -> Result<Output, CommandError> {
    output_within(cmd, DEFAULT_TIMEOUT)
}

/// Like [`output_within`], but reads at most `max` bytes of stdout, so a
/// huge diff is never held whole in memory.
pub fn output_capped(
    cmd: &mut Command,
    timeout: Duration,
    max: u64,
) -> Result<Capped, CommandError> {
    collect_output(cmd, timeout, max)
}

/// Runs `cmd` and returns its stdout, failing on a non-zero exit.
pub fn run(cmd: &mut Command) -> Result<String, CommandError> {
    let out = output(cmd)?;
    if !out.status.success() {
        return Err(failed(cmd, &out));
    }
    String::from_utf8(out.stdout).map_err(|e| CommandError::Parse {
        command: describe(cmd),
        detail: e.to_string(),
    })
}

pub fn failed(cmd: &Command, out: &Output) -> CommandError {
    CommandError::Failed {
        command: describe(cmd),
        status: out.status.to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    }
}

/// `f` over `items` on at most eight threads, preserving order.
pub fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if items.is_empty() {
        return Vec::new();
    }
    let chunk = items.len().div_ceil(MAX_THREADS);
    thread::scope(|s| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|c| s.spawn(|| c.iter().map(&f).collect::<Vec<_>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("worker panicked"))
            .collect()
    })
}

/// A program, its arguments and environment, ready to run in a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
    env_remove: Vec<OsString>,
    join: Join,
    notes: Vec<String>,
}

impl LaunchSpec {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            join: Pid::current().map_or(Join::Untraced, Join::Fresh),
            notes: Vec::new(),
        }
    }

    /// Its hooks come from a process other than the one this spec starts,
    /// as when it joins a session that is already running.
    #[must_use]
    pub fn untraced(mut self) -> Self {
        self.join = Join::Untraced;
        self
    }

    /// Something to record about how this was worked out. The terminal it
    /// starts in is no place to print it.
    #[must_use]
    pub fn noting(mut self, note: String) -> Self {
        self.notes.push(note);
        self
    }

    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    pub fn join(&self) -> Join {
        self.join
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    #[must_use]
    pub fn env_remove<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.env_remove.extend(keys.into_iter().map(Into::into));
        self
    }

    pub fn program(&self) -> &OsStr {
        &self.program
    }

    pub fn arguments(&self) -> &[OsString] {
        &self.args
    }

    pub fn working_dir(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn environment(&self) -> &[(OsString, OsString)] {
        &self.env
    }

    pub fn removed_environment(&self) -> &[OsString] {
        &self.env_remove
    }

    pub fn argv(&self) -> impl Iterator<Item = &OsStr> {
        std::iter::once(self.program.as_os_str()).chain(self.args.iter().map(OsString::as_os_str))
    }

    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        for key in &self.env_remove {
            cmd.env_remove(key);
        }
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrunning_commands_are_killed() {
        let mut cmd = Command::new("sleep");
        cmd.arg("5");
        let started = Instant::now();
        assert!(matches!(
            output_within(&mut cmd, Duration::from_millis(100)),
            Err(CommandError::TimedOut { .. })
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn output_is_captured_and_failures_returned() {
        let out = output(Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]))
            .expect("succeeds");
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn parallel_map_preserves_order() {
        let items: Vec<u32> = (0..50).collect();
        assert_eq!(
            parallel_map(&items, |n| n * 2),
            items.iter().map(|n| n * 2).collect::<Vec<_>>()
        );
    }

    #[test]
    fn capped_output_stops_a_command_that_has_more() {
        let mut cmd = quiet("sh");
        cmd.args(["-c", "yes | head -c 1000000"]);
        match output_capped(&mut cmd, Duration::from_secs(10), 100).expect("runs") {
            Capped::Cut { stdout } => assert_eq!(stdout.len(), 100),
            Capped::Whole(_) => panic!("output was cut"),
        }
    }

    #[test]
    fn capped_output_under_the_cap_is_whole() {
        let mut cmd = quiet("sh");
        cmd.args(["-c", "printf abc; exit 3"]);
        match output_capped(&mut cmd, Duration::from_secs(10), 100).expect("runs") {
            Capped::Whole(out) => {
                assert_eq!(out.stdout, b"abc");
                assert_eq!(out.status.code(), Some(3));
            }
            Capped::Cut { .. } => panic!("nothing was cut"),
        }
    }

    #[test]
    fn stderr_is_capped_without_blocking_the_command() {
        let mut cmd = quiet("sh");
        cmd.args(["-c", "yes err | head -c 2000000 >&2; echo done"]);
        let out = output(&mut cmd).expect("runs");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"done\n");
        assert_eq!(out.stderr.len() as u64, STDERR_BYTES);
    }

    #[test]
    fn a_timeout_reports_the_time_allowed() {
        let mut cmd = quiet("sleep");
        cmd.arg("5");
        let Err(CommandError::TimedOut { after, .. }) =
            output_capped(&mut cmd, Duration::from_millis(100), 100)
        else {
            panic!("should time out");
        };
        assert_eq!(after, Duration::from_millis(100));
    }
}
