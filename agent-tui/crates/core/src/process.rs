use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

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
pub fn quiet(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.env("LC_ALL", "C").stdin(Stdio::null());
    cmd
}

/// Reads a pipe to the end on its own thread. A grandchild that inherits the
/// pipe can keep it open past the child's exit, so the result is collected
/// with a deadline rather than joined.
fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

/// The pipe's contents, or `None` if something still holds it open past the
/// deadline; partial output must never pass for complete output.
fn collect(pipe: &mpsc::Receiver<Vec<u8>>, deadline: Instant) -> Option<Vec<u8>> {
    pipe.recv_timeout(
        deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(100)),
    )
    .ok()
}

/// Runs `cmd` to completion within `timeout`, killing it if it overruns.
/// Non-zero exits are returned, not errors.
pub fn output_within(cmd: &mut Command, timeout: Duration) -> Result<Output, CommandError> {
    let command = describe(cmd);
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| CommandError::Spawn {
            command: command.clone(),
            source,
        })?;
    let stdout = drain(child.stdout.take().expect("stdout is piped"));
    let stderr = drain(child.stderr.take().expect("stderr is piped"));
    let deadline = Instant::now() + timeout;
    let status: ExitStatus = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CommandError::TimedOut {
                    command,
                    after: timeout,
                });
            }
            Err(source) => return Err(CommandError::Spawn { command, source }),
        }
    };
    match (collect(&stdout, deadline), collect(&stderr, deadline)) {
        (Some(stdout), Some(stderr)) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        _ => Err(CommandError::TimedOut {
            command,
            after: timeout,
        }),
    }
}

pub fn output(cmd: &mut Command) -> Result<Output, CommandError> {
    output_within(cmd, DEFAULT_TIMEOUT)
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
}

impl LaunchSpec {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
        }
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
        let out =
            output(Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"])).unwrap();
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
}
