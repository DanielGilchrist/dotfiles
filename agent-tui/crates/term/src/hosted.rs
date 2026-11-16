use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use agent_core::activity::Pid;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::TermSize;

/// A command to run in a pty with exactly this environment, inherited
/// variables included, so the caller decides every variable the program sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
}

pub(crate) fn pty_size(size: TermSize) -> PtySize {
    PtySize {
        rows: size.rows.get(),
        cols: size.cols.get(),
        pixel_width: 0,
        pixel_height: 0,
    }
}

pub(crate) fn to_io(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

pub struct HostedPty(Box<dyn MasterPty + Send>);

impl HostedPty {
    pub fn resize(&self, size: TermSize) -> io::Result<()> {
        self.0.resize(pty_size(size)).map_err(to_io)
    }
}

/// How the program ended, as `portable-pty` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedExit {
    pub code: u32,
    pub signal: Option<String>,
}

pub struct HostedChild {
    child: Box<dyn Child + Send + Sync>,
    pid: Pid,
}

impl HostedChild {
    /// The program is a session leader, so this is also its process group.
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Blocks until the program ends, reaping it.
    pub fn wait(mut self) -> io::Result<HostedExit> {
        let status = self.child.wait()?;
        Ok(HostedExit {
            code: status.exit_code(),
            signal: status.signal().map(str::to_owned),
        })
    }
}

/// A program running in a pty with nothing rendered: the owner reads its
/// output, writes its input and sizes it.
pub struct Hosted {
    pub pty: HostedPty,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub child: HostedChild,
}

pub fn spawn_hosted(command: &HostedCommand, size: TermSize) -> io::Result<Hosted> {
    let pair = native_pty_system().openpty(pty_size(size)).map_err(to_io)?;
    let mut builder = CommandBuilder::new(&command.program);
    builder.args(&command.args);
    builder.cwd(&command.cwd);
    builder.env_clear();
    for (key, value) in &command.environment {
        builder.env(key, value);
    }
    let child = pair.slave.spawn_command(builder).map_err(to_io)?;
    drop(pair.slave);
    let pid = child
        .process_id()
        .and_then(Pid::new)
        .ok_or_else(|| io::Error::other("the program has no process id"))?;
    let reader = pair.master.try_clone_reader().map_err(to_io)?;
    let writer = pair.master.take_writer().map_err(to_io)?;
    Ok(Hosted {
        pty: HostedPty(pair.master),
        reader,
        writer,
        child: HostedChild { child, pid },
    })
}
