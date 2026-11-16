use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use agent_core::names::SessionName;

use crate::paths::HostFiles;
use crate::record::{Liveness, probe};

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("a host already runs {0}")]
    AlreadyRunning(SessionName),
    #[error("starting the host: {0}")]
    Start(std::io::Error),
    #[error("the host couldn't start: {0}")]
    Failed(String),
    #[error("the host ended before it was ready")]
    Ended,
    #[error("the host wasn't ready after {0:?}")]
    NotReady(Duration),
}

/// Starts `exe` with `host_args`, which runs a host for `name` (the hidden
/// `agent session host`), and returns once the host says it is serving. The
/// host tells it on its own standard output, which is a pipe made here, so
/// readiness is the host's word and not a file it may have left behind.
///
/// The host is a child of this process until this process exits, and then of
/// init, so it is never left as a zombie of a long-lived process: run this in
/// a short lived process, `agent session host --spawn`, and wait for that one.
pub fn spawn_host(
    exe: &Path,
    host_args: &[OsString],
    name: &SessionName,
    files: &HostFiles,
    within: Duration,
) -> Result<(), SpawnError> {
    if probe(&files.record(name)).is_ok_and(|liveness| liveness == Liveness::Live) {
        return Err(SpawnError::AlreadyRunning(name.clone()));
    }
    let mut host = Command::new(exe)
        .args(host_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(SpawnError::Start)?;
    let Some(mut said) = host.stdout.take() else {
        return Err(SpawnError::Ended);
    };
    let (told, heard) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        let _ = said.read_to_string(&mut text);
        let _ = told.send(text);
    });
    match heard.recv_timeout(within) {
        Ok(text) => match text.strip_prefix('R') {
            Some(_) => Ok(()),
            None => match text.strip_prefix('E') {
                Some(reason) => Err(SpawnError::Failed(reason.to_owned())),
                None => Err(SpawnError::Ended),
            },
        },
        Err(_) => Err(SpawnError::NotReady(within)),
    }
}
