use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;

use agent_core::model::{Head, Worktree};
use agent_core::names::{BranchName, NameError, PullRequest};
use agent_core::paths::{AgentWorktree, RepoRoot};
use agent_core::ports::{
    Added, BranchLog, BranchState, Branches, Change, ChangeKind, Checkout, Compared, DiffBase,
    Edit, Fetch, FileDiff, FileState, Forge, Lines, LocalBranch, PullRequestHead, RemoteBranch,
    RemoteRef, RepoWorktrees, TrackedState, Uncommitted, Vcs, WorktreeFacts, WorktreeStart,
};
use agent_core::process::{self, Capped, CommandError};

/// A diff over this many lines keeps only the first of them.
const DIFF_LINES: usize = 5000;
/// How much of a diff's text is read, and how much of one line is kept.
const DIFF_BYTES: u64 = 2 * 1024 * 1024;
const DIFF_LINE_CHARS: usize = 2000;
/// The most bytes one facts read spends counting untracked files. A file
/// that doesn't fit what is left is listed unread.
const UNTRACKED_BUDGET: u64 = 8 * 1024 * 1024;
/// How much of a file git looks at to call it binary.
const BINARY_PROBE: usize = 8000;
/// A diff that takes longer than this to produce is given up on.
const DIFF_TIMEOUT: Duration = Duration::from_secs(20);

/// A checkout of a large repo can take minutes.
const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(600);

/// [`Vcs`] backed by the `git` CLI.
#[derive(Debug, Default, Clone, Copy)]
pub struct Git;

/// GitHub through the `gh` CLI.
pub struct GitHub;

impl Forge for GitHub {
    fn pull_request_head(&self, pr: &PullRequest) -> Result<PullRequestHead, CommandError> {
        let mut cmd = process::quiet("gh");
        cmd.args(["pr", "view", &pr.number.to_string(), "--repo", &pr.slug()])
            .args(["--json", "headRefName,isCrossRepository"])
            .args(["--jq", "[.headRefName, .isCrossRepository] | @tsv"]);
        let out = process::run(&mut cmd)?;
        parse_pull_request_head(out.trim()).map_err(|detail| CommandError::Parse {
            command: "gh pr view".into(),
            detail,
        })
    }
}

/// `<branch>\t<true|false>`, as `gh --jq @tsv` prints it.
fn parse_pull_request_head(line: &str) -> Result<PullRequestHead, String> {
    let (branch, cross) = line
        .split_once('\t')
        .ok_or_else(|| format!("expected '<branch>\t<bool>', got {line:?}"))?;
    let cross_repository = match cross {
        "true" => true,
        "false" => false,
        other => return Err(format!("expected true or false, got {other:?}")),
    };
    let branch = branch.parse().map_err(|e: NameError| e.to_string())?;
    Ok(PullRequestHead {
        branch,
        cross_repository,
    })
}

/// Whether the user has their own ssh command for git. If not, ours runs ssh
/// in batch mode so a passphrase prompt can't draw over the TUI.
fn custom_ssh() -> bool {
    static CUSTOM: OnceLock<bool> = OnceLock::new();
    *CUSTOM.get_or_init(|| {
        std::env::var_os("GIT_SSH_COMMAND").is_some()
            || std::env::var_os("GIT_SSH").is_some()
            || process::output(process::quiet("git").args(["config", "--get", "core.sshCommand"]))
                .is_ok_and(|out| out.status.success())
    })
}

fn git(dir: &Path) -> Command {
    let mut cmd = process::quiet("git");
    // Output is parsed, and the lock reason a worktree add writes is
    // translated, so git runs in the C locale.
    cmd.env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("-C")
        .arg(dir)
        .arg("--no-optional-locks");
    if !custom_ssh() {
        cmd.env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes");
    }
    cmd
}

/// Bytes left to read untracked files with.
struct Budget(u64);

/// An untracked file as `git diff --numstat` would count it if it listed
/// the file: all lines added, binary with a NUL in the first `BINARY_PROBE`
/// bytes. A file that isn't a plain one, or doesn't fit `budget`, is
/// unread and spends nothing.
fn read_added(path: &Path, budget: &mut Budget) -> Added {
    use std::io::Read;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Added::Unread;
    };
    if !meta.is_file() || meta.len() > budget.0 {
        return Added::Unread;
    }
    let mut bytes = Vec::new();
    let read =
        std::fs::File::open(path).and_then(|file| file.take(budget.0).read_to_end(&mut bytes));
    if read.is_err() {
        return Added::Unread;
    }
    budget.0 -= bytes.len() as u64;
    added_of(&bytes, meta.len())
}

/// `bytes` is what was read of a file that was `expected` bytes long. A file
/// that changed size in between would be counted partly, so it is unread.
fn added_of(bytes: &[u8], expected: u64) -> Added {
    if bytes.len() as u64 != expected {
        return Added::Unread;
    }
    if bytes[..bytes.len().min(BINARY_PROBE)].contains(&0) {
        return Added::Binary;
    }
    Added::Counted(bytes.split_inclusive(|&b| b == b'\n').count())
}

/// What `git status` lists, the untracked files already read.
struct Status {
    tracked: Vec<(PathBuf, TrackedState)>,
    untracked: Vec<Change>,
}

impl Status {
    fn read(dir: &Path, files: Vec<Uncommitted>) -> Self {
        let mut budget = Budget(UNTRACKED_BUDGET);
        let mut status = Self {
            tracked: Vec::new(),
            untracked: Vec::new(),
        };
        for file in files {
            if let Some(state) = file.state.tracked() {
                status.tracked.push((file.path, state));
            } else {
                let added = read_added(&dir.join(&file.path), &mut budget);
                status.untracked.push(Change::untracked(file.path, added));
            }
        }
        status
    }
}

/// `git diff --numstat -z` output: `added\tremoved\tpath\0` per file, with
/// `-` counts for binary files.
fn parse_numstat(out: &str) -> Vec<Change> {
    out.split('\0')
        .filter_map(|entry| {
            let mut fields = entry.splitn(3, '\t');
            let added = fields.next()?;
            let removed = fields.next()?;
            let path = fields.next().filter(|p| !p.is_empty())?;
            let lines = match (added.parse(), removed.parse()) {
                (Ok(added), Ok(removed)) => Lines::Counted { added, removed },
                _ => Lines::Binary,
            };
            Some(Change::tracked(path, lines, TrackedState::Committed))
        })
        .collect()
}

/// `git status --porcelain=v1 -z --no-renames` output: `XY path\0` per
/// file.
fn parse_status(out: &str) -> Vec<Uncommitted> {
    out.split('\0')
        .filter_map(|entry| {
            let mut chars = entry.chars();
            let (index, tree, space) = (chars.next()?, chars.next()?, chars.next()?);
            let path = chars.as_str();
            let state = parse_state(index, tree);
            (space == ' ' && !path.is_empty() && !state.is_committed()).then(|| Uncommitted {
                path: PathBuf::from(path),
                state,
            })
        })
        .collect()
}

fn edit_of(code: char) -> Option<Edit> {
    match code {
        'A' => Some(Edit::Added),
        'D' => Some(Edit::Deleted),
        'M' | 'T' | 'R' | 'C' => Some(Edit::Modified),
        _ => None,
    }
}

/// A porcelain `XY` pair: `X` is the index, `Y` the working tree.
fn parse_state(index: char, tree: char) -> FileState {
    if (index, tree) == ('?', '?') {
        return FileState::Untracked;
    }
    let unmerged = matches!((index, tree), ('A', 'A') | ('D', 'D')) || index == 'U' || tree == 'U';
    if unmerged {
        return FileState::Conflicted;
    }
    match (edit_of(index), edit_of(tree)) {
        (Some(staged), Some(unstaged)) => FileState::Both { staged, unstaged },
        (Some(edit), None) => FileState::Staged(edit),
        (None, Some(edit)) => FileState::Unstaged(edit),
        (None, None) => FileState::Committed,
    }
}

fn parsed<T: FromStr<Err = NameError>>(cmd: &Command, raw: &str) -> Result<T, CommandError> {
    raw.parse()
        .map_err(|e: NameError| parse_error(cmd, e.to_string()))
}

fn parse_error(cmd: &Command, detail: impl Into<String>) -> CommandError {
    CommandError::Parse {
        command: process::describe(cmd),
        detail: detail.into(),
    }
}

/// Runs a query whose exit code 1 means "no" (`rev-parse --verify --quiet`).
fn query(cmd: &mut Command) -> Result<bool, CommandError> {
    let out = process::output(cmd)?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(process::failed(cmd, &out)),
    }
}

fn parse_branches(refs: &str) -> Branches {
    let mut branches = Branches::default();
    for line in refs.lines() {
        if let Some(name) = line.strip_prefix("refs/heads/") {
            branches.local.extend(name.parse());
        } else if let Some(name) = line.strip_prefix("refs/remotes/origin/")
            && name != "HEAD"
        {
            branches.on_origin.extend(name.parse());
        }
    }
    branches
}

fn parse_repo_worktrees(porcelain: &str) -> Result<RepoWorktrees, String> {
    let mut worktrees = parse_worktrees(porcelain)?.into_iter();
    let main = worktrees.next().ok_or("no worktrees listed")?;
    Ok(RepoWorktrees {
        main,
        linked: worktrees.collect(),
    })
}

fn parse_worktrees(porcelain: &str) -> Result<Vec<Worktree>, String> {
    porcelain
        .split("\n\n")
        .filter(|block| !block.trim().is_empty())
        // `locked initializing` is what `git worktree add` holds while it
        // checks the files out (builtin/worktree.c, add_worktree), so a
        // worktree listed that way isn't finished.
        .filter(|block| {
            !block
                .lines()
                .any(|l| l == "bare" || l.starts_with("prunable") || l == "locked initializing")
        })
        .map(|block| {
            let mut path = None;
            let mut head = Head::Detached;
            for line in block.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    path = Some(PathBuf::from(p));
                } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                    head = Head::Branch(b.parse().map_err(|e| format!("{e}"))?);
                }
            }
            let path = path.ok_or_else(|| format!("worktree block without a path: {block:?}"))?;
            Ok(Worktree { path, head })
        })
        .collect()
}

impl Git {
    fn config(repo: &RepoRoot, key: &str) -> Result<Option<String>, CommandError> {
        let mut cmd = git(repo.path());
        cmd.args(["config", "--get", key]);
        let out = process::output(&mut cmd)?;
        match out.status.code() {
            Some(0) => Ok(Some(
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            )),
            Some(1) => Ok(None),
            _ => Err(process::failed(&cmd, &out)),
        }
    }

    /// Lines changed per file since `since`, committed or not.
    fn numstat(dir: &Path, since: &str) -> Result<Vec<Change>, CommandError> {
        let mut cmd = git(dir);
        cmd.args(["diff", "--numstat", "--no-renames", "-z", since]);
        let out = process::run(&mut cmd)?;
        Ok(parse_numstat(&out))
    }

    /// Every modified, staged and untracked file.
    fn status(dir: &Path) -> Result<Vec<Uncommitted>, CommandError> {
        let mut cmd = git(dir);
        cmd.args([
            "status",
            "--porcelain=v1",
            "-z",
            "--no-renames",
            "--untracked-files=all",
        ]);
        Ok(parse_status(&process::run(&mut cmd)?))
    }

    fn count(dir: &Path, range: &[&str]) -> Result<usize, CommandError> {
        let mut cmd = git(dir);
        cmd.args(["rev-list", "--count"]).args(range);
        let count = process::run(&mut cmd)?;
        count
            .trim()
            .parse()
            .map_err(|_| parse_error(&cmd, count.trim()))
    }

    /// For a branch with commits ahead of the base: did it track its own
    /// branch on a remote that has since been deleted (merged with
    /// auto-delete)? Safe only if nothing was committed after the last push,
    /// which needs the remote-tracking ref to still be present locally.
    fn upstream_state(
        repo: &RepoRoot,
        branch: &BranchName,
        ahead: NonZeroUsize,
    ) -> Result<BranchState, CommandError> {
        let unmerged = BranchState::Unmerged { ahead };
        let (Some(remote), Some(merge)) = (
            Self::config(repo, &format!("branch.{branch}.remote"))?,
            Self::config(repo, &format!("branch.{branch}.merge"))?,
        ) else {
            return Ok(unmerged);
        };
        if merge != format!("refs/heads/{branch}") || remote == "." {
            return Ok(unmerged);
        }
        let mut ls = git(repo.path());
        ls.args([
            "ls-remote",
            "--exit-code",
            "--heads",
            &remote,
            branch.as_str(),
        ]);
        let out = process::output(&mut ls)?;
        match out.status.code() {
            Some(0) => return Ok(unmerged),
            Some(2) => {}
            _ => return Err(process::failed(&ls, &out)),
        }
        let tracking = format!("refs/remotes/{remote}/{branch}");
        let mut verify = git(repo.path());
        verify.args(["rev-parse", "--verify", "--quiet", &tracking]);
        if query(&mut verify)?
            && Self::count(repo.path(), &[&format!("{tracking}..{branch}")])? == 0
        {
            return Ok(BranchState::Safe);
        }
        Ok(BranchState::UpstreamGone { ahead })
    }

    fn checkout(dir: &Path) -> Result<Checkout, CommandError> {
        let mut cmd = git(dir);
        cmd.args(["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let out = process::output(&mut cmd)?;
        if out.status.success() {
            let name = String::from_utf8_lossy(&out.stdout);
            return parsed(&cmd, name.trim()).map(Checkout::Branch);
        }
        let mut cmd = git(dir);
        cmd.args(["rev-parse", "--short", "HEAD"]);
        Ok(Checkout::Detached(
            process::run(&mut cmd)?.trim().to_string(),
        ))
    }

    /// Origin's default branch, as seen from `dir` (any of the repo's
    /// worktrees: refs are shared).
    fn default_base(dir: &Path) -> Result<Option<RemoteRef>, CommandError> {
        let mut cmd = git(dir);
        cmd.args([
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ]);
        let out = process::output(&mut cmd)?;
        if out.status.success() {
            let name = String::from_utf8_lossy(&out.stdout);
            return parsed(&cmd, name.trim()).map(Some);
        }
        for candidate in ["origin/main", "origin/master"] {
            let mut cmd = git(dir);
            cmd.args(["rev-parse", "--verify", "--quiet", candidate]);
            if query(&mut cmd)? {
                return parsed(&cmd, candidate).map(Some);
            }
        }
        Ok(None)
    }

    /// `changes` with the state `status` gives each file, and the untracked
    /// files last.
    fn overlay(changes: Vec<Change>, status: &Status) -> Vec<Change> {
        let states: HashMap<&Path, TrackedState> = status
            .tracked
            .iter()
            .map(|(path, state)| (path.as_path(), *state))
            .collect();
        changes
            .into_iter()
            .map(|mut change| {
                if let (ChangeKind::Tracked { state, .. }, Some(found)) =
                    (&mut change.kind, states.get(change.path.as_path()))
                {
                    *state = *found;
                }
                change
            })
            .chain(status.untracked.iter().cloned())
            .collect()
    }

    /// Every file `status` lists, counted against the last commit where a
    /// diff has counts for it. One with no diff left, such as a staged edit
    /// reverted in the working tree, is still uncommitted work.
    fn pending(dir: &Path, status: &Status) -> Result<Vec<Change>, CommandError> {
        let mut changes = Self::overlay(Self::numstat(dir, "HEAD")?, status);
        let counted: HashSet<PathBuf> = changes.iter().map(|c| c.path.clone()).collect();
        changes.extend(
            status
                .tracked
                .iter()
                .filter(|(path, _)| !counted.contains(path))
                .map(|(path, state)| Change::tracked(path.clone(), Lines::Unknown, *state)),
        );
        Ok(changes)
    }

    /// Where HEAD forked from `base`.
    fn fork_point(dir: &Path, base: &RemoteRef) -> Result<String, CommandError> {
        let mut cmd = git(dir);
        cmd.args(["merge-base", "HEAD", base.as_str()]);
        Ok(process::run(&mut cmd)?.trim().to_string())
    }

    /// The files changed since the fork point from `base`, committed or not.
    fn fork_changes(
        dir: &Path,
        base: &RemoteRef,
        status: &Status,
    ) -> Result<Vec<Change>, CommandError> {
        let fork = Self::fork_point(dir, base)?;
        Ok(Self::overlay(Self::numstat(dir, &fork)?, status))
    }

    /// How many commits `base` is `(behind, ahead)` of HEAD's.
    fn compare(dir: &Path, base: &RemoteRef) -> Result<(usize, usize), CommandError> {
        let mut cmd = git(dir);
        cmd.args([
            "rev-list",
            "--left-right",
            "--count",
            &format!("{base}...HEAD"),
        ]);
        let counts = process::run(&mut cmd)?;
        counts
            .split_once('\t')
            .and_then(|(behind, ahead)| {
                Some((behind.trim().parse().ok()?, ahead.trim().parse().ok()?))
            })
            .ok_or_else(|| parse_error(&cmd, format!("expected two counts, got {counts:?}")))
    }
}

impl Vcs for Git {
    fn repo_of(&self, dir: &Path) -> Result<Option<RepoRoot>, CommandError> {
        if !dir.is_dir() {
            return Ok(None);
        }
        let mut inside = git(dir);
        inside.args(["rev-parse", "--is-inside-work-tree"]);
        let out = process::output(&mut inside)?;
        match out.status.code() {
            Some(0) => {}
            Some(128) => return Ok(None),
            _ => return Err(process::failed(&inside, &out)),
        }
        let mut cmd = git(dir);
        cmd.args(["worktree", "list", "--porcelain"]);
        let stdout = process::run(&mut cmd)?;
        let worktrees = parse_repo_worktrees(&stdout).map_err(|e| parse_error(&cmd, e))?;
        RepoRoot::new(worktrees.main.path)
            .map(Some)
            .map_err(|e| parse_error(&cmd, e.to_string()))
    }

    fn worktrees(&self, repo: &RepoRoot) -> Result<RepoWorktrees, CommandError> {
        let mut cmd = git(repo.path());
        cmd.args(["worktree", "list", "--porcelain"]);
        let stdout = process::run(&mut cmd)?;
        parse_repo_worktrees(&stdout).map_err(|e| parse_error(&cmd, e))
    }

    fn head(&self, dir: &Path) -> Result<Head, CommandError> {
        let mut cmd = git(dir);
        cmd.args(["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let out = process::output(&mut cmd)?;
        match out.status.code() {
            Some(0) => {
                let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
                name.parse()
                    .map(Head::Branch)
                    .map_err(|e: agent_core::names::NameError| parse_error(&cmd, e.to_string()))
            }
            Some(1) => Ok(Head::Detached),
            _ => Err(process::failed(&cmd, &out)),
        }
    }

    fn add_worktree(
        &self,
        repo: &RepoRoot,
        at: &AgentWorktree,
        start: &WorktreeStart,
    ) -> Result<(), CommandError> {
        let mut cmd = git(repo.path());
        cmd.args([
            "-c",
            "checkout.workers=8",
            "-c",
            "checkout.thresholdForParallelism=100",
            "worktree",
            "add",
        ]);
        match start {
            WorktreeStart::DefaultBase => {
                cmd.arg("--detach").arg(at.path());
                if let Some(base) = Self::default_base(repo.path())? {
                    cmd.arg(base.as_str());
                }
            }
            WorktreeStart::Branch(local) => {
                cmd.arg(at.path()).arg(local.name().as_str());
            }
            WorktreeStart::TrackRemote(remote) => {
                let branch = remote.name();
                cmd.args(["-b", branch.as_str()])
                    .arg(at.path())
                    .arg(format!("origin/{branch}"));
            }
            WorktreeStart::AtRemote(remote) => {
                let branch = remote.name();
                cmd.args(["--no-track", "-B", branch.as_str()])
                    .arg(at.path())
                    .arg(format!("origin/{branch}"));
            }
        }
        let out = process::output_within(&mut cmd, CHECKOUT_TIMEOUT)?;
        if out.status.success() {
            Ok(())
        } else {
            Err(process::failed(&cmd, &out))
        }
    }

    fn prune_worktrees(&self, repo: &RepoRoot) -> Result<(), CommandError> {
        process::run(git(repo.path()).args(["worktree", "prune"])).map(drop)
    }

    fn branches(&self, repo: &RepoRoot) -> Result<Branches, CommandError> {
        let mut cmd = git(repo.path());
        cmd.args([
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes/origin",
        ]);
        Ok(parse_branches(&process::run(&mut cmd)?))
    }

    fn local_branch(
        &self,
        repo: &RepoRoot,
        branch: &BranchName,
    ) -> Result<Option<LocalBranch>, CommandError> {
        let mut cmd = git(repo.path());
        cmd.args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/heads/{branch}"));
        Ok(query(&mut cmd)?.then(|| LocalBranch::found(branch.clone())))
    }

    fn remote_branch(
        &self,
        repo: &RepoRoot,
        branch: &BranchName,
    ) -> Result<Option<RemoteBranch>, CommandError> {
        let mut cmd = git(repo.path());
        cmd.args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/remotes/origin/{branch}"));
        Ok(query(&mut cmd)?.then(|| RemoteBranch::found(branch.clone())))
    }

    fn fetch(&self, repo: &RepoRoot, what: &Fetch) -> Result<(), CommandError> {
        let mut cmd = git(repo.path());
        let into = what.branch();
        let from = match what {
            Fetch::Branch(branch) => format!("refs/heads/{branch}"),
            Fetch::PullRequest(number) => format!("refs/pull/{number}/head"),
        };
        // An explicit refspec lands the tracking branch even in a clone
        // whose configured refspec doesn't cover it (`--single-branch`).
        cmd.args(["fetch", "-q", "origin"])
            .arg(format!("+{from}:refs/remotes/origin/{into}"));
        let out = process::output_within(&mut cmd, CHECKOUT_TIMEOUT)?;
        if out.status.success()
            || String::from_utf8_lossy(&out.stderr).contains("couldn't find remote ref")
        {
            Ok(())
        } else {
            Err(process::failed(&cmd, &out))
        }
    }

    fn branch_state(
        &self,
        repo: &RepoRoot,
        branch: &BranchName,
    ) -> Result<BranchState, CommandError> {
        let Some(local) = self.local_branch(repo, branch)? else {
            return Ok(BranchState::Missing);
        };
        let branch = local.name();
        let Some(base) = Self::default_base(repo.path())? else {
            return Ok(BranchState::Unknown);
        };
        let ahead = Self::count(repo.path(), &[&format!("{base}..{branch}")])?;
        match NonZeroUsize::new(ahead) {
            None => Ok(BranchState::Safe),
            Some(ahead) => Self::upstream_state(repo, branch, ahead),
        }
    }

    fn dirty_files(&self, worktree: &Path) -> Result<usize, CommandError> {
        Ok(Self::status(worktree)?.len())
    }

    fn unreachable_commits(&self, worktree: &Path) -> Result<usize, CommandError> {
        Self::count(worktree, &["HEAD", "--not", "--branches", "--remotes"])
    }

    fn facts(&self, worktree: &Path) -> Result<WorktreeFacts, CommandError> {
        let status = Status::read(worktree, Self::status(worktree)?);
        let pending = Self::pending(worktree, &status)?;
        let base = Self::default_base(worktree)?;
        let compared = Compared::of(Self::checkout(worktree)?, base.clone());
        let Some(base) = base else {
            return Ok(WorktreeFacts {
                changed: pending.clone(),
                pending,
                ..WorktreeFacts::unchanged(compared)
            });
        };
        let (behind, ahead) = Self::compare(worktree, &base)?;
        Ok(WorktreeFacts {
            ahead,
            behind,
            changed: Self::fork_changes(worktree, &base, &status)?,
            pending,
            compared,
        })
    }

    fn branch_log(&self, worktree: &Path) -> Result<BranchLog, CommandError> {
        let uncommitted = Self::status(worktree)?;
        let Some(base) = Self::default_base(worktree)? else {
            return Ok(BranchLog {
                commits: Vec::new(),
                uncommitted,
            });
        };
        let mut cmd = git(worktree);
        cmd.args(["log", "--oneline", "--no-decorate", "--max-count=500"])
            .arg(format!("{base}..HEAD"));
        let commits = process::run(&mut cmd)?
            .lines()
            .map(str::to_string)
            .collect();
        Ok(BranchLog {
            commits,
            uncommitted,
        })
    }

    fn commit_diff(
        &self,
        worktree: &Path,
        sha: &str,
    ) -> Result<Vec<(Change, FileDiff)>, CommandError> {
        let mut cmd = git(worktree);
        cmd.args([
            "show",
            "--no-color",
            "--no-ext-diff",
            "--format=",
            "-m",
            "--first-parent",
        ])
        .arg(sha);
        match process::output_capped(&mut cmd, DIFF_TIMEOUT, DIFF_BYTES * 4)? {
            Capped::Whole(out) if !out.status.success() => Err(process::failed(&cmd, &out)),
            Capped::Whole(out) => Ok(split_commit(&String::from_utf8_lossy(&out.stdout), false)),
            Capped::Cut { stdout } => Ok(split_commit(&String::from_utf8_lossy(&stdout), true)),
        }
    }

    fn file_diff(
        &self,
        worktree: &Path,
        change: &Change,
        base: DiffBase,
    ) -> Result<FileDiff, CommandError> {
        let path = change.path.as_path();
        let binary = || FileDiff::Binary {
            bytes: std::fs::metadata(worktree.join(path)).ok().map(|m| m.len()),
        };
        let mut cmd = git(worktree);
        cmd.args(["diff", "--no-color", "--no-ext-diff"]);
        let untracked = match change.kind {
            ChangeKind::Tracked {
                lines: Lines::Binary,
                ..
            } => return Ok(binary()),
            ChangeKind::Tracked { .. } => {
                let since = match (base, Self::default_base(worktree)?) {
                    (DiffBase::Fork, Some(default)) => Self::fork_point(worktree, &default)?,
                    (DiffBase::Fork, None) | (DiffBase::Head, _) => "HEAD".to_string(),
                };
                cmd.arg(since).arg("--").arg(path);
                false
            }
            ChangeKind::Untracked { .. } => {
                cmd.args(["--no-index", "--", "/dev/null"]).arg(path);
                true
            }
        };
        let (stdout, cut) = match process::output_capped(&mut cmd, DIFF_TIMEOUT, DIFF_BYTES)? {
            Capped::Cut { stdout } => (stdout, true),
            Capped::Whole(out) => {
                // `--no-index` exits 1 when the files differ.
                let ok = out.status.success() || (untracked && out.status.code() == Some(1));
                if !ok {
                    return Err(process::failed(&cmd, &out));
                }
                (out.stdout, false)
            }
        };
        let text = String::from_utf8_lossy(&stdout);
        Ok(parse_diff(&text, DIFF_LINES, cut).unwrap_or_else(binary))
    }

    fn delete_branch(&self, repo: &RepoRoot, branch: LocalBranch) -> Result<(), CommandError> {
        process::run(git(repo.path()).args(["branch", "-D", branch.name().as_str()])).map(drop)
    }
}

/// Undoes git's quoting of a path with a quote, backslash or control
/// character in it: `"a\"b"` with C escapes and octal bytes.
fn unquote(token: &str) -> Option<String> {
    let inner = token.strip_prefix('"')?.strip_suffix('"')?;
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.bytes().peekable();
    while let Some(b) = chars.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        let escaped = chars.next()?;
        bytes.push(match escaped {
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 11,
            b'0'..=b'7' => {
                let mut value = u32::from(escaped - b'0');
                for _ in 0..2 {
                    let digit = chars.next_if(|d| (b'0'..=b'7').contains(d))?;
                    value = value * 8 + u32::from(digit - b'0');
                }
                u8::try_from(value).ok()?
            }
            other => other,
        });
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// A path as a header line gives it, minus the `prefix` git puts on it:
/// possibly quoted, and followed by a tab when it has a space in it.
fn header_path(raw: &str, prefix: &str) -> Option<String> {
    let path = if raw.starts_with('"') {
        unquote(raw)?
    } else {
        raw.strip_suffix('\t').unwrap_or(raw).to_string()
    };
    path.strip_prefix(prefix).map(str::to_string)
}

/// `x` in `a/x b/x`, which a path with ` b/` in it can't confuse: both halves
/// are the same length.
fn same_path_halves(rest: &str) -> Option<String> {
    let path_len = rest.len().checked_sub(5)? / 2;
    let (old, new) = (rest.get(..2 + path_len)?, rest.get(3 + path_len..)?);
    let path = old.strip_prefix("a/")?;
    (rest.as_bytes().get(2 + path_len) == Some(&b' ') && new.strip_prefix("b/") == Some(path))
        .then(|| path.to_string())
}

/// The new path in `diff --git a/x b/y`, for diffs with no `+++` line.
fn git_header_path(rest: &str) -> Option<String> {
    let second = if rest.starts_with('"') {
        let mut escaped = false;
        let end = rest.char_indices().skip(1).find(|&(_, c)| {
            let closes = c == '"' && !escaped;
            escaped = c == '\\' && !escaped;
            closes
        })?;
        rest[end.0 + 1..].trim_start()
    } else if let Some(at) = rest.find(" \"") {
        &rest[at + 1..]
    } else {
        return same_path_halves(rest).or_else(|| {
            rest.strip_prefix("a/")?
                .split(" b/")
                .next()
                .map(str::to_string)
        });
    };
    header_path(second, "b/")
}

/// The path a file's chunk of `git show` output is about.
fn chunk_path(chunk: &[&str]) -> Option<String> {
    let first_of = |prefix: &str, strip: &str| {
        chunk
            .iter()
            .find_map(|l| l.strip_prefix(prefix).and_then(|r| header_path(r, strip)))
    };
    first_of("+++ ", "b/")
        .or_else(|| first_of("rename to ", ""))
        .or_else(|| first_of("--- ", "a/"))
        .or_else(|| git_header_path(chunk[0].strip_prefix("diff --git ")?))
}

/// The per-file diffs in `git show` output, in git's order, with their
/// counts. A `cut` output ended in the middle of its last file.
fn split_commit(out: &str, cut: bool) -> Vec<(Change, FileDiff)> {
    let mut chunks: Vec<Vec<&str>> = Vec::new();
    for line in out.lines() {
        if line.starts_with("diff --git ") {
            chunks.push(Vec::new());
        }
        if let Some(chunk) = chunks.last_mut() {
            chunk.push(line);
        }
    }
    let last = chunks.len().saturating_sub(1);
    chunks
        .into_iter()
        .enumerate()
        .filter_map(|(i, chunk)| {
            let path = chunk_path(&chunk)?;
            let text = chunk.join("\n");
            let diff = parse_diff(&text, DIFF_LINES, cut && i == last)
                .unwrap_or(FileDiff::Binary { bytes: None });
            let lines = match &diff {
                FileDiff::Binary { .. } => Lines::Binary,
                FileDiff::Text { lines, .. } => Lines::Counted {
                    added: lines.iter().filter(|l| l.starts_with('+')).count(),
                    removed: lines.iter().filter(|l| l.starts_with('-')).count(),
                },
            };
            Some((Change::tracked(path, lines, TrackedState::Committed), diff))
        })
        .collect()
}

/// The hunks of `git diff` output, from the first `@@`, at most `keep` lines
/// of them. `None` when git says the file is binary. A `cut` diff ended
/// mid-line because it was too big to read whole.
fn parse_diff(out: &str, keep: usize, cut: bool) -> Option<FileDiff> {
    let mut lines = out.lines().skip_while(|l| !l.starts_with("@@")).peekable();
    if lines.peek().is_none() && out.lines().any(|l| l.starts_with("Binary files")) {
        return None;
    }
    let short = |line: &str| -> String {
        if line.chars().count() > DIFF_LINE_CHARS {
            line.chars()
                .take(DIFF_LINE_CHARS)
                .chain(std::iter::once('…'))
                .collect()
        } else {
            line.to_string()
        }
    };
    let mut kept: Vec<String> = lines.by_ref().take(keep).map(short).collect();
    let more = lines.count();
    if cut && more == 0 {
        kept.pop();
        kept.push("@@ … the rest of the diff is too big to show".to_string());
    }
    Some(FileDiff::Text { lines: kept, more })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::ports::Readiness;

    #[test]
    fn fetching_a_ref_origin_lacks_is_nothing_fetched_not_a_failure() {
        let (_tmp, origin, work) = clone_with(&[]);
        let work = work.canonicalize().expect("exists");
        let repo = Git.repo_of(&work).expect("succeeds").expect("a repo");
        let branch = |name: &str| name.parse::<BranchName>().expect("parses");
        Git.fetch(&repo, &Fetch::Branch(branch("nosuch")))
            .expect("a branch origin lacks is nothing fetched");
        assert_eq!(
            Git.remote_branch(&repo, &branch("nosuch"))
                .expect("succeeds"),
            None
        );
        Git.fetch(&repo, &Fetch::PullRequest(99))
            .expect("a pull request origin lacks is nothing fetched");
        run_git(&origin, &["branch", "later"]);
        Git.fetch(&repo, &Fetch::Branch(branch("later")))
            .expect("succeeds");
        assert!(
            Git.remote_branch(&repo, &branch("later"))
                .expect("succeeds")
                .is_some()
        );
    }

    #[test]
    fn porcelain_parses_branches_detached_and_skips_prunable() {
        let porcelain = "worktree /src/api\nHEAD abc\nbranch refs/heads/main\n\n\
                         worktree /w/api/fix\nHEAD def\ndetached\n\n\
                         worktree /w/api/gone\nHEAD 123\nprunable gitdir file points to non-existent location\n\n\
                         worktree /w/api/new\nHEAD 456\ndetached\nlocked initializing\n\n\
                         worktree /w/api/held\nHEAD 789\ndetached\nlocked by hand\n\n\
                         worktree /w/api/bare-lock\nHEAD 790\ndetached\nlocked\n\n";
        let worktrees = parse_repo_worktrees(porcelain).expect("succeeds");
        assert_eq!(
            worktrees.main.head,
            Head::Branch("main".parse().expect("parses"))
        );
        assert_eq!(
            worktrees.linked.len(),
            3,
            "one still being created is skipped, locks with other or no reasons stay"
        );
        assert_eq!(worktrees.linked[0].head, Head::Detached);
        assert_eq!(worktrees.linked[1].path, Path::new("/w/api/held"));
        assert_eq!(worktrees.linked[2].path, Path::new("/w/api/bare-lock"));
    }

    #[test]
    fn git_repo_round_trip() {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let dir = tmp.path().to_path_buf();
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&dir)
                    .args(args)
                    .output()
                    .expect("succeeds")
                    .status
                    .success()
            );
        };
        run(&["init", "-q", "-b", "main"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ]);

        let dir = dir.canonicalize().expect("exists");
        let repo = Git.repo_of(&dir).expect("succeeds").expect("succeeds");
        assert_eq!(repo.path(), dir);
        assert_eq!(
            Git.head(&dir).expect("succeeds"),
            Head::Branch("main".parse().expect("parses"))
        );
        let main: BranchName = "main".parse().expect("parses");
        assert_eq!(
            Git.local_branch(&repo, &main).expect("succeeds"),
            Some(LocalBranch::found(main.clone()))
        );
        assert_eq!(
            Git.local_branch(&repo, &"nope".parse().expect("parses"))
                .expect("succeeds"),
            None
        );
        assert!(Git.repo_of(Path::new("/")).expect("succeeds").is_none());
        assert_eq!(
            Git.branch_state(&repo, &main).expect("succeeds"),
            BranchState::Unknown,
            "no origin means no base"
        );
    }

    #[test]
    fn facts_classify_the_checkout_against_the_default_branch() {
        let (_tmp, _origin, work) = clone_with(&[]);
        let run = run_git;
        let compared = || Git.facts(&work).expect("succeeds").compared;
        let branch = |name: &str| -> BranchName { name.parse().expect("parses") };
        let base = || -> RemoteRef { "origin/main".parse().expect("parses") };
        assert_eq!(
            compared(),
            Compared::Trunk {
                branch: branch("main"),
                base: base()
            }
        );
        run(&work, &["checkout", "-q", "-b", "fix-thing"]);
        assert_eq!(
            compared(),
            Compared::Branch {
                branch: branch("fix-thing"),
                base: base()
            }
        );
        run(&work, &["checkout", "-q", "--detach"]);
        let sha = run(&work, &["rev-parse", "--short", "HEAD"]);
        assert_eq!(compared(), Compared::Detached { sha, base: base() });
        run(&work, &["checkout", "-q", "main"]);
        run(&work, &["remote", "remove", "origin"]);
        assert_eq!(
            compared(),
            Compared::NoBase {
                checkout: Checkout::Branch(branch("main"))
            }
        );
    }

    #[test]
    fn facts_count_commits_both_ways_and_changes_since_the_fork() {
        let (_tmp, origin, work) = clone_with(&[]);
        let git = run_git;
        std::fs::write(work.join("a.txt"), "one\ntwo\n").expect("writes");
        git(&work, &["add", "a.txt"]);
        std::fs::write(work.join("b.txt"), "b\n").expect("writes");
        git(&work, &["add", "b.txt"]);
        git(&work, &["commit", "-q", "-m", "a"]);
        std::fs::write(work.join("a.txt"), "one\n").expect("writes");
        git(
            &origin,
            &["commit", "-q", "--allow-empty", "-m", "upstream"],
        );
        git(&work, &["fetch", "-q"]);
        let facts = Git.facts(&work).expect("succeeds");
        assert_eq!((facts.ahead, facts.behind, facts.dirty()), (1, 1, 1));
        assert_eq!(
            facts.compared,
            Compared::Trunk {
                branch: "main".parse().expect("parses"),
                base: "origin/main".parse().expect("parses")
            }
        );
        std::fs::write(work.join("new.txt"), "x\n").expect("writes");
        let new_txt = || Change::untracked("new.txt", Added::Counted(1));
        let facts = Git.facts(&work).expect("succeeds");
        assert_eq!(
            facts.changed,
            [
                Change::tracked(
                    "a.txt",
                    Lines::Counted {
                        added: 1,
                        removed: 0
                    },
                    TrackedState::Unstaged(Edit::Modified)
                ),
                Change::tracked(
                    "b.txt",
                    Lines::Counted {
                        added: 1,
                        removed: 0
                    },
                    TrackedState::Committed
                ),
                new_txt(),
            ],
            "uncommitted and untracked work counts too"
        );
        assert_eq!(
            facts.pending,
            [
                Change::tracked(
                    "a.txt",
                    Lines::Counted {
                        added: 0,
                        removed: 1
                    },
                    TrackedState::Unstaged(Edit::Modified)
                ),
                new_txt(),
            ],
            "uncommitted files count against the last commit, committed ones are left out"
        );
    }

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .expect("succeeds");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// An origin on `main` committed with `files`, and a clone of it.
    fn clone_with(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).expect("creates");
        run_git(&origin, &["init", "-q", "-b", "main"]);
        for (name, text) in files {
            std::fs::write(origin.join(name), text).expect("writes");
            run_git(&origin, &["add", name]);
        }
        run_git(&origin, &["commit", "-q", "--allow-empty", "-m", "init"]);
        run_git(
            tmp.path(),
            &["clone", "-q", origin.to_str().expect("succeeds"), "work"],
        );
        let work = tmp.path().join("work");
        (tmp, origin, work)
    }

    #[test]
    fn branches_lists_local_and_origin_names_without_origin_head() {
        let (_tmp, origin, work) = clone_with(&[("a.txt", "one\n")]);
        run_git(&origin, &["branch", "feature/x"]);
        run_git(&work, &["fetch", "-q"]);
        run_git(&work, &["branch", "mine"]);
        let repo = RepoRoot::new(work).expect("absolute");
        let branches = Git.branches(&repo).expect("lists");
        let names = |list: Vec<agent_core::names::BranchName>| -> Vec<String> {
            list.iter().map(ToString::to_string).collect()
        };
        assert_eq!(names(branches.local), ["main", "mine"]);
        assert_eq!(names(branches.on_origin), ["feature/x", "main"]);
    }

    #[test]
    fn parse_branches_skips_names_git_allows_but_branch_names_refuse() {
        let parsed = parse_branches(
            "refs/heads/ok\nrefs/heads/a b\nrefs/remotes/origin/HEAD\nrefs/remotes/origin/up\nrefs/tags/t\n",
        );
        assert_eq!(parsed.local.len(), 1);
        assert_eq!(parsed.on_origin.len(), 1);
    }

    fn stat(added: usize, removed: usize, files: usize) -> agent_core::ports::DiffStat {
        agent_core::ports::DiffStat {
            files,
            added,
            removed,
        }
    }

    #[test]
    fn untracked_files_add_their_lines_in_both_tabs() {
        let (_tmp, _origin, work) = clone_with(&[("a.txt", "one\n")]);
        std::fs::create_dir_all(work.join("sub")).expect("creates");
        std::fs::write(work.join("sub/three.txt"), "a\nb\nc").expect("writes");
        std::fs::write(work.join("two.txt"), "a\nb\n").expect("writes");
        std::fs::write(work.join("empty.txt"), "").expect("writes");
        std::fs::write(work.join("logo.bin"), b"\x89PNG\0\0").expect("writes");
        run_git(&work, &["checkout", "-q", "-b", "topic"]);
        let facts = Git.facts(&work).expect("succeeds");
        let added = |changes: &[Change], path: &str| {
            let change = changes
                .iter()
                .find(|c| c.path == Path::new(path))
                .unwrap_or_else(|| panic!("{path} is listed"));
            let ChangeKind::Untracked { added } = change.kind else {
                panic!("{path} is untracked");
            };
            added
        };
        for tab in [&facts.pending, &facts.changed] {
            assert_eq!(
                added(tab, "sub/three.txt"),
                Added::Counted(3),
                "a last line with no newline still counts"
            );
            assert_eq!(added(tab, "two.txt"), Added::Counted(2));
            assert_eq!(added(tab, "empty.txt"), Added::Counted(0));
            assert_eq!(added(tab, "logo.bin"), Added::Binary);
        }
        assert_eq!(facts.pending_diff(), stat(5, 0, 4));
        assert_eq!(facts.diff(), stat(5, 0, 4));
    }

    #[test]
    fn what_is_not_a_plain_file_is_unread() {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let file = tmp.path().join("f.txt");
        std::fs::write(&file, "x\n").expect("writes");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&file, &link).expect("links");
        let mut budget = Budget(UNTRACKED_BUDGET);
        assert_eq!(read_added(&file, &mut budget), Added::Counted(1));
        assert_eq!(read_added(&link, &mut budget), Added::Unread);
        assert_eq!(read_added(tmp.path(), &mut budget), Added::Unread);
        assert_eq!(
            read_added(&tmp.path().join("missing"), &mut budget),
            Added::Unread
        );
    }

    #[test]
    fn a_file_that_changed_size_while_it_was_read_is_unread() {
        assert_eq!(added_of(b"a\nb\n", 4), Added::Counted(2));
        assert_eq!(added_of(b"a\nb\n", 3), Added::Unread);
    }

    #[test]
    fn a_nested_repo_is_listed_unread() {
        let (_tmp, _origin, work) = clone_with(&[("a.txt", "one\n")]);
        let inner = work.join("inner");
        std::fs::create_dir_all(&inner).expect("creates");
        run_git(&inner, &["init", "-q", "-b", "main"]);
        std::fs::write(inner.join("f.txt"), "x\n").expect("writes");
        let facts = Git.facts(&work).expect("succeeds");
        assert_eq!(facts.pending.len(), 1, "the nested repo is one entry");
        for change in &facts.pending {
            assert_eq!(
                change.kind,
                ChangeKind::Untracked {
                    added: Added::Unread
                },
                "{}",
                change.path.display()
            );
        }
    }

    #[test]
    fn untracked_files_past_the_byte_budget_are_listed_unread() {
        let (_tmp, _origin, work) = clone_with(&[("a.txt", "one\n")]);
        let mib = 1024 * 1024;
        let lines = vec![b'\n'; 5 * mib];
        assert!(2 * lines.len() as u64 > UNTRACKED_BUDGET);
        std::fs::write(work.join("a_first.log"), &lines).expect("writes");
        std::fs::write(work.join("b_second.log"), &lines).expect("writes");
        std::fs::write(work.join("c_small.txt"), "x\n").expect("writes");
        let facts = Git.facts(&work).expect("succeeds");
        let kinds: Vec<_> = facts.pending.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds,
            [
                ChangeKind::Untracked {
                    added: Added::Counted(5 * mib)
                },
                ChangeKind::Untracked {
                    added: Added::Unread
                },
                ChangeKind::Untracked {
                    added: Added::Counted(1)
                },
            ],
            "a file that doesn't fit what is left is unread, a smaller one after it still counts"
        );
    }

    #[test]
    fn deleted_files_add_their_removed_lines_staged_unstaged_or_committed() {
        let (_tmp, _origin, work) = clone_with(&[
            ("unstaged.txt", "1\n2\n3\n"),
            ("staged.txt", "1\n2\n"),
            ("committed.txt", "1\n2\n3\n4\n"),
        ]);
        run_git(&work, &["checkout", "-q", "-b", "topic"]);
        run_git(&work, &["rm", "-q", "committed.txt"]);
        run_git(&work, &["commit", "-q", "-m", "drop"]);
        run_git(&work, &["rm", "-q", "staged.txt"]);
        std::fs::remove_file(work.join("unstaged.txt")).expect("removes");
        let facts = Git.facts(&work).expect("succeeds");
        assert_eq!(
            facts.pending_diff(),
            stat(0, 5, 2),
            "uncommitted deletions count against the last commit"
        );
        assert_eq!(
            facts.diff(),
            stat(0, 9, 3),
            "the branch's own deletion counts since the fork point"
        );
    }

    fn repo_with(files: &[(&str, &str)]) -> (tempfile::TempDir, impl Fn(&[&str]) -> bool) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().to_path_buf();
        let git = move |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .expect("succeeds");
            out.status.success()
        };
        assert!(git(&["init", "-q", "-b", "main"]));
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).expect("writes");
            assert!(git(&["add", name]));
        }
        assert!(git(&["commit", "-q", "-m", "init"]));
        (dir, git)
    }

    fn pending_states(dir: &Path) -> Vec<(String, FileState)> {
        let facts = Git.facts(dir).expect("succeeds");
        assert_eq!(facts.dirty(), facts.pending.len());
        let mut states: Vec<_> = facts
            .pending
            .iter()
            .map(|c| (c.path.display().to_string(), c.state()))
            .collect();
        states.sort_by(|a, b| a.0.cmp(&b.0));
        states
    }

    #[test]
    fn uncommitted_files_without_a_diff_against_head_still_count() {
        let (dir, git) = repo_with(&[("a.txt", "one\n"), ("gone.txt", "x\n")]);
        std::fs::write(dir.path().join("a.txt"), "two\n").expect("writes");
        assert!(git(&["add", "a.txt"]));
        std::fs::write(dir.path().join("a.txt"), "one\n").expect("writes");
        std::fs::write(dir.path().join("new.txt"), "n\n").expect("writes");
        assert!(git(&["add", "new.txt"]));
        std::fs::remove_file(dir.path().join("new.txt")).expect("removes");
        assert_eq!(
            pending_states(dir.path()),
            [
                (
                    "a.txt".to_string(),
                    FileState::Both {
                        staged: Edit::Modified,
                        unstaged: Edit::Modified
                    }
                ),
                (
                    "new.txt".to_string(),
                    FileState::Both {
                        staged: Edit::Added,
                        unstaged: Edit::Deleted
                    }
                ),
            ],
            "a staged edit reverted in the tree, and a staged add deleted from it"
        );
    }

    #[test]
    fn a_conflicted_merge_lists_each_unmerged_file_as_conflicted() {
        let (dir, git) = repo_with(&[("f.txt", "x\n"), ("g.txt", "x\n")]);
        assert!(git(&["checkout", "-q", "-b", "side"]));
        std::fs::write(dir.path().join("f.txt"), "side\n").expect("writes");
        assert!(git(&["commit", "-q", "-am", "side f"]));
        assert!(git(&["rm", "-q", "g.txt"]));
        assert!(git(&["commit", "-q", "-m", "side g"]));
        assert!(git(&["checkout", "-q", "main"]));
        std::fs::write(dir.path().join("f.txt"), "main\n").expect("writes");
        std::fs::write(dir.path().join("g.txt"), "main\n").expect("writes");
        assert!(git(&["commit", "-q", "-am", "main"]));
        assert!(!git(&["merge", "-q", "side"]), "the merge conflicts");
        assert_eq!(
            pending_states(dir.path()),
            [
                ("f.txt".to_string(), FileState::Conflicted),
                ("g.txt".to_string(), FileState::Conflicted),
            ],
            "UU and UD"
        );
        let facts = Git.facts(dir.path()).expect("succeeds");
        assert!(matches!(facts.readiness(), Readiness::Dirty { .. }));
    }

    #[test]
    fn diffs_keep_the_hunks_cap_their_length_and_spot_binaries() {
        let out =
            "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n a\n+b\n+c\n";
        assert_eq!(
            parse_diff(out, 10, false),
            Some(FileDiff::Text {
                lines: vec![
                    "@@ -1 +1,2 @@".into(),
                    " a".into(),
                    "+b".into(),
                    "+c".into()
                ],
                more: 0
            })
        );
        assert_eq!(
            parse_diff(out, 2, false),
            Some(FileDiff::Text {
                lines: vec!["@@ -1 +1,2 @@".into(), " a".into()],
                more: 2
            })
        );
        let big = format!("@@ -1 +1 @@\n+{}\n+tail", "x".repeat(DIFF_LINE_CHARS + 50));
        let Some(FileDiff::Text { lines, more }) = parse_diff(&big, 10, false) else {
            panic!("text");
        };
        assert_eq!(
            lines[1].chars().count(),
            DIFF_LINE_CHARS + 1,
            "a long line is cut"
        );
        assert!(lines[1].ends_with('…'));
        assert_eq!((lines.len(), more), (3, 0));
        assert_eq!(
            parse_diff("@@ -1 +1 @@\n+a\n+par", 10, true),
            Some(FileDiff::Text {
                lines: vec![
                    "@@ -1 +1 @@".into(),
                    "+a".into(),
                    "@@ … the rest of the diff is too big to show".into()
                ],
                more: 0
            }),
            "the partial last line goes and the cut is said"
        );
        assert_eq!(
            parse_diff("Binary files a/x and b/x differ\n", 10, false),
            None
        );
        assert_eq!(
            parse_diff("", 10, false),
            Some(FileDiff::Text {
                lines: Vec::new(),
                more: 0
            }),
            "a mode change alone has no hunks"
        );
    }

    #[test]
    fn a_files_diff_runs_from_the_fork_and_shows_a_new_file_whole() {
        let (_tmp, _origin, work) = clone_with(&[("a.txt", "one\n")]);
        let git = run_git;
        std::fs::write(work.join("a.txt"), "one\ntwo\n").expect("writes");
        git(&work, &["commit", "-q", "-am", "two"]);
        std::fs::write(work.join("a.txt"), "one\ntwo\nthree\n").expect("writes");
        std::fs::write(work.join("new.txt"), "x\ny\n").expect("writes");
        let tracked = |path: &str, lines| {
            Change::tracked(path, lines, TrackedState::Unstaged(Edit::Modified))
        };
        let counted = tracked(
            "a.txt",
            Lines::Counted {
                added: 2,
                removed: 0,
            },
        );
        let FileDiff::Text { lines, more } = Git
            .file_diff(&work, &counted, DiffBase::Fork)
            .expect("succeeds")
        else {
            panic!("a text diff");
        };
        assert_eq!(more, 0);
        assert_eq!(
            lines[1..],
            [" one", "+two", "+three"],
            "committed and uncommitted together"
        );
        let FileDiff::Text { lines, .. } = Git
            .file_diff(&work, &counted, DiffBase::Head)
            .expect("succeeds")
        else {
            panic!("a text diff");
        };
        assert_eq!(
            lines[1..],
            [" one", " two", "+three"],
            "only what's uncommitted"
        );
        let FileDiff::Text { lines, .. } = Git
            .file_diff(
                &work,
                &Change::untracked("new.txt", Added::Unread),
                DiffBase::Fork,
            )
            .expect("succeeds")
        else {
            panic!("a text diff");
        };
        assert_eq!(lines[1..], ["+x", "+y"]);
        let log = Git.branch_log(&work).expect("succeeds");
        assert_eq!(log.commits.len(), 1);
        assert!(log.commits[0].ends_with(" two"), "{:?}", log.commits);
        let files: Vec<&Path> = log.uncommitted.iter().map(|f| f.path.as_path()).collect();
        assert_eq!(files, [Path::new("a.txt"), Path::new("new.txt")]);
        assert_eq!(
            Git.file_diff(&work, &tracked("new.txt", Lines::Binary), DiffBase::Fork)
                .expect("succeeds"),
            FileDiff::Binary { bytes: Some(4) }
        );
    }

    #[test]
    fn numstat_reads_binary_files_and_odd_paths() {
        assert_eq!(
            parse_numstat("3\t1\tsrc/a b.rs\0-\t-\tlogo.png\0"),
            [
                Change::tracked(
                    "src/a b.rs",
                    Lines::Counted {
                        added: 3,
                        removed: 1
                    },
                    TrackedState::Committed
                ),
                Change::tracked("logo.png", Lines::Binary, TrackedState::Committed),
            ]
        );
    }

    #[test]
    fn status_codes_parse_into_states() {
        use Edit::*;
        let cases = [
            ("??", FileState::Untracked),
            (" M", FileState::Unstaged(Modified)),
            ("M ", FileState::Staged(Modified)),
            ("A ", FileState::Staged(Added)),
            ("D ", FileState::Staged(Deleted)),
            (" D", FileState::Unstaged(Deleted)),
            (
                "MM",
                FileState::Both {
                    staged: Modified,
                    unstaged: Modified,
                },
            ),
            (
                "AM",
                FileState::Both {
                    staged: Added,
                    unstaged: Modified,
                },
            ),
            ("  ", FileState::Committed),
            ("UU", FileState::Conflicted),
            ("AA", FileState::Conflicted),
            ("DD", FileState::Conflicted),
            ("UD", FileState::Conflicted),
            ("DU", FileState::Conflicted),
            ("AU", FileState::Conflicted),
            ("UA", FileState::Conflicted),
        ];
        for (code, state) in cases {
            let mut chars = code.chars();
            let (index, tree) = (chars.next().expect("two"), chars.next().expect("two"));
            assert_eq!(parse_state(index, tree), state, "{code:?}");
        }
    }

    #[test]
    fn status_lists_each_file_with_its_parsed_state() {
        assert_eq!(
            parse_status(" M src/a.rs\0A  new.rs\0?? notes/b.md\0"),
            [
                Uncommitted {
                    path: "src/a.rs".into(),
                    state: FileState::Unstaged(Edit::Modified)
                },
                Uncommitted {
                    path: "new.rs".into(),
                    state: FileState::Staged(Edit::Added)
                },
                Uncommitted {
                    path: "notes/b.md".into(),
                    state: FileState::Untracked
                },
            ]
        );
    }

    #[test]
    fn only_branches_deleted_from_their_own_remote_count_as_merged() {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let sh = |dir: &Path, script: &str| {
            let out = Command::new("sh")
                .arg("-c")
                .arg(script)
                .current_dir(dir)
                .output()
                .expect("succeeds");
            assert!(
                out.status.success(),
                "{script}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let commit = "git -c user.name=t -c user.email=t@t commit -q --allow-empty -m";
        sh(
            tmp.path(),
            "git init -q --bare -b main origin.git && git init -q -b main work",
        );
        let work = tmp.path().join("work").canonicalize().expect("exists");
        sh(
            &work,
            &format!(
                "{commit} init && git remote add origin ../origin.git && git push -q -u origin main && git remote set-head origin main"
            ),
        );
        let repo = Git.repo_of(&work).expect("succeeds").expect("succeeds");

        sh(
            &work,
            &format!("git checkout -q -b tracks-main origin/main && {commit} wip"),
        );
        let tracks_main: BranchName = "tracks-main".parse().expect("parses");
        assert!(matches!(
            Git.branch_state(&repo, &tracks_main).expect("succeeds"),
            BranchState::Unmerged { .. }
        ));

        sh(
            &work,
            &format!(
                "git checkout -q -b merged main && {commit} done && git push -q -u origin merged && git -C ../origin.git branch -q -D merged"
            ),
        );
        let merged: BranchName = "merged".parse().expect("parses");
        assert_eq!(
            Git.branch_state(&repo, &merged).expect("succeeds"),
            BranchState::Safe,
            "tracking ref still local, nothing after it"
        );

        sh(&work, &format!("{commit} after-merge"));
        assert!(
            matches!(
                Git.branch_state(&repo, &merged).expect("succeeds"),
                BranchState::UpstreamGone { .. }
            ),
            "commits after the push"
        );

        sh(&work, "git fetch -q --prune origin");
        assert!(
            matches!(
                Git.branch_state(&repo, &merged).expect("succeeds"),
                BranchState::UpstreamGone { .. }
            ),
            "can't tell once the tracking ref is pruned"
        );

        sh(&work, "git checkout -q main");
        let main: BranchName = "main".parse().expect("parses");
        assert_eq!(
            Git.branch_state(&repo, &main).expect("succeeds"),
            BranchState::Safe
        );
    }

    #[test]
    fn dirty_files_and_detached_commits_are_counted() {
        let tmp = tempfile::tempdir().expect("a temp dir");
        let dir = tmp.path().canonicalize().expect("exists");
        let sh = |script: &str| {
            assert!(
                Command::new("sh")
                    .arg("-c")
                    .arg(script)
                    .current_dir(&dir)
                    .status()
                    .expect("succeeds")
                    .success()
            );
        };
        let commit = "git -c user.name=t -c user.email=t@t commit -q --allow-empty -m";
        sh(&format!("git init -q -b main && {commit} init"));
        assert_eq!(Git.dirty_files(&dir).expect("succeeds"), 0);
        sh("echo x > new.txt && echo y > other.txt");
        assert_eq!(Git.dirty_files(&dir).expect("succeeds"), 2);
        assert_eq!(Git.unreachable_commits(&dir).expect("succeeds"), 0);
        sh(&format!("git checkout -q --detach && {commit} detached"));
        assert_eq!(Git.unreachable_commits(&dir).expect("succeeds"), 1);
    }

    #[test]
    fn a_commit_splits_into_files_with_counts() {
        let out = "diff --git a/src/a.rs b/src/a.rs\nindex 1..2 100644\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n keep\ndiff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n";
        let files = split_commit(out, false);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0.path, PathBuf::from("src/a.rs"));
        assert_eq!(
            files[0].0.lines(),
            Lines::Counted {
                added: 1,
                removed: 1
            }
        );
        assert_eq!(files[1].0.path, PathBuf::from("img.png"));
        assert_eq!(files[1].0.lines(), Lines::Binary);
    }

    #[test]
    fn commit_paths_survive_quoting_tabs_deletions_and_renames() {
        let out = concat!(
            "diff --git \"a/q\\\"x.rs\" \"b/q\\\"x.rs\"\n--- \"a/q\\\"x.rs\"\n+++ \"b/q\\\"x.rs\"\n@@ -1 +1 @@\n-a\n+b\n",
            "diff --git a/my file.rs b/my file.rs\n--- a/my file.rs\t\n+++ b/my file.rs\t\n@@ -1 +1 @@\n-a\n+b\n",
            "diff --git \"a/caf\\303\\251.rs\" \"b/caf\\303\\251.rs\"\ndeleted file mode 100644\n--- \"a/caf\\303\\251.rs\"\n+++ /dev/null\n@@ -1 +0,0 @@\n-a\n",
            "diff --git a/old.rs b/new name.rs\nsimilarity index 100%\nrename from old.rs\nrename to new name.rs\n",
            "diff --git a/mode.sh b/mode.sh\nold mode 100644\nnew mode 100755\n",
            "diff --git a/dir b/x b/dir b/x\nold mode 100644\nnew mode 100755\n",
        );
        let paths: Vec<_> = split_commit(out, false)
            .into_iter()
            .map(|(c, _)| c.path)
            .collect();
        assert_eq!(
            paths,
            [
                PathBuf::from("q\"x.rs"),
                PathBuf::from("my file.rs"),
                PathBuf::from("caf\u{e9}.rs"),
                PathBuf::from("new name.rs"),
                PathBuf::from("mode.sh"),
                PathBuf::from("dir b/x"),
            ]
        );
    }

    #[test]
    fn a_cut_commit_says_so_in_its_last_file() {
        let out = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-a\n+b\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-x\n+par";
        let files = split_commit(out, true);
        let text = |i: usize| match &files[i].1 {
            FileDiff::Text { lines, .. } => lines.clone(),
            FileDiff::Binary { .. } => panic!("text"),
        };
        assert!(text(0).iter().all(|l| !l.contains("too big")));
        assert!(
            text(1)
                .last()
                .is_some_and(|l| l.contains("too big to show")),
            "{:?}",
            text(1)
        );
    }
}
