use std::fs::OpenOptions;
use std::io;

use nix::errno::Errno;
use nix::unistd::{dup2_stderr, dup2_stdin, dup2_stdout, setsid};

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("starting a session of its own: {0}")]
    Session(Errno),
    #[error("opening /dev/null: {0}")]
    Null(io::Error),
    #[error("pointing the standard streams at /dev/null: {0}")]
    Streams(Errno),
}

/// Makes the process fit to outlive its terminal: a new session, so it has
/// no controlling terminal and gets no `SIGHUP` when one closes, and the
/// standard streams on `/dev/null`. It fails when the process leads its
/// process group, as a command started in a shell's foreground does, so a
/// host is started through [`spawn_host`](crate::spawn_host).
pub fn detach_from_terminal() -> Result<(), DaemonError> {
    setsid().map_err(DaemonError::Session)?;
    let null = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .map_err(DaemonError::Null)?;
    dup2_stdin(&null).map_err(DaemonError::Streams)?;
    dup2_stdout(&null).map_err(DaemonError::Streams)?;
    dup2_stderr(&null).map_err(DaemonError::Streams)?;
    Ok(())
}
