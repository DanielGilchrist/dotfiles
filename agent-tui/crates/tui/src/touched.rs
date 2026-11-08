//! The files an activity touched: what its edits and writes aimed at, joined with
//! the files whose git state moved since the activity began, which also catches
//! what a shell command changed.

use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use agent_core::ports::{Change, Log, LogEntry, WorktreeFacts};

/// Where an agent's checkout is, so a path a harness names (absolute, maybe
/// through a symlink or with `..`) can be told from one git names (relative
/// to the checkout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    root: PathBuf,
    /// The checkout with symlinks resolved, read when its facts were.
    real: Option<PathBuf>,
}

impl Roots {
    pub fn new(root: impl Into<PathBuf>, real: Option<PathBuf>) -> Self {
        Self {
            root: root.into(),
            real,
        }
    }

    /// `path` below the checkout, when it is.
    pub fn strip(&self, path: &Path) -> Option<PathBuf> {
        let tidy = lexical(path);
        [Some(&self.root), self.real.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|base| tidy.strip_prefix(base).ok())
            .map(Path::to_path_buf)
    }

    /// `path` below the checkout, or as it is with `.` and `..` resolved
    /// when it is somewhere else.
    pub fn within(&self, path: &Path) -> PathBuf {
        self.strip(path).unwrap_or_else(|| lexical(path))
    }
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir
                if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
            {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The changed files of an agent when its activity began.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Baseline {
    /// The TUI wasn't watching when the activity began, or git hadn't been read
    /// since the agent last reported, so only the log can say what was
    /// touched.
    #[default]
    Unknown,
    Taken {
        pending: HashMap<PathBuf, Change>,
        changed: HashMap<PathBuf, Change>,
    },
}

fn by_path(changes: &[Change]) -> HashMap<PathBuf, Change> {
    changes
        .iter()
        .map(|c| (c.path.clone(), c.clone()))
        .collect()
}

impl Baseline {
    pub fn of(facts: Option<&WorktreeFacts>) -> Self {
        match facts {
            Some(facts) => Self::Taken {
                pending: by_path(&facts.pending),
                changed: by_path(&facts.changed),
            },
            None => Self::Unknown,
        }
    }

    /// The paths whose entry in `facts` is new or different from the one
    /// the activity began with.
    fn moved<'a>(&self, facts: &'a WorktreeFacts) -> Vec<&'a Path> {
        let Self::Taken { pending, changed } = self else {
            return Vec::new();
        };
        let new = |now: &'a [Change], then: &HashMap<PathBuf, Change>| {
            now.iter()
                .filter(|c| then.get(&c.path) != Some(*c))
                .map(|c| c.path.as_path())
                .collect::<Vec<_>>()
        };
        let mut paths = new(&facts.pending, pending);
        paths.extend(new(&facts.changed, changed));
        paths
    }
}

/// A file the activity touched, with where git has it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedFile {
    /// Relative to the checkout, or as the harness named it when it is
    /// outside.
    pub path: PathBuf,
    pub now: Now,
}

/// Where git has a touched file now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Now {
    /// Its uncommitted state, else its state since the fork.
    Changed(Change),
    /// Git sees no difference.
    Clean,
    /// Git hasn't been read.
    Unknown,
}

/// What git has for each path now, uncommitted before since the fork.
struct Current<'a> {
    pending: HashMap<&'a Path, &'a Change>,
    changed: HashMap<&'a Path, &'a Change>,
}

impl<'a> Current<'a> {
    fn of(facts: &'a WorktreeFacts) -> Self {
        let index = |list: &'a [Change]| list.iter().map(|c| (c.path.as_path(), c)).collect();
        Self {
            pending: index(&facts.pending),
            changed: index(&facts.changed),
        }
    }

    fn get(&self, path: &Path) -> Now {
        match self.pending.get(path).or_else(|| self.changed.get(path)) {
            Some(change) => Now::Changed((*change).clone()),
            None => Now::Clean,
        }
    }
}

/// Whether a path from [`Roots::within`] is below the checkout, which is
/// the only place git can see.
fn within_checkout(path: &Path) -> bool {
    path.is_relative() && !path.starts_with("..")
}

pub fn files(
    log: &Log,
    roots: &Roots,
    facts: Option<&WorktreeFacts>,
    baseline: &Baseline,
) -> Vec<TouchedFile> {
    let mut paths: BTreeSet<PathBuf> = log
        .log
        .iter()
        .filter_map(|entry| match entry {
            LogEntry::Called(call) => call.touched().map(|path| roots.within(Path::new(path))),
            LogEntry::Said { .. } | LogEntry::Queued { .. } | LogEntry::Compacted => None,
        })
        .collect();
    let current = facts.map(Current::of);
    if let Some(facts) = facts {
        paths.extend(baseline.moved(facts).into_iter().map(Path::to_path_buf));
    }
    paths
        .into_iter()
        .map(|path| TouchedFile {
            now: match &current {
                Some(current) if within_checkout(&path) => current.get(&path),
                Some(_) | None => Now::Unknown,
            },
            path,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{facts, modified};
    use crate::log::fixtures::{detail, edit};

    fn paths(files: &[TouchedFile]) -> Vec<&str> {
        files.iter().filter_map(|f| f.path.to_str()).collect()
    }

    fn roots() -> Roots {
        Roots::new("/w", None)
    }

    #[test]
    fn edits_inside_the_checkout_are_listed_relative_once_and_in_path_order() {
        let log = detail(vec![
            edit("/w/z.rs"),
            edit("/w/src/a.rs"),
            edit("/w/z.rs"),
            edit("/elsewhere/n.txt"),
        ]);
        let files = files(&log, &roots(), None, &Baseline::Unknown);
        assert_eq!(paths(&files), ["/elsewhere/n.txt", "src/a.rs", "z.rs"]);
    }

    #[test]
    fn a_file_git_changed_during_the_activity_counts_though_no_edit_call_named_it() {
        let began = Baseline::of(Some(&facts(vec![modified("old.rs")], Vec::new())));
        let now = facts(vec![modified("old.rs"), modified("fmt.rs")], Vec::new());
        let files = files(&detail(Vec::new()), &roots(), Some(&now), &began);
        assert_eq!(
            paths(&files),
            ["fmt.rs"],
            "old.rs was already dirty and is unchanged"
        );
        assert_eq!(files[0].now, Now::Changed(modified("fmt.rs")));
    }

    #[test]
    fn a_file_dirty_before_the_activity_counts_once_its_numbers_move() {
        let began = Baseline::of(Some(&facts(vec![modified("old.rs")], Vec::new())));
        let more = agent_core::ports::Change::tracked(
            "old.rs",
            agent_core::ports::Lines::Counted {
                added: 9,
                removed: 0,
            },
            agent_core::ports::TrackedState::Committed,
        );
        let now = facts(vec![more], Vec::new());
        let files = files(&detail(Vec::new()), &roots(), Some(&now), &began);
        assert_eq!(paths(&files), ["old.rs"]);
    }

    #[test]
    fn without_a_baseline_only_the_log_speaks_and_git_still_gives_the_current_state() {
        let now = facts(vec![modified("a.rs"), modified("b.rs")], Vec::new());
        let log = detail(vec![edit("/w/a.rs")]);
        let files = files(&log, &roots(), Some(&now), &Baseline::Unknown);
        assert_eq!(paths(&files), ["a.rs"]);
        assert_eq!(files[0].now, Now::Changed(modified("a.rs")));
    }

    #[test]
    fn an_edited_file_git_no_longer_differs_in_is_kept_without_a_change() {
        let log = detail(vec![edit("/w/gone.rs")]);
        let now = facts(Vec::new(), Vec::new());
        let files = files(&log, &roots(), Some(&now), &Baseline::Unknown);
        assert_eq!(paths(&files), ["gone.rs"]);
        assert_eq!(files[0].now, Now::Clean);
    }

    #[test]
    fn a_file_outside_the_checkout_is_unknown_to_git_not_clean() {
        let log = detail(vec![edit("/elsewhere/n.txt"), edit("/w/../n.txt")]);
        let now = facts(Vec::new(), Vec::new());
        let files = files(&log, &roots(), Some(&now), &Baseline::Unknown);
        assert_eq!(paths(&files), ["/elsewhere/n.txt", "/n.txt"]);
        assert!(files.iter().all(|f| f.now == Now::Unknown));
    }

    #[test]
    fn a_file_touched_before_git_was_read_is_unknown_not_clean() {
        let files = files(
            &detail(vec![edit("/w/a.rs")]),
            &roots(),
            None,
            &Baseline::Unknown,
        );
        assert_eq!(files[0].now, Now::Unknown);
    }

    #[test]
    fn a_baseline_needs_facts() {
        assert_eq!(Baseline::of(None), Baseline::Unknown);
    }

    #[test]
    fn an_absolute_path_through_a_symlinked_checkout_is_one_file_not_two() {
        let roots = Roots::new("/link/w", Some("/real/w".into()));
        let log = detail(vec![
            edit("/real/w/src/a.rs"),
            edit("/link/w/src/../src/a.rs"),
        ]);
        let now = facts(vec![modified("src/a.rs")], Vec::new());
        let files = files(&log, &roots, Some(&now), &Baseline::Unknown);
        assert_eq!(paths(&files), ["src/a.rs"]);
    }

    #[test]
    fn dots_in_a_path_are_resolved_and_a_path_outside_stays_outside() {
        let roots = roots();
        assert_eq!(roots.within(Path::new("/w/./a/../b.rs")), Path::new("b.rs"));
        assert_eq!(
            roots.within(Path::new("/x/../y/z.rs")),
            Path::new("/y/z.rs")
        );
        assert_eq!(roots.within(Path::new("a/b.rs")), Path::new("a/b.rs"));
        assert_eq!(roots.strip(Path::new("/y/z.rs")), None);
    }
}
