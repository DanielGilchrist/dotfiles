use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::PathBuf;

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

pub fn observe(
    vcs: &dyn Vcs,
    host: &dyn SessionHost,
    layout: &Layout,
) -> Result<Observation, HostError> {
    let sessions = host.sessions()?;
    let mut warnings = Vec::new();

    let mut candidates: Vec<PathBuf> = sessions
        .iter()
        .filter_map(|s| s.cwd().map(PathBuf::from))
        .collect();
    match layout.scan() {
        Ok(worktrees) => candidates.extend(worktrees.iter().map(|wt| wt.path().to_path_buf())),
        Err(error) => warnings.push(Warning::Scan {
            root: layout.root().to_path_buf(),
            error,
        }),
    }
    candidates.sort();
    candidates.dedup();

    let mut roots = BTreeSet::new();
    for (path, result) in candidates
        .iter()
        .zip(parallel_map(&candidates, |p| vcs.repo_of(p)))
    {
        match result {
            Ok(Some(root)) => {
                roots.insert(root);
            }
            Ok(None) => {}
            Err(error) => warnings.push(Warning::Repo {
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
        Layout::new("/w".into()).unwrap()
    }

    fn root(path: &str) -> RepoRoot {
        RepoRoot::new(path.into()).unwrap()
    }

    fn name(s: &str) -> SessionName {
        s.parse().unwrap()
    }

    fn wt(path: &str, branch: Option<&str>) -> Worktree {
        Worktree {
            path: path.into(),
            head: branch.map_or(Head::Detached, |b| Head::Branch(b.parse().unwrap())),
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
            repo.entry(&name("api")).unwrap().presence(),
            Some(Presence::Exited)
        );
        assert_eq!(
            repo.entry(&name("fix")).unwrap().presence(),
            Some(Presence::Live)
        );
        let idle = repo.entry(&name("idle")).unwrap();
        assert_eq!(idle.presence(), None);
        let EntryState::Idle(at) = idle.state() else {
            panic!("idle should be startable")
        };
        assert_eq!(at.cwd(), std::path::Path::new("/w/api/idle"));
    }

    #[test]
    fn other_sessions_are_adopted_by_the_repo_holding_their_cwd() {
        let obs = Observation {
            sessions: vec![live("scratch", "/src/api/sub/dir")],
            repos: vec![repo("/src/api", &[])],
            warnings: Vec::new(),
        };
        let snap = snapshot(&obs, &[]);
        let entry = snap.repos()[0].entry(&name("scratch")).unwrap();
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
            snap.slot(crate::model::Slot::new(1).unwrap())
                .unwrap()
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
            snap.slot(crate::model::Slot::new(1).unwrap())
                .unwrap()
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
