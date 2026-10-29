use std::borrow::Cow;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::names::AgentName;

const TRASH_DIR: &str = ".trash";

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("{0} is not an absolute path")]
    NotAbsolute(PathBuf),
    #[error("{0} has no final component")]
    NoName(PathBuf),
    #[error("HOME is not set")]
    NoHome,
}

pub fn home() -> Result<PathBuf, PathError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(PathError::NoHome)?;
    absolute(home)
}

fn absolute(path: PathBuf) -> Result<PathBuf, PathError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(PathError::NotAbsolute(path))
    }
}

/// The main worktree of a git repository: the identity of a repo.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoRoot(PathBuf);

impl RepoRoot {
    pub fn new(path: PathBuf) -> Result<Self, PathError> {
        let path = absolute(path)?;
        if path.file_name().is_none() {
            return Err(PathError::NoName(path));
        }
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn name(&self) -> Cow<'_, str> {
        self.0
            .file_name()
            .map_or(Cow::Borrowed(""), |n| n.to_string_lossy())
    }
}

/// `<root>/<repo dir>/<agent name>`. Only [`Layout`] constructs these, so the
/// path always lies directly under the worktrees root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentWorktree {
    path: PathBuf,
    name: AgentName,
}

impl AgentWorktree {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn name(&self) -> &AgentName {
        &self.name
    }
}

/// Where agent worktrees live on disk.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: PathBuf) -> Result<Self, PathError> {
        Ok(Self {
            root: absolute(root)?,
        })
    }

    /// `$AGENT_WORKTREES`, defaulting to `~/worktrees`.
    pub fn from_env() -> Result<Self, PathError> {
        match std::env::var_os("AGENT_WORKTREES") {
            Some(root) => Self::new(PathBuf::from(root)),
            None => Self::new(home()?.join("worktrees")),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory for a repo's agents. A leading `.` becomes `_`, so
    /// dot-named repos (`~/.config`) aren't hidden and can't be `.trash`.
    fn repo_dir(repo: &RepoRoot) -> String {
        let name = repo.name();
        match name.strip_prefix('.') {
            Some(rest) => format!("_{rest}"),
            None => name.into_owned(),
        }
    }

    pub fn worktree(&self, repo: &RepoRoot, name: AgentName) -> AgentWorktree {
        let path = self.root.join(Self::repo_dir(repo)).join(name.as_str());
        AgentWorktree { path, name }
    }

    /// The agent worktree whose directory is exactly `path`.
    pub fn worktree_at(&self, path: &Path) -> Option<AgentWorktree> {
        self.enclosing(path).filter(|wt| wt.path == path)
    }

    /// The agent worktree containing `path` (or equal to it).
    pub fn enclosing(&self, path: &Path) -> Option<AgentWorktree> {
        let mut components = path.strip_prefix(&self.root).ok()?.components();
        let (Some(Component::Normal(repo)), Some(Component::Normal(name))) =
            (components.next(), components.next())
        else {
            return None;
        };
        if repo.to_string_lossy().starts_with('.') {
            return None;
        }
        let name: AgentName = name.to_str()?.parse().ok()?;
        Some(AgentWorktree {
            path: self.root.join(repo).join(name.as_str()),
            name,
        })
    }

    pub fn scan(&self) -> io::Result<Vec<AgentWorktree>> {
        let repos = match std::fs::read_dir(&self.root) {
            Ok(repos) => repos,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut found = Vec::new();
        for repo in repos {
            let repo = repo?;
            if !repo.file_type()?.is_dir() {
                continue;
            }
            for agent in std::fs::read_dir(repo.path())? {
                let path = agent?.path();
                if path.join(".git").exists() {
                    found.extend(self.worktree_at(&path));
                }
            }
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(found)
    }

    pub fn named(&self, name: &AgentName) -> io::Result<Vec<AgentWorktree>> {
        Ok(self
            .scan()?
            .into_iter()
            .filter(|wt| &wt.name == name)
            .collect())
    }

    pub fn trash(&self) -> PathBuf {
        self.root.join(TRASH_DIR)
    }

    /// A fresh path in the trash dir. It shares a filesystem with every
    /// worktree, so moving a worktree there is an atomic rename.
    pub fn trash_slot(&self, name: &AgentName) -> TrashSlot {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        TrashSlot(self.trash().join(format!("{name}-{nanos}")))
    }
}

/// A path inside [`Layout::trash`]; only the layout constructs these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashSlot(PathBuf);

impl TrashSlot {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::new("/w".into()).unwrap()
    }

    fn repo(path: &str) -> RepoRoot {
        RepoRoot::new(path.into()).unwrap()
    }

    #[test]
    fn worktree_paths_stay_under_the_root() {
        let wt = layout().worktree(&repo("/src/api"), "fix".parse().unwrap());
        assert_eq!(wt.path(), Path::new("/w/api/fix"));
    }

    #[test]
    fn enclosing_finds_the_agent_worktree_around_a_path() {
        let wt = layout()
            .enclosing(Path::new("/w/api/fix/src/main.rs"))
            .unwrap();
        assert_eq!(wt.path(), Path::new("/w/api/fix"));
        assert_eq!(wt.name().as_str(), "fix");
    }

    #[test]
    fn worktree_at_only_matches_the_worktree_itself() {
        assert!(layout().worktree_at(Path::new("/w/api/fix")).is_some());
        assert!(layout().worktree_at(Path::new("/w/api/fix/src")).is_none());
    }

    #[test]
    fn paths_outside_agent_worktrees_are_rejected() {
        for path in [
            "/elsewhere/api/fix",
            "/w/api",
            "/w/.trash/fix-1",
            "/w/api/.hidden",
        ] {
            assert!(layout().enclosing(Path::new(path)).is_none(), "{path}");
        }
    }

    #[test]
    fn dot_named_repos_get_visible_directories() {
        let wt = layout().worktree(&repo("/home/d/.config"), "fix".parse().unwrap());
        assert_eq!(wt.path(), Path::new("/w/_config/fix"));
        assert_eq!(layout().worktree_at(wt.path()), Some(wt));
        let trash = layout().worktree(&repo("/src/.trash"), "x".parse().unwrap());
        assert!(!trash.path().starts_with(layout().trash()));
    }

    #[test]
    fn relative_roots_are_rejected() {
        assert!(Layout::new("worktrees".into()).is_err());
        assert!(RepoRoot::new("src/api".into()).is_err());
    }
}
