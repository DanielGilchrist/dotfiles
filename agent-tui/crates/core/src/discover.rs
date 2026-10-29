use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::model::{Checkout, Entry, EntryKind, Repo, RepoKey, Snapshot, Worktree};
use crate::names::{NameError, SessionName};
use crate::paths::{Layout, RepoRoot};
use crate::ports::{HostError, RepoWorktrees, SessionHost, SessionInfo, Vcs};
use crate::process::{CommandError, parallel_map};

/// Something discovery skipped, without failing as a whole.
#[derive(Debug)]
pub enum Warning {
    Scan {
        root: PathBuf,
        error: io::Error,
    },
    Repo {
        path: PathBuf,
        error: CommandError,
    },
    /// A repo whose directory name isn't a usable session name.
    Unnamed {
        root: RepoRoot,
        error: NameError,
    },
    /// Two checkouts want the same session name; the later one is hidden.
    NameClash {
        name: SessionName,
        hidden: PathBuf,
    },
    /// A listed repo that isn't a git repo any more.
    NotARepo {
        path: PathBuf,
    },
    /// A `[repos] dirs` entry that couldn't be read.
    Catalogue {
        dir: PathBuf,
        error: io::Error,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scan { root, error } => write!(f, "scanning {}: {error}", root.display()),
            Self::Repo { path, error } => write!(f, "{}: {error}", path.display()),
            Self::Unnamed { root, error } => {
                write!(f, "{} has no session: {error}", root.path().display())
            }
            Self::NameClash { name, hidden } => {
                write!(f, "{} is hidden: {name} is already taken", hidden.display())
            }
            Self::NotARepo { path } => {
                write!(
                    f,
                    "{} isn't a git repo (agent forget drops it)",
                    path.display()
                )
            }
            Self::Catalogue { dir, error } => {
                write!(f, "repos.dirs {}: {error}", dir.display())
            }
        }
    }
}

/// Everything discovery saw, before it's joined into a [`Snapshot`].
#[derive(Debug, Default)]
pub struct Observation {
    pub sessions: Vec<SessionInfo>,
    pub repos: Vec<(RepoRoot, RepoWorktrees)>,
    pub warnings: Vec<Warning>,
}

/// Where a candidate path came from, which decides what not being in a
/// repo means for it. Listed sorts first, so a path that's both keeps its
/// warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Candidate {
    /// A listed repo: not in a repo any more is worth a warning.
    Listed,
    /// A session's cwd or an agent worktree: not in a repo is normal.
    Seen,
}

/// `listed` are repos the user keeps in the list whether or not anything
/// runs in them. One that isn't a repo any more is a warning, not an error.
pub fn observe(
    vcs: &dyn Vcs,
    host: &dyn SessionHost,
    layout: &Layout,
    listed: &[RepoRoot],
) -> Result<Observation, HostError> {
    let sessions = host.sessions()?;
    let mut warnings = Vec::new();

    let mut candidates: Vec<(PathBuf, Candidate)> = sessions
        .iter()
        .filter_map(|s| s.cwd().map(|cwd| (PathBuf::from(cwd), Candidate::Seen)))
        .collect();
    match layout.scan() {
        Ok(worktrees) => candidates.extend(
            worktrees
                .iter()
                .map(|wt| (wt.path().to_path_buf(), Candidate::Seen)),
        ),
        Err(error) => warnings.push(Warning::Scan {
            root: layout.root().to_path_buf(),
            error,
        }),
    }
    candidates.extend(
        listed
            .iter()
            .map(|root| (root.path().to_path_buf(), Candidate::Listed)),
    );
    candidates.sort();
    candidates.dedup_by(|a, b| a.0 == b.0);

    let paths: Vec<PathBuf> = candidates.iter().map(|(p, _)| p.clone()).collect();
    let mut roots = BTreeSet::new();
    for ((path, candidate), result) in candidates
        .iter()
        .zip(parallel_map(&paths, |p| vcs.repo_of(p)))
    {
        match (result, candidate) {
            (Ok(Some(root)), _) => {
                roots.insert(root);
            }
            (Ok(None), Candidate::Seen) => {}
            (Ok(None), Candidate::Listed) => {
                warnings.push(Warning::NotARepo { path: path.clone() });
            }
            (Err(error), _) => warnings.push(Warning::Repo {
                path: path.clone(),
                error,
            }),
        }
    }

    let roots: Vec<RepoRoot> = roots.into_iter().collect();
    let mut repos = Vec::new();
    for (root, result) in roots.iter().zip(parallel_map(&roots, |r| vcs.worktrees(r))) {
        match result {
            Ok(worktrees) => repos.push((root.clone(), worktrees)),
            Err(error) => warnings.push(Warning::Repo {
                path: root.path().to_path_buf(),
                error,
            }),
        }
    }
    Ok(Observation {
        sessions,
        repos,
        warnings,
    })
}

/// The repos the pickers offer beyond the snapshot, by name: each of
/// `dirs` is a repo itself (it has a `.git` directory) or a directory
/// whose children are, so `~/repos` lists every clone in it. Nothing runs
/// git: a `.git` directory is enough to offer it, and the repo is checked
/// when it's picked. Paths are canonical, as git reports main checkouts,
/// so a found repo matches its listing. Linked worktrees (a `.git` file)
/// are left out, since they'd list under another name.
#[derive(Debug, Default)]
pub struct Catalogue {
    pub repos: Vec<RepoRoot>,
    pub warnings: Vec<Warning>,
}

pub fn catalogue(dirs: &[PathBuf]) -> Catalogue {
    let mut found = BTreeSet::new();
    let mut warnings = Vec::new();
    let mut offer = |path: PathBuf, warnings: &mut Vec<Warning>| match path.canonicalize() {
        Ok(real) => found.extend(RepoRoot::new(real).ok()),
        Err(error) => warnings.push(Warning::Catalogue { dir: path, error }),
    };
    for dir in dirs {
        if has_git_dir(dir) {
            offer(dir.clone(), &mut warnings);
            continue;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                warnings.push(Warning::Catalogue {
                    dir: dir.clone(),
                    error,
                });
                continue;
            }
        };
        for entry in entries {
            match entry {
                Ok(entry) if has_git_dir(&entry.path()) => offer(entry.path(), &mut warnings),
                Ok(_) => {}
                Err(error) => warnings.push(Warning::Catalogue {
                    dir: dir.clone(),
                    error,
                }),
            }
        }
    }
    let mut repos: Vec<RepoRoot> = found.into_iter().collect();
    repos.sort_by(|a, b| a.name().cmp(&b.name()).then_with(|| a.cmp(b)));
    Catalogue { repos, warnings }
}

/// A main checkout: `.git` is a directory. A linked worktree has a file.
fn has_git_dir(path: &Path) -> bool {
    path.join(".git").is_dir()
}

/// Joins sessions to checkouts. An entry's identity is its session name:
/// the main checkout is named after the repo, agent worktrees after their
/// directory, and any other session joins the repo whose checkout holds its
/// cwd. Sessions inside no known repo become single-entry orphan repos.
pub fn assemble(
    obs: &Observation,
    layout: &Layout,
    pins: &[SessionName],
) -> (Snapshot, Vec<Warning>) {
    let sessions: HashMap<&SessionName, &SessionInfo> =
        obs.sessions.iter().map(|s| (&s.name, s)).collect();
    let session_of = |name: &SessionName| sessions.get(name).map(|s| s.status.presence());
    let mut claimed: HashSet<SessionName> = HashSet::new();
    let mut warnings = Vec::new();
    let mut grouped: Vec<(RepoKey, Vec<Entry>)> = Vec::new();

    for (root, worktrees) in &obs.repos {
        let mut entries = Vec::new();
        let mut claim = |name: SessionName, path: &PathBuf, warnings: &mut Vec<Warning>| {
            if claimed.insert(name.clone()) {
                Some(name)
            } else {
                warnings.push(Warning::NameClash {
                    name,
                    hidden: path.clone(),
                });
                None
            }
        };

        match root.name().parse::<SessionName>() {
            Ok(name) => {
                if let Some(name) = claim(name, &worktrees.main.path, &mut warnings) {
                    let kind = EntryKind::Main {
                        root: root.clone(),
                        head: worktrees.main.head.clone(),
                        session: session_of(&name),
                    };
                    entries.push(Entry::new(name.clone(), kind));
                }
            }
            Err(error) => warnings.push(Warning::Unnamed {
                root: root.clone(),
                error,
            }),
        }

        for wt in &worktrees.linked {
            let Some(worktree) = layout.worktree_at(&wt.path) else {
                continue;
            };
            let Some(name) = claim(worktree.name().session(), &wt.path, &mut warnings) else {
                continue;
            };
            let kind = EntryKind::Agent {
                session: session_of(&name),
                head: wt.head.clone(),
                worktree,
            };
            entries.push(Entry::new(name.clone(), kind));
        }

        let checkout_of = |wt: &Worktree| match layout.worktree_at(&wt.path) {
            Some(agent) => Checkout::Agent(agent),
            None => Checkout::External(wt.path.clone()),
        };
        let all: Vec<(&Worktree, Checkout)> =
            std::iter::once((&worktrees.main, Checkout::Main(root.clone())))
                .chain(worktrees.linked.iter().map(|w| (w, checkout_of(w))))
                .collect();
        for session in &obs.sessions {
            if claimed.contains(&session.name) {
                continue;
            }
            let Some(cwd) = session.cwd() else { continue };
            let Some((wt, checkout)) = all
                .iter()
                .filter(|(wt, _)| cwd.starts_with(&wt.path))
                .max_by_key(|(wt, _)| wt.path.as_os_str().len())
            else {
                continue;
            };
            claimed.insert(session.name.clone());
            let kind = EntryKind::Adopted {
                checkout: checkout.clone(),
                head: wt.head.clone(),
                presence: session.status.presence(),
            };
            entries.push(Entry::new(session.name.clone(), kind));
        }

        grouped.push((RepoKey::Root(root.clone()), entries));
    }

    for session in obs.sessions.iter().filter(|s| !claimed.contains(&s.name)) {
        let kind = EntryKind::Orphan {
            cwd: session.cwd().map(PathBuf::from),
            presence: session.status.presence(),
        };
        grouped.push((
            RepoKey::Orphan(session.name.clone()),
            vec![Entry::new(session.name.clone(), kind)],
        ));
    }

    // Pins for agents that no longer exist are kept but don't take a slot.
    let present: HashSet<&SessionName> = grouped
        .iter()
        .flat_map(|(_, es)| es.iter().map(Entry::name))
        .collect();
    let live_pins: Vec<SessionName> = pins
        .iter()
        .filter(|p| present.contains(p))
        .cloned()
        .collect();
    for (_, entries) in &mut grouped {
        for entry in entries {
            if let Some(position) = live_pins.iter().position(|p| p == entry.name()) {
                entry.pin_at(position);
            }
        }
    }

    let repos = grouped
        .into_iter()
        .filter_map(|(key, entries)| Repo::new(key, entries))
        .collect();
    (Snapshot::new(repos), warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntryState, Head, Presence};
    use crate::ports::SessionStatus;

    fn layout() -> Layout {
        Layout::new("/w".into()).expect("absolute")
    }

    fn root(path: &str) -> RepoRoot {
        RepoRoot::new(path.into()).expect("absolute")
    }

    fn name(s: &str) -> SessionName {
        s.parse().expect("parses")
    }

    fn wt(path: &str, branch: Option<&str>) -> Worktree {
        Worktree {
            path: path.into(),
            head: branch.map_or(Head::Detached, |b| Head::Branch(b.parse().expect("parses"))),
        }
    }

    fn repo(main: &str, linked: &[&str]) -> (RepoRoot, RepoWorktrees) {
        (
            root(main),
            RepoWorktrees {
                main: wt(main, Some("main")),
                linked: linked.iter().map(|p| wt(p, None)).collect(),
            },
        )
    }

    fn live(n: &str, cwd: &str) -> SessionInfo {
        SessionInfo {
            name: name(n),
            status: SessionStatus::Live {
                cwd: Some(cwd.into()),
            },
        }
    }

    fn exited(n: &str) -> SessionInfo {
        SessionInfo {
            name: name(n),
            status: SessionStatus::Exited { cwd: None },
        }
    }

    fn snapshot(obs: &Observation, pins: &[SessionName]) -> Snapshot {
        assemble(obs, &layout(), pins).0
    }

    #[test]
    fn worktrees_and_sessions_join_by_name() {
        let obs = Observation {
            sessions: vec![live("fix", "/w/api/fix"), exited("api")],
            repos: vec![repo("/src/api", &["/w/api/fix", "/w/api/idle"])],
            warnings: Vec::new(),
        };
        let snap = snapshot(&obs, &[]);
        let repo = &snap.repos()[0];
        let order: Vec<_> = repo
            .entries()
            .iter()
            .map(|e| e.name().to_string())
            .collect();
        assert_eq!(order, ["api", "fix", "idle"]);
        assert_eq!(repo.first().label(), "main");
        assert_eq!(
            repo.entry(&name("api")).expect("succeeds").presence(),
            Some(Presence::Exited)
        );
        assert_eq!(
            repo.entry(&name("fix")).expect("succeeds").presence(),
            Some(Presence::Live)
        );
        let idle = repo.entry(&name("idle")).expect("succeeds");
        assert_eq!(idle.presence(), None);
        let EntryState::Idle(at) = idle.state() else {
            panic!("idle should be startable")
        };
        assert_eq!(at.cwd(), std::path::Path::new("/w/api/idle"));
    }

    #[test]
    fn the_catalogue_offers_each_dir_or_its_children_that_are_repos() {
        let base = tempfile::tempdir().expect("a temp dir");
        let mk = |rel: &str| {
            let path = base.path().join(rel);
            std::fs::create_dir_all(&path).expect("creates");
            path
        };
        let repos = mk("repos");
        mk("repos/web/.git");
        mk("repos/api/.git");
        mk("repos/notes");
        let linked = mk("repos/linked");
        std::fs::write(linked.join(".git"), "gitdir: elsewhere").expect("writes");
        let single = mk("dots");
        mk("dots/.git");
        let via_link = base.path().join("link");
        std::os::unix::fs::symlink(&single, &via_link).expect("links");
        let missing = base.path().join("nowhere");
        let found = catalogue(&[repos.clone(), via_link, missing.clone()]);
        let names: Vec<String> = found.repos.iter().map(|r| r.name().into_owned()).collect();
        assert_eq!(
            names,
            ["api", "dots", "web"],
            "linked worktrees are left out"
        );
        assert_eq!(
            found.repos[1].path(),
            single.canonicalize().expect("exists"),
            "a symlinked dir is offered by its real path"
        );
        assert_eq!(found.warnings.len(), 1);
        assert!(
            matches!(&found.warnings[0], Warning::Catalogue { dir, .. } if *dir == missing),
            "{:?}",
            found.warnings
        );
    }

    #[test]
    fn other_sessions_are_adopted_by_the_repo_holding_their_cwd() {
        let obs = Observation {
            sessions: vec![live("scratch", "/src/api/sub/dir")],
            repos: vec![repo("/src/api", &[])],
            warnings: Vec::new(),
        };
        let snap = snapshot(&obs, &[]);
        let entry = snap.repos()[0].entry(&name("scratch")).expect("succeeds");
        assert!(matches!(
            entry.kind(),
            EntryKind::Adopted {
                checkout: Checkout::Main(_),
                ..
            }
        ));
        assert!(!matches!(entry.state(), EntryState::Idle(_)));
    }

    #[test]
    fn sessions_outside_known_repos_become_orphans() {
        let obs = Observation {
            sessions: vec![exited("stray")],
            ..Observation::default()
        };
        let snap = snapshot(&obs, &[]);
        assert!(matches!(snap.repos()[0].key(), RepoKey::Orphan(n) if n.as_str() == "stray"));
    }

    #[test]
    fn name_clashes_are_reported_not_merged() {
        let obs = Observation {
            sessions: vec![exited("api")],
            repos: vec![repo("/a/api", &[]), repo("/b/api", &[])],
            warnings: Vec::new(),
        };
        let (snap, warnings) = assemble(&obs, &layout(), &[]);
        assert_eq!(
            snap.entries()
                .filter(|(_, e)| e.name().as_str() == "api")
                .count(),
            1
        );
        assert!(matches!(warnings.as_slice(), [Warning::NameClash { .. }]));
    }

    #[test]
    fn unusable_repo_names_are_reported() {
        let obs = Observation {
            repos: vec![repo("/src/a-very-long-repository-name", &[])],
            ..Observation::default()
        };
        let (snap, warnings) = assemble(&obs, &layout(), &[]);
        assert!(snap.is_empty());
        assert!(matches!(warnings.as_slice(), [Warning::Unnamed { .. }]));
    }

    #[test]
    fn pinned_entries_sort_first_in_pin_order() {
        let obs = Observation {
            repos: vec![repo("/src/api", &["/w/api/a", "/w/api/b"])],
            ..Observation::default()
        };
        let snap = snapshot(&obs, &[name("b"), name("a")]);
        let order: Vec<_> = snap.repos()[0]
            .entries()
            .iter()
            .map(|e| e.name().to_string())
            .collect();
        assert_eq!(order, ["b", "a", "api"]);
        assert_eq!(
            snap.slot(crate::model::Slot::new(1).expect("succeeds"))
                .expect("succeeds")
                .1
                .name()
                .as_str(),
            "b"
        );
    }

    #[test]
    fn pins_for_missing_agents_dont_take_slots() {
        let obs = Observation {
            repos: vec![repo("/src/api", &["/w/api/a"])],
            ..Observation::default()
        };
        let snap = snapshot(&obs, &[name("gone"), name("a")]);
        assert_eq!(
            snap.slot(crate::model::Slot::new(1).expect("succeeds"))
                .expect("succeeds")
                .1
                .name()
                .as_str(),
            "a"
        );
    }

    #[test]
    fn external_worktrees_without_sessions_are_omitted() {
        let obs = Observation {
            repos: vec![repo("/src/api", &["/src/api/.claude/worktrees/x"])],
            ..Observation::default()
        };
        assert_eq!(snapshot(&obs, &[]).repos()[0].entries().len(), 1);
    }
}
