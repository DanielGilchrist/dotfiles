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

/// How text that starts like a path reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// `/`, `~`, `~/`, `.`, `./` or `..`, made absolute against `base`.
    Path(PathBuf),
    /// `~user/...`, which isn't supported.
    Unsupported,
    /// `~` with no home directory to stand for.
    NoHome,
}

/// `text` as a path when it starts like one, with `~` as the home
/// directory and `.` or `..` relative to `base`. Anything else is a name.
pub fn typed_path(text: &str, base: &Path) -> Option<Typed> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix('~') {
        if !(rest.is_empty() || rest.starts_with('/')) {
            return Some(Typed::Unsupported);
        }
        return Some(match home() {
            Ok(home) => Typed::Path(home.join(rest.trim_start_matches('/'))),
            Err(_) => Typed::NoHome,
        });
    }
    if text.starts_with('/') {
        return Some(Typed::Path(PathBuf::from(text)));
    }
    if text.starts_with("./") || text.starts_with("../") || text == "." || text == ".." {
        return Some(Typed::Path(base.join(text)));
    }
    None
}

/// `path` with `.` dropped and `..` folded into the component before it,
/// without touching the filesystem, so a path that no longer exists still
/// compares equal to how it was stored.
pub fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir) | None
                ) {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    out
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

    /// Whether a checkout is there: a worktree's `.git` file, and git no
    /// longer holding the worktree while it checks the files out.
    pub fn checked_out(&self) -> bool {
        self.checkout() == Checkout::Ready
    }

    /// What's at the worktree's path.
    pub fn checkout(&self) -> Checkout {
        let Ok(dot_git) = std::fs::read_to_string(self.path.join(".git")) else {
            return Checkout::Gone;
        };
        // `git worktree add` writes `locked` with this reason into the
        // worktree's admin dir while it checks the files out
        // (builtin/worktree.c, add_worktree), so the directory fills in
        // behind a `.git` file that's already there.
        let gitdir = dot_git
            .lines()
            .find_map(|line| line.strip_prefix("gitdir: "))
            .map(|dir| PathBuf::from(dir.trim()));
        let initialising = gitdir.is_some_and(|dir| {
            std::fs::read_to_string(dir.join("locked"))
                .is_ok_and(|reason| reason.trim() == "initializing")
        });
        if initialising {
            Checkout::Initialising
        } else {
            Checkout::Ready
        }
    }
}

/// What an agent worktree's directory holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkout {
    /// A finished checkout.
    Ready,
    /// `git worktree add` is still filling it in.
    Initialising,
    /// No `.git`: a directory left behind by a removal.
    Gone,
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

    /// The agent worktree containing `path` whose checkout is still there.
    pub fn enclosing_checkout(&self, path: &Path) -> Option<AgentWorktree> {
        self.enclosing(path).filter(AgentWorktree::checked_out)
    }

    /// The agent worktree containing `path` (or equal to it), by its path
    /// alone.
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
                found.extend(
                    self.worktree_at(&path)
                        .filter(|wt| wt.checkout() == Checkout::Ready),
                );
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
        Layout::new("/w".into()).expect("absolute")
    }

    fn repo(path: &str) -> RepoRoot {
        RepoRoot::new(path.into()).expect("absolute")
    }

    #[test]
    fn worktree_paths_stay_under_the_root() {
        let wt = layout().worktree(&repo("/src/api"), "fix".parse().expect("parses"));
        assert_eq!(wt.path(), Path::new("/w/api/fix"));
    }

    #[test]
    fn enclosing_finds_the_agent_worktree_around_a_path() {
        let wt = layout()
            .enclosing(Path::new("/w/api/fix/src/main.rs"))
            .expect("succeeds");
        assert_eq!(wt.path(), Path::new("/w/api/fix"));
        assert_eq!(wt.name().as_str(), "fix");
    }

    #[test]
    fn a_worktree_git_is_still_creating_isnt_scanned() {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let layout = Layout::new(tmp.path().join("w")).expect("absolute");
        let admin = tmp.path().join("repo/.git/worktrees/new");
        std::fs::create_dir_all(&admin).expect("creates");
        let new = layout.root().join("api/new");
        std::fs::create_dir_all(&new).expect("creates");
        std::fs::write(new.join(".git"), format!("gitdir: {}\n", admin.display())).expect("writes");
        std::fs::write(admin.join("locked"), "initializing").expect("writes");
        let wt = layout.worktree_at(&new).expect("an agent worktree");
        assert_eq!(wt.checkout(), Checkout::Initialising);
        assert!(layout.scan().expect("scans").is_empty(), "not listed yet");
        std::fs::remove_file(admin.join("locked")).expect("removes");
        assert_eq!(wt.checkout(), Checkout::Ready);
        assert_eq!(layout.scan().expect("scans").len(), 1);
        std::fs::remove_file(new.join(".git")).expect("removes");
        assert_eq!(wt.checkout(), Checkout::Gone);
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
        let wt = layout().worktree(&repo("/home/d/.config"), "fix".parse().expect("parses"));
        assert_eq!(wt.path(), Path::new("/w/_config/fix"));
        assert_eq!(layout().worktree_at(wt.path()), Some(wt));
        let trash = layout().worktree(&repo("/src/.trash"), "x".parse().expect("parses"));
        assert!(!trash.path().starts_with(layout().trash()));
    }

    #[test]
    fn typed_paths_expand_home_and_anchor_relative_ones() {
        let home = home().expect("HOME");
        let base = Path::new("/w/api/fix");
        assert_eq!(
            typed_path("~/code", base),
            Some(Typed::Path(home.join("code")))
        );
        assert_eq!(typed_path("~", base), Some(Typed::Path(home.clone())));
        assert_eq!(typed_path("~bob/x", base), Some(Typed::Unsupported));
        assert_eq!(
            typed_path("../other", base),
            Some(Typed::Path(base.join("../other")))
        );
        assert_eq!(typed_path("/srv", base), Some(Typed::Path("/srv".into())));
        assert_eq!(typed_path("api", base), None);
    }

    #[test]
    fn normalising_folds_dots_without_the_filesystem() {
        assert_eq!(normalise(Path::new("/a/b/../c/./d")), Path::new("/a/c/d"));
        assert_eq!(normalise(Path::new("/../a")), Path::new("/a"));
        assert_eq!(normalise(Path::new("a/../../b")), Path::new("b"));
    }

    #[test]
    fn relative_roots_are_rejected() {
        assert!(Layout::new("worktrees".into()).is_err());
        assert!(RepoRoot::new("src/api".into()).is_err());
    }
}
