use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agent_core::activity::{Activity, Reported};
use agent_core::names::SessionName;
use agent_core::paths::{PathError, home};
use agent_core::ports::{AgentRecord, Store, StoreError};
use nix::fcntl::{Flock, FlockArg};

/// Pins and agent records under `$XDG_STATE_HOME/agent`. Read-modify-write
/// cycles hold an advisory lock, so the CLI and a running TUI don't lose
/// each other's updates.
pub struct FsStore {
    dir: PathBuf,
}

impl FsStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn from_env() -> Result<Self, PathError> {
        let base = match std::env::var_os("XDG_STATE_HOME").map(PathBuf::from) {
            Some(dir) if dir.is_absolute() => dir,
            _ => home()?.join(".local/state"),
        };
        Ok(Self::new(base.join("agent")))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn pins_path(&self) -> PathBuf {
        self.dir.join("pinned")
    }

    fn selection_path(&self) -> PathBuf {
        self.dir.join("selected.json")
    }

    fn record_path(&self, name: &SessionName) -> PathBuf {
        self.dir.join("records").join(format!("{name}.json"))
    }

    fn activity_dir(&self) -> PathBuf {
        self.dir.join("activity")
    }

    fn activity_path(&self, name: &SessionName) -> PathBuf {
        self.activity_dir().join(format!("{name}.json"))
    }

    fn read_activity(path: &Path) -> Result<Option<Activity>, StoreError> {
        let Some(contents) = read_optional(path)? else {
            return Ok(None);
        };
        serde_json::from_str(&contents)
            .map(Some)
            .map_err(|e| StoreError::Corrupt {
                path: path.to_path_buf(),
                detail: e.to_string(),
            })
    }

    fn lock(&self) -> Result<Flock<File>, StoreError> {
        std::fs::create_dir_all(&self.dir).map_err(io_err(&self.dir))?;
        let path = self.dir.join(".lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io_err(&path))?;
        Flock::lock(file, FlockArg::LockExclusive).map_err(|(_, errno)| StoreError::Io {
            path,
            source: io::Error::from(errno),
        })
    }

    fn read_pins(&self) -> Result<Vec<SessionName>, StoreError> {
        let path = self.pins_path();
        let Some(contents) = read_optional(&path)? else {
            return Ok(Vec::new());
        };
        contents
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                l.parse().map_err(|e| StoreError::Corrupt {
                    path: path.clone(),
                    detail: format!("{e}"),
                })
            })
            .collect()
    }
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, StoreError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(contents)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path)(e)),
    }
}

/// Write via a temp file in the same directory and rename into place, so a
/// crash never leaves a truncated file.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().expect("store paths have a parent");
    std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io_err(dir))?;
    tmp.write_all(contents).map_err(io_err(path))?;
    tmp.persist(path).map_err(|e| io_err(path)(e.error))?;
    Ok(())
}

impl FsStore {
    /// Applies `change` to the pins under the store lock, writing only if it
    /// changed anything.
    fn update_pins(&self, change: impl FnOnce(&mut Vec<SessionName>)) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        let before = self.read_pins()?;
        let mut pins = before.clone();
        change(&mut pins);
        if pins == before {
            return Ok(());
        }
        let contents: String = pins.iter().flat_map(|p| [p.as_str(), "\n"]).collect();
        write_atomic(&self.pins_path(), contents.as_bytes())
    }
}

impl Store for FsStore {
    fn pinned(&self) -> Result<Vec<SessionName>, StoreError> {
        self.read_pins()
    }

    fn pin(&self, name: &SessionName) -> Result<(), StoreError> {
        self.update_pins(|pins| {
            if !pins.contains(name) {
                pins.push(name.clone());
            }
        })
    }

    fn unpin(&self, name: &SessionName) -> Result<(), StoreError> {
        self.update_pins(|pins| pins.retain(|p| p != name))
    }

    fn record(&self, name: &SessionName) -> Result<Option<AgentRecord>, StoreError> {
        let path = self.record_path(name);
        let Some(contents) = read_optional(&path)? else {
            return Ok(None);
        };
        serde_json::from_str(&contents)
            .map(Some)
            .map_err(|e| StoreError::Corrupt {
                path,
                detail: e.to_string(),
            })
    }

    fn put_record(&self, name: &SessionName, record: &AgentRecord) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        let json = serde_json::to_vec_pretty(record).expect("records serialize");
        write_atomic(&self.record_path(name), &json)
    }

    fn last_selected(&self) -> Result<Option<SessionName>, StoreError> {
        let path = self.selection_path();
        let Some(contents) = read_optional(&path)? else {
            return Ok(None);
        };
        serde_json::from_str(&contents)
            .map(Some)
            .map_err(|e| StoreError::Corrupt {
                path,
                detail: e.to_string(),
            })
    }

    fn set_last_selected(&self, name: &SessionName) -> Result<(), StoreError> {
        let json = serde_json::to_vec(name).expect("names serialize");
        write_atomic(&self.selection_path(), &json)
    }

    fn forget(&self, name: &SessionName) -> Result<(), StoreError> {
        self.unpin(name)?;
        for path in [self.record_path(name), self.activity_path(name)] {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(io_err(&path)(e)),
                _ => {}
            }
        }
        Ok(())
    }

    fn report(&self, name: &SessionName, event: Reported) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        let path = self.activity_path(name);
        // A corrupt file shouldn't stop an agent's state from moving on.
        let previous = Self::read_activity(&path).ok().flatten();
        let next = Activity::next(previous, event, SystemTime::now());
        let json = serde_json::to_vec(&next).expect("activity serializes");
        write_atomic(&path, &json)
    }

    fn acknowledge(&self, name: &SessionName) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        let path = self.activity_path(name);
        let Some(activity) = Self::read_activity(&path)? else {
            return Ok(());
        };
        let json = serde_json::to_vec(&activity.acknowledged(SystemTime::now()))
            .expect("activity serializes");
        write_atomic(&path, &json)
    }

    /// Unreadable files (corrupt, or from an older format) are left out
    /// rather than hiding everyone else's activity.
    fn activities(&self) -> Result<HashMap<SessionName, Activity>, StoreError> {
        let dir = self.activity_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(e) => return Err(io_err(&dir)(e)),
        };
        let mut activities = HashMap::new();
        for path in entries.filter_map(Result::ok).map(|e| e.path()) {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<SessionName>().ok());
            if let (Some(name), Some("json")) = (name, path.extension().and_then(|e| e.to_str()))
                && let Ok(Some(activity)) = Self::read_activity(&path)
            {
                activities.insert(name, activity);
            }
        }
        Ok(activities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> SessionName {
        s.parse().unwrap()
    }

    #[test]
    fn pins_keep_their_order_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsStore::new(dir.path().to_path_buf());
        store.pin(&name("b")).unwrap();
        store.pin(&name("a")).unwrap();
        store.pin(&name("b")).unwrap();
        assert_eq!(store.pinned().unwrap(), vec![name("b"), name("a")]);
        store.unpin(&name("b")).unwrap();
        assert_eq!(store.pinned().unwrap(), vec![name("a")]);
    }

    #[test]
    fn corrupt_pins_are_an_error_not_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pinned"), "ok\n-bad\n").unwrap();
        let store = FsStore::new(dir.path().to_path_buf());
        assert!(matches!(store.pinned(), Err(StoreError::Corrupt { .. })));
        assert!(store.pin(&name("x")).is_err());
    }

    #[test]
    fn records_round_trip_and_forget_unpins() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsStore::new(dir.path().to_path_buf());
        let pending = agent_core::ports::PendingPrompt::now("go".parse().unwrap());
        let record = AgentRecord {
            harness: "claude".parse().unwrap(),
            pending: Some(pending),
        };
        store.put_record(&name("a"), &record).unwrap();
        store.pin(&name("a")).unwrap();
        assert_eq!(store.record(&name("a")).unwrap(), Some(record));
        store.forget(&name("a")).unwrap();
        assert_eq!(store.record(&name("a")).unwrap(), None);
        assert_eq!(
            store.pinned().unwrap(),
            [] as [agent_core::names::SessionName; 0]
        );
    }

    #[test]
    fn the_last_selection_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = FsStore::new(dir.path().to_path_buf());
        assert_eq!(store.last_selected().unwrap(), None);
        store.set_last_selected(&name("fix")).unwrap();
        assert_eq!(store.last_selected().unwrap(), Some(name("fix")));
    }

    #[test]
    fn concurrent_pins_are_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || FsStore::new(path).pin(&name(&format!("a{i}"))).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(FsStore::new(path).pinned().unwrap().len(), 8);
    }
}
