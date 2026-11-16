use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};

/// The command a session's pane runs, kept so a session that exited can be
/// started again from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRecord {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

impl HostRecord {
    /// The record as last written, read without the lock, which a host
    /// holds for its life. `None` for a file that was never written.
    pub fn read(path: &Path) -> Result<Option<Self>, RecordError> {
        let text = std::fs::read(path)?;
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&text)?))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another host already runs this session")]
    Held,
    #[error("there is no record of this session")]
    Missing,
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("the record can't be read: {0}")]
    Io(#[from] io::Error),
    #[error("the record isn't valid: {0}")]
    Invalid(#[from] serde_json::Error),
}

/// The exclusive `flock` a host holds on its record file for its whole life.
/// The kernel drops it when the process dies, so a crashed host never looks
/// alive and two hosts can't run one session. The file is opened close on
/// exec, so the program the host runs doesn't inherit the lock.
pub(crate) struct HostLock {
    file: Flock<File>,
    created: bool,
}

/// Whether taking the lock may make the record file. Only a start that
/// brings its own command does, so a start that has nothing to run can't
/// leave a file that looks like a session that exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Presence {
    CreateIfMissing,
    MustExist,
}

/// How long taking the lock waits out a [`probe`], which holds it shared
/// for an instant. A live host holds it exclusively for its life, so a real
/// conflict is reported once this has passed.
const PROBE_GRACE: Duration = Duration::from_millis(250);

const RETRY_EVERY: Duration = Duration::from_millis(5);

impl HostLock {
    pub(crate) fn acquire(path: &Path, presence: Presence) -> Result<Self, LockError> {
        let io = |source| LockError::Io {
            path: path.to_path_buf(),
            source,
        };
        let mut options = OpenOptions::new();
        options.read(true).write(true).mode(0o600);
        let (file, created) = match presence {
            Presence::MustExist => match options.open(path) {
                Ok(file) => (file, false),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(LockError::Missing),
                Err(e) => return Err(io(e)),
            },
            Presence::CreateIfMissing => match options.clone().create_new(true).open(path) {
                Ok(file) => (file, true),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    (options.open(path).map_err(io)?, false)
                }
                Err(e) => return Err(io(e)),
            },
        };
        let deadline = Instant::now() + PROBE_GRACE;
        let mut file = file;
        loop {
            match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
                Ok(file) => return Ok(Self { file, created }),
                Err((back, Errno::EWOULDBLOCK)) if Instant::now() < deadline => {
                    file = back;
                    std::thread::sleep(RETRY_EVERY);
                }
                Err((_, errno)) => {
                    if created {
                        let _ = std::fs::remove_file(path);
                    }
                    return match errno {
                        Errno::EWOULDBLOCK => Err(LockError::Held),
                        errno => Err(io(errno.into())),
                    };
                }
            }
        }
    }

    /// Whether taking the lock made the record file.
    pub(crate) fn created(&self) -> bool {
        self.created
    }

    /// Rewrites the record in place. A new file renamed over it would be a
    /// different inode than the one that is locked.
    pub(crate) fn store(&mut self, record: &HostRecord) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
        let file: &mut File = &mut self.file;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&json)?;
        file.flush()
    }

    /// `None` for a record file that was never written.
    pub(crate) fn load(&mut self) -> Result<Option<HostRecord>, RecordError> {
        let file: &mut File = &mut self.file;
        file.seek(SeekFrom::Start(0))?;
        let mut text = Vec::new();
        file.read_to_end(&mut text)?;
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&text)?))
    }
}

/// Removes a dead session's record and log while holding the record's lock,
/// so a host that starts meanwhile is refused or leaves a record that stays.
/// A record that is already gone is not an error.
pub fn forget(record: &Path, log: &Path) -> Result<(), LockError> {
    let _held = match HostLock::acquire(record, Presence::MustExist) {
        Ok(lock) => lock,
        Err(LockError::Missing) => return remove_log(log),
        Err(e) => return Err(e),
    };
    for path in [record, log] {
        match std::fs::remove_file(path) {
            Err(source) if source.kind() != io::ErrorKind::NotFound => {
                return Err(LockError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

fn remove_log(log: &Path) -> Result<(), LockError> {
    match std::fs::remove_file(log) {
        Err(source) if source.kind() != io::ErrorKind::NotFound => Err(LockError::Io {
            path: log.to_path_buf(),
            source,
        }),
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    Live,
    Dead,
    /// No record, so no host was ever started under this name.
    Unrecorded,
}

/// Whether a host holds the record's lock, by taking it shared without
/// waiting and giving it straight back.
pub fn probe(record: &Path) -> io::Result<Liveness> {
    let file = match OpenOptions::new().read(true).open(record) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Liveness::Unrecorded),
        Err(e) => return Err(e),
    };
    match Flock::lock(file, FlockArg::LockSharedNonblock) {
        Ok(_released_on_drop) => Ok(Liveness::Dead),
        Err((_, Errno::EWOULDBLOCK)) => Ok(Liveness::Live),
        Err((_, errno)) => Err(errno.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> HostRecord {
        HostRecord {
            program: "sh".into(),
            args: vec!["-c".into(), "true".into()],
            cwd: "/tmp".into(),
        }
    }

    #[test]
    fn a_held_lock_is_live_and_a_released_one_is_dead() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.json");
        assert_eq!(probe(&path).expect("probes"), Liveness::Unrecorded);
        let lock = HostLock::acquire(&path, Presence::CreateIfMissing).expect("locks");
        assert_eq!(probe(&path).expect("probes"), Liveness::Live);
        drop(lock);
        assert_eq!(probe(&path).expect("probes"), Liveness::Dead);
    }

    #[test]
    fn a_probe_in_progress_does_not_make_a_start_look_like_a_second_host() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.json");
        drop(HostLock::acquire(&path, Presence::CreateIfMissing).expect("locks"));
        let probing = OpenOptions::new().read(true).open(&path).expect("opens");
        let shared = Flock::lock(probing, FlockArg::LockSharedNonblock).expect("shared");
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            drop(shared);
        });
        let started = HostLock::acquire(&path, Presence::MustExist);
        release.join().expect("joins");
        assert!(started.is_ok(), "a momentary probe is waited out");
    }

    #[test]
    fn forgetting_a_dead_session_removes_its_files_and_refuses_a_live_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = dir.path().join("s.json");
        let log = dir.path().join("s.log");
        std::fs::write(&log, b"log").expect("writes");
        let live = HostLock::acquire(&record, Presence::CreateIfMissing).expect("locks");
        assert!(matches!(forget(&record, &log), Err(LockError::Held)));
        assert!(record.exists() && log.exists());
        drop(live);
        forget(&record, &log).expect("forgets");
        assert!(!record.exists() && !log.exists());
        forget(&record, &log).expect("forgetting nothing is fine");
    }

    #[test]
    fn a_second_host_for_one_record_is_refused_while_the_first_lives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.json");
        let _first = HostLock::acquire(&path, Presence::CreateIfMissing).expect("locks");
        assert!(matches!(
            HostLock::acquire(&path, Presence::CreateIfMissing),
            Err(LockError::Held)
        ));
    }

    #[test]
    fn the_probe_does_not_keep_the_lock_it_took() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.json");
        drop(HostLock::acquire(&path, Presence::CreateIfMissing).expect("locks"));
        assert_eq!(probe(&path).expect("probes"), Liveness::Dead);
        assert!(HostLock::acquire(&path, Presence::CreateIfMissing).is_ok());
    }

    #[test]
    fn a_record_is_written_in_place_and_read_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.json");
        let mut lock = HostLock::acquire(&path, Presence::CreateIfMissing).expect("locks");
        assert_eq!(lock.load().expect("loads"), None);
        lock.store(&record()).expect("stores");
        let longer = HostRecord {
            args: vec!["a much longer argument than before".into()],
            ..record()
        };
        lock.store(&longer).expect("stores");
        lock.store(&record()).expect("stores");
        assert_eq!(lock.load().expect("loads"), Some(record()));
        drop(lock);
        let on_disk: HostRecord =
            serde_json::from_slice(&std::fs::read(&path).expect("reads")).expect("valid json");
        assert_eq!(on_disk, record());
    }
}

#[cfg(test)]
mod presence_tests {
    use super::*;

    #[test]
    fn a_start_with_nothing_to_run_makes_no_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.json");
        assert!(matches!(
            HostLock::acquire(&path, Presence::MustExist),
            Err(LockError::Missing)
        ));
        assert!(!path.exists());
        assert_eq!(probe(&path).expect("probes"), Liveness::Unrecorded);
    }

    #[test]
    fn a_start_refused_because_a_host_runs_leaves_its_record_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.json");
        let first = HostLock::acquire(&path, Presence::CreateIfMissing).expect("locks");
        assert!(first.created());
        assert!(matches!(
            HostLock::acquire(&path, Presence::CreateIfMissing),
            Err(LockError::Held)
        ));
        assert!(path.exists(), "the live host's record stays");
    }
}
