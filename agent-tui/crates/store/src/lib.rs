use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agent_core::activity::{Activity, Going, Reported, Stopped};
use agent_core::names::SessionName;
use agent_core::paths::{PathError, RepoRoot, home};
use agent_core::ports::{AgentRecord, Hint, Listing, Store, StoreError, Unlisting};
use nix::fcntl::{Flock, FlockArg};

/// Pins and agent records under `$XDG_STATE_HOME/agent`. Read-modify-write
/// cycles hold one advisory lock for the whole store, so the CLI and a
/// running TUI don't lose each other's updates. It is global because a hold
/// is one small read and an atomic rename, and a lock per session would
/// leave a file behind for each or race with `forget` removing it.
pub struct FsStore {
    dir: PathBuf,
}

impl FsStore {
    /// Replaces `name`'s activity with what `change` makes of it, if it has
    /// one.
    fn rewrite(
        &self,
        name: &SessionName,
        change: impl FnOnce(Activity) -> Activity,
    ) -> Result<(), StoreError> {
        let _lock = self.lock()?;
        let path = self.activity_path(name);
        let Some(activity) = Self::read_activity(&path)? else {
            return Ok(());
        };
        let json = serde_json::to_vec(&change(activity)).expect("activity serializes");
        write_atomic(&path, &json)
    }

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

    fn repos_path(&self) -> PathBuf {
        self.dir.join("repos")
    }

    fn hint_path(&self, hint: Hint) -> PathBuf {
        self.dir.join("hints").join(hint.name())
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
        let contents = match std::fs::read(path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err(path)(e)),
        };
        serde_json::from_slice(&contents)
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

    /// Sets a corrupt activity file aside as `<name>.<time>-<n>.corrupt`
    /// and notes it in `errors.log`, so an agent's state can move on without
    /// the loss going unseen. Neither step may fail the caller: a hook has
    /// no way to say so and the TUI's terminal isn't a place to print.
    fn quarantine(&self, path: &Path, why: &StoreError) {
        let now = unix_seconds();
        let aside = (0..u32::MAX)
            .map(|n| path.with_extension(format!("{now}-{n}.corrupt")))
            .find(|candidate| !candidate.exists());
        if let Some(aside) = aside {
            let _ = std::fs::rename(path, aside);
        }
        self.log(&format!("replaced a corrupt activity file: {why}"));
    }

    /// Appends to `errors.log`, moving it to `errors.log.old` once it passes
    /// [`LOG_BYTES`].
    fn log(&self, message: &str) {
        let log = self.dir.join("errors.log");
        if std::fs::metadata(&log).is_ok_and(|m| m.len() > LOG_BYTES) {
            let _ = std::fs::rename(&log, self.dir.join("errors.log.old"));
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log) {
            let _ = writeln!(file, "[{}] {message}", unix_seconds());
        }
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

impl FsStore {
    fn read_repos(&self) -> Result<Vec<RepoRoot>, StoreError> {
        let path = self.repos_path();
        let Some(contents) = read_optional(&path)? else {
            return Ok(Vec::new());
        };
        contents
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                RepoRoot::new(PathBuf::from(l)).map_err(|e| StoreError::Corrupt {
                    path: path.clone(),
                    detail: format!("{e}"),
                })
            })
            .collect()
    }

    /// Applies `change` to the listed repos under the store lock, writing
    /// only if it changed anything, and says whether it did.
    fn update_repos(&self, change: impl FnOnce(&mut Vec<RepoRoot>)) -> Result<bool, StoreError> {
        let _lock = self.lock()?;
        let before = self.read_repos()?;
        let mut repos = before.clone();
        change(&mut repos);
        if repos == before {
            return Ok(false);
        }
        let contents: String = repos
            .iter()
            .flat_map(|r| [r.path().to_string_lossy().into_owned(), "\n".into()])
            .collect();
        write_atomic(&self.repos_path(), contents.as_bytes())?;
        Ok(true)
    }
}

/// How large `errors.log` grows before it is set aside.
const LOG_BYTES: u64 = 64 * 1024;

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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
    fn hint_shown(&self, hint: Hint) -> Result<bool, StoreError> {
        let path = self.hint_path(hint);
        path.try_exists().map_err(io_err(&path))
    }

    fn mark_hint_shown(&self, hint: Hint) -> Result<(), StoreError> {
        write_atomic(&self.hint_path(hint), b"")
    }

    fn note(&self, message: &str) {
        self.log(message);
    }

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

    fn repos(&self) -> Result<Vec<RepoRoot>, StoreError> {
        self.read_repos()
    }

    fn list_repo(&self, repo: &RepoRoot) -> Result<Listing, StoreError> {
        let changed = self.update_repos(|repos| {
            if !repos.contains(repo) {
                repos.push(repo.clone());
            }
        })?;
        Ok(if changed {
            Listing::Added
        } else {
            Listing::AlreadyListed
        })
    }

    fn unlist_repo(&self, repo: &RepoRoot) -> Result<Unlisting, StoreError> {
        let changed = self.update_repos(|repos| repos.retain(|r| r != repo))?;
        Ok(if changed {
            Unlisting::Dropped
        } else {
            Unlisting::NotListed
        })
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
        let previous = match Self::read_activity(&path) {
            Ok(previous) => previous,
            Err(corrupt @ StoreError::Corrupt { .. }) => {
                self.quarantine(&path, &corrupt);
                None
            }
            Err(e) => return Err(e),
        };
        let next = Activity::next(previous, event, SystemTime::now());
        let json = serde_json::to_vec(&next).expect("activity serializes");
        write_atomic(&path, &json)
    }

    fn acknowledge(&self, name: &SessionName) -> Result<(), StoreError> {
        self.rewrite(name, |activity| activity.acknowledged(SystemTime::now()))
    }

    fn interrupt(
        &self,
        name: &SessionName,
        proof: Going,
        at: SystemTime,
    ) -> Result<(), StoreError> {
        self.rewrite(name, |activity| activity.interrupted(proof, at))
    }

    fn resume(&self, name: &SessionName, proof: Stopped) -> Result<(), StoreError> {
        self.rewrite(name, |activity| activity.resumed(proof, SystemTime::now()))
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
        s.parse().expect("parses")
    }

    #[test]
    fn pins_keep_their_order_and_round_trip() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = FsStore::new(dir.path().to_path_buf());
        store.pin(&name("b")).expect("succeeds");
        store.pin(&name("a")).expect("succeeds");
        store.pin(&name("b")).expect("succeeds");
        assert_eq!(
            store.pinned().expect("succeeds"),
            vec![name("b"), name("a")]
        );
        store.unpin(&name("b")).expect("succeeds");
        assert_eq!(store.pinned().expect("succeeds"), vec![name("a")]);
    }

    #[test]
    fn listed_repos_keep_their_order_and_round_trip() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = FsStore::new(dir.path().to_path_buf());
        let repo = |p: &str| RepoRoot::new(PathBuf::from(p)).expect("absolute");
        assert_eq!(
            store.list_repo(&repo("/src/b")).expect("store"),
            Listing::Added
        );
        assert_eq!(
            store.list_repo(&repo("/src/a")).expect("store"),
            Listing::Added
        );
        assert_eq!(
            store.list_repo(&repo("/src/b")).expect("store"),
            Listing::AlreadyListed
        );
        assert_eq!(
            store.repos().expect("store"),
            vec![repo("/src/b"), repo("/src/a")]
        );
        assert_eq!(
            store.unlist_repo(&repo("/src/b")).expect("store"),
            Unlisting::Dropped
        );
        assert_eq!(
            store.unlist_repo(&repo("/src/b")).expect("store"),
            Unlisting::NotListed
        );
        assert_eq!(store.repos().expect("store"), vec![repo("/src/a")]);
        std::fs::write(dir.path().join("repos"), "relative\n").expect("writes");
        assert!(matches!(store.repos(), Err(StoreError::Corrupt { .. })));
    }

    #[test]
    fn corrupt_pins_are_an_error_not_an_empty_list() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join("pinned"), "ok\n-bad\n").expect("writes");
        let store = FsStore::new(dir.path().to_path_buf());
        assert!(matches!(store.pinned(), Err(StoreError::Corrupt { .. })));
        assert!(store.pin(&name("x")).is_err());
    }

    #[test]
    fn records_round_trip_and_forget_unpins() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = FsStore::new(dir.path().to_path_buf());
        let record = AgentRecord::new(
            "claude".parse().expect("parses"),
            Some("go".parse().expect("parses")),
        );
        store.put_record(&name("a"), &record).expect("succeeds");
        store.pin(&name("a")).expect("succeeds");
        assert_eq!(store.record(&name("a")).expect("succeeds"), Some(record));
        store.forget(&name("a")).expect("succeeds");
        assert_eq!(store.record(&name("a")).expect("succeeds"), None);
        assert_eq!(
            store.pinned().expect("succeeds"),
            [] as [agent_core::names::SessionName; 0]
        );
    }

    fn forgetting_unpins_and_drops_the_record(store: &dyn Store) {
        let record = AgentRecord::new("claude".parse().expect("parses"), None);
        store.put_record(&name("a"), &record).expect("succeeds");
        store.pin(&name("a")).expect("succeeds");
        store.pin(&name("b")).expect("succeeds");
        store.forget(&name("a")).expect("succeeds");
        assert_eq!(store.record(&name("a")).expect("succeeds"), None);
        assert_eq!(store.pinned().expect("succeeds"), [name("b")]);
    }

    #[test]
    fn the_in_memory_store_forgets_like_the_file_store() {
        let dir = tempfile::tempdir().expect("a temp dir");
        forgetting_unpins_and_drops_the_record(&FsStore::new(dir.path().to_path_buf()));
        forgetting_unpins_and_drops_the_record(&agent_core::testing::MemStore::default());
    }

    #[test]
    fn a_hint_is_shown_until_it_is_marked() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = FsStore::new(dir.path().to_path_buf());
        assert!(!store.hint_shown(Hint::Keys).expect("succeeds"));
        store.mark_hint_shown(Hint::Keys).expect("succeeds");
        assert!(store.hint_shown(Hint::Keys).expect("succeeds"));
        let again = FsStore::new(dir.path().to_path_buf());
        assert!(again.hint_shown(Hint::Keys).expect("succeeds"));
    }

    #[test]
    fn the_last_selection_round_trips() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = FsStore::new(dir.path().to_path_buf());
        assert_eq!(store.last_selected().expect("succeeds"), None);
        store.set_last_selected(&name("fix")).expect("succeeds");
        assert_eq!(store.last_selected().expect("succeeds"), Some(name("fix")));
    }

    fn corrupt_files(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .expect("reads")
            .map(|e| e.expect("entry").path())
            .filter(|p| p.extension().is_some_and(|e| e == "corrupt"))
            .collect()
    }

    #[test]
    fn a_note_lands_in_the_error_log() {
        let dir = tempfile::tempdir().expect("a temp dir");
        FsStore::new(dir.path().to_path_buf()).note("something odd");
        let log = std::fs::read_to_string(dir.path().join("errors.log")).expect("logged");
        assert!(
            log.starts_with('[') && log.trim_end().ends_with("something odd"),
            "{log}"
        );
    }

    #[test]
    fn corrupt_activity_files_are_replaced_but_set_aside_and_logged() {
        use agent_core::activity::MainEvent;
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = FsStore::new(dir.path().to_path_buf());
        let activity = dir.path().join("activity");
        std::fs::create_dir_all(&activity).expect("creates");
        for bad in [&b"{not json"[..], &[0xff, 0xfe, 0x00][..]] {
            std::fs::write(activity.join("a.json"), bad).expect("writes");
            store
                .report(&name("a"), Reported::main(MainEvent::Started))
                .expect("reports");
            assert!(store.activities().expect("reads").contains_key(&name("a")));
        }
        let kept = corrupt_files(&activity);
        assert_eq!(kept.len(), 2, "{kept:?}");
        let log = std::fs::read_to_string(dir.path().join("errors.log")).expect("logged");
        assert_eq!(log.lines().count(), 2, "{log}");
        assert!(log.starts_with('[') && log.contains("a.json"), "{log}");
    }

    #[test]
    fn an_interrupt_is_written_only_for_the_activity_it_was_proved_for_and_can_be_undone() {
        use agent_core::activity::{Input, MainEvent, State};
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = FsStore::new(dir.path().to_path_buf());
        let prompt = || Reported::main(MainEvent::Prompted(Input::User("go".into())));
        let state = || store.activities().expect("reads")[&name("a")].state.clone();
        store.report(&name("a"), prompt()).expect("reports");
        let stale = store.activities().expect("reads")[&name("a")]
            .going()
            .expect("going");
        store.report(&name("a"), prompt()).expect("reports");
        store
            .interrupt(&name("a"), stale, SystemTime::now())
            .expect("succeeds");
        assert!(matches!(state(), State::Working { .. }));

        let current = store.activities().expect("reads")[&name("a")]
            .going()
            .expect("going");
        store
            .interrupt(&name("a"), current, SystemTime::now())
            .expect("succeeds");
        assert_eq!(state(), State::Interrupted);

        let stopped = store.activities().expect("reads")[&name("a")]
            .stopped()
            .expect("stopped");
        store.resume(&name("a"), stopped).expect("succeeds");
        assert!(matches!(state(), State::Working { .. }));
        store
            .resume(&name("never-reported"), stopped)
            .expect("nothing to change is fine");
    }

    #[test]
    fn the_error_log_is_set_aside_once_it_grows_large() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let store = FsStore::new(dir.path().to_path_buf());
        std::fs::write(dir.path().join("errors.log"), vec![b'x'; 70_000]).expect("writes");
        store.log("fresh");
        let log = std::fs::read_to_string(dir.path().join("errors.log")).expect("reads");
        assert_eq!(log.lines().count(), 1);
        assert!(dir.path().join("errors.log.old").exists());
    }

    #[test]
    fn concurrent_pins_are_not_lost() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    FsStore::new(path)
                        .pin(&name(&format!("a{i}")))
                        .expect("succeeds");
                })
            })
            .collect();
        for h in handles {
            h.join().expect("succeeds");
        }
        assert_eq!(FsStore::new(path).pinned().expect("succeeds").len(), 8);
    }
}
