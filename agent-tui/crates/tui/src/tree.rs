//! [3]'s rows: the changed files flat in numstat order, or as a directory
//! tree with folders first and single-child folder chains on one row.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use agent_core::ports::{Change, ChangeKind, FileState, Lines};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChangesView {
    #[default]
    Tree,
    Flat,
}

impl ChangesView {
    pub fn toggled(self) -> Self {
        match self {
            Self::Tree => Self::Flat,
            Self::Flat => Self::Tree,
        }
    }
}

/// Which files [3] lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChangesScope {
    /// Only what isn't committed, counted against the last commit.
    #[default]
    Uncommitted,
    /// Everything since the fork from the default branch.
    Since,
}

impl ChangesScope {
    pub fn toggled(self) -> Self {
        match self {
            Self::Uncommitted => Self::Since,
            Self::Since => Self::Uncommitted,
        }
    }

    pub fn diff_base(self) -> agent_core::ports::DiffBase {
        match self {
            Self::Uncommitted => agent_core::ports::DiffBase::Head,
            Self::Since => agent_core::ports::DiffBase::Fork,
        }
    }

    /// Whether this scope lists `path`.
    pub fn lists(self, facts: &agent_core::ports::WorktreeFacts, path: &std::path::Path) -> bool {
        self.of(facts).iter().any(|c| c.path == path)
    }

    /// The files of `facts` in this scope.
    pub fn of(self, facts: &agent_core::ports::WorktreeFacts) -> &[Change] {
        match self {
            Self::Uncommitted => &facts.pending,
            Self::Since => &facts.changed,
        }
    }
}

/// Lines changed under a row: the counted files' sums, and how many files
/// are new or binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub added: usize,
    pub removed: usize,
    pub new: usize,
    pub binary: usize,
    /// Files with something in the index, and with something only in the
    /// working tree (untracked ones included).
    pub staged: usize,
    pub unstaged: usize,
}

impl Counts {
    fn of(change: &Change) -> Self {
        let state = change.state();
        let pending = Self {
            staged: usize::from(state.staged().is_some()),
            unstaged: usize::from(
                state.unstaged().is_some()
                    || matches!(state, FileState::Untracked | FileState::Conflicted),
            ),
            ..Self::default()
        };
        pending.plus(Self::lines_of(change))
    }

    fn lines_of(change: &Change) -> Self {
        match change.kind {
            ChangeKind::Tracked {
                lines: Lines::Counted { added, removed },
                ..
            } => Self {
                added,
                removed,
                ..Self::default()
            },
            ChangeKind::Tracked {
                lines: Lines::Binary,
                ..
            } => Self {
                binary: 1,
                ..Self::default()
            },
            ChangeKind::Tracked {
                lines: Lines::Unknown,
                ..
            } => Self::default(),
            ChangeKind::Untracked => Self {
                new: 1,
                ..Self::default()
            },
        }
    }

    fn plus(self, other: Self) -> Self {
        Self {
            added: self.added + other.added,
            removed: self.removed + other.removed,
            new: self.new + other.new,
            binary: self.binary + other.binary,
            staged: self.staged + other.staged,
            unstaged: self.unstaged + other.unstaged,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    /// The change at this index of the agent's changed files.
    File(usize),
    Folder {
        open: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub node: Node,
    pub depth: usize,
    /// A file's name, or a folder's chain of names (`crates/tui/src`).
    pub label: String,
    /// The whole path from the checkout's root.
    pub path: PathBuf,
    /// A folder row's every real folder, outermost first and ending at
    /// `path`: a chain row stands for all of them. Empty for files.
    pub chain: Vec<PathBuf>,
    pub counts: Counts,
    /// A file's state against the last commit. `Committed` for folders,
    /// whose `counts` say what's pending under them.
    pub state: FileState,
}

#[derive(Default)]
struct Dir {
    dirs: BTreeMap<String, Dir>,
    files: Vec<(String, usize)>,
}

impl Dir {
    fn counts(&self, changes: &[Change]) -> Counts {
        let own = self.files.iter().fold(Counts::default(), |c, (_, i)| {
            c.plus(Counts::of(&changes[*i]))
        });
        self.dirs
            .values()
            .fold(own, |c, d| c.plus(d.counts(changes)))
    }
}

fn emit(
    dir: &Dir,
    base: &Path,
    depth: usize,
    changes: &[Change],
    collapsed: &HashSet<PathBuf>,
    out: &mut Vec<TreeRow>,
) {
    for (name, child) in &dir.dirs {
        let mut label = name.clone();
        let mut path = base.join(name);
        let mut chain = vec![path.clone()];
        let mut child = child;
        while child.files.is_empty() && child.dirs.len() == 1 {
            let Some((next, only)) = child.dirs.iter().next() else {
                break;
            };
            label = format!("{label}/{next}");
            path = path.join(next);
            chain.push(path.clone());
            child = only;
        }
        let open = !chain.iter().any(|p| collapsed.contains(p));
        out.push(TreeRow {
            node: Node::Folder { open },
            depth,
            label,
            path: path.clone(),
            chain,
            counts: child.counts(changes),
            state: FileState::Committed,
        });
        if open {
            emit(child, &path, depth + 1, changes, collapsed, out);
        }
    }
    let mut files: Vec<&(String, usize)> = dir.files.iter().collect();
    files.sort_by_key(|(name, _)| name.to_lowercase());
    for (name, i) in files {
        out.push(TreeRow {
            node: Node::File(*i),
            depth,
            label: name.clone(),
            path: changes[*i].path.clone(),
            chain: Vec::new(),
            counts: Counts::of(&changes[*i]),
            state: changes[*i].state(),
        });
    }
}

/// The rows [3] lists. `collapsed` holds the paths of the real folders that
/// are closed, however the rows chain them. A row is closed when any folder
/// of its chain is, and a folder it doesn't name is open, so one that
/// appears starts open.
pub fn build(changes: &[Change], view: ChangesView, collapsed: &HashSet<PathBuf>) -> Vec<TreeRow> {
    let mut out = Vec::new();
    match view {
        ChangesView::Flat => {
            for (i, change) in changes.iter().enumerate() {
                out.push(TreeRow {
                    node: Node::File(i),
                    depth: 0,
                    label: change.path.to_string_lossy().into_owned(),
                    path: change.path.clone(),
                    chain: Vec::new(),
                    counts: Counts::of(change),
                    state: change.state(),
                });
            }
        }
        ChangesView::Tree => {
            let mut root = Dir::default();
            for (i, change) in changes.iter().enumerate() {
                let parts: Vec<String> = change
                    .path
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                let Some((file, folders)) = parts.split_last() else {
                    continue;
                };
                let mut dir = &mut root;
                for folder in folders {
                    dir = dir.dirs.entry(folder.clone()).or_default();
                }
                dir.files.push((file.clone(), i));
            }
            emit(&root, Path::new(""), 0, changes, collapsed, &mut out);
        }
    }
    out
}

/// Every real folder the changes sit under, intermediate ones of a chain
/// included.
pub fn folders(changes: &[Change]) -> HashSet<PathBuf> {
    changes
        .iter()
        .flat_map(|c| {
            let mut ancestors: Vec<&Path> = c.path.ancestors().skip(1).collect();
            ancestors.retain(|p| !p.as_os_str().is_empty());
            ancestors.into_iter().map(Path::to_path_buf)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::ports::TrackedState;

    fn change(path: &str, added: usize, removed: usize) -> Change {
        Change::tracked(
            path,
            Lines::Counted { added, removed },
            TrackedState::Committed,
        )
    }

    fn sample() -> Vec<Change> {
        vec![
            change("README.md", 1, 0),
            change("crates/tui/src/update.rs", 12, 3),
            change("crates/tui/src/view/sidebar.rs", 4, 1),
            change("crates/tui/src/lib.rs", 2, 2),
            Change::untracked("nvim/lsp/new.lua"),
            change("nvim/lua/a.lua", 5, 0),
        ]
    }

    fn labels(rows: &[TreeRow]) -> Vec<(usize, &str)> {
        rows.iter().map(|r| (r.depth, r.label.as_str())).collect()
    }

    #[test]
    fn single_child_chains_share_a_row_and_folders_sort_before_files() {
        let rows = build(&sample(), ChangesView::Tree, &HashSet::new());
        assert_eq!(
            labels(&rows),
            [
                (0, "crates/tui/src"),
                (1, "view"),
                (2, "sidebar.rs"),
                (1, "lib.rs"),
                (1, "update.rs"),
                (0, "nvim"),
                (1, "lsp"),
                (2, "new.lua"),
                (1, "lua"),
                (2, "a.lua"),
                (0, "README.md"),
            ]
        );
    }

    #[test]
    fn folders_sum_what_is_under_them() {
        let rows = build(&sample(), ChangesView::Tree, &HashSet::new());
        let src = &rows[0];
        assert_eq!(src.path, PathBuf::from("crates/tui/src"));
        assert_eq!((src.counts.added, src.counts.removed), (18, 6));
        let nvim = rows.iter().find(|r| r.label == "nvim").expect("nvim");
        assert_eq!((nvim.counts.added, nvim.counts.new), (5, 1));
    }

    #[test]
    fn a_closed_folder_hides_its_rows_and_a_new_folder_starts_open() {
        let closed: HashSet<PathBuf> = ["crates/tui/src".into()].into();
        let rows = build(&sample(), ChangesView::Tree, &closed);
        assert_eq!(rows[0].node, Node::Folder { open: false });
        assert_eq!(rows[1].label, "nvim");
        let mut more = sample();
        more.push(change("docs/design.md", 1, 1));
        let rows = build(&more, ChangesView::Tree, &closed);
        assert!(
            rows.iter()
                .any(|r| r.label == "docs" && r.node == Node::Folder { open: true })
        );
    }

    #[test]
    fn flat_keeps_numstat_order_and_full_paths() {
        let rows = build(&sample(), ChangesView::Flat, &HashSet::new());
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[1].label, "crates/tui/src/update.rs");
        assert!(rows.iter().all(|r| matches!(r.node, Node::File(_))));
    }

    #[test]
    fn every_folder_is_found_for_collapse_all() {
        let all = folders(&sample());
        for expected in [
            "crates/tui/src",
            "crates/tui/src/view",
            "nvim",
            "nvim/lsp",
            "nvim/lua",
        ] {
            assert!(all.contains(Path::new(expected)), "{expected}");
        }
        let rows = build(&sample(), ChangesView::Tree, &all);
        assert_eq!(
            labels(&rows),
            [(0, "crates/tui/src"), (0, "nvim"), (0, "README.md")]
        );
    }
}
