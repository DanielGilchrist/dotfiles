use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use agent_core::model::{Head, Worktree};
use agent_core::names::BranchName;
use agent_core::paths::{AgentWorktree, RepoRoot};
use agent_core::ports::{BranchState, LocalBranch, RemoteBranch, RepoWorktrees, Vcs, WorktreeStart};
use agent_core::process::{self, CommandError};

/// A checkout of a large repo can take minutes.
const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(600);

/// [`Vcs`] backed by the `git` CLI.
#[derive(Debug, Default, Clone, Copy)]
pub struct Git;

/// Whether the user has their own ssh command for git; if not, ours runs ssh
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
    cmd.env("GIT_TERMINAL_PROMPT", "0").arg("-C").arg(dir);
    if !custom_ssh() {
        cmd.env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes");
    }
    cmd
}

fn parse_error(cmd: &Command, detail: impl Into<String>) -> CommandError {
    CommandError::Parse { command: process::describe(cmd), detail: detail.into() }
}

/// Runs a query whose exit code 1 means "no" (`rev-parse --verify --quiet`).
fn query(mut cmd: Command) -> Result<bool, CommandError> {
    let out = process::output(&mut cmd)?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(process::failed(&cmd, &out)),
    }
}

fn parse_repo_worktrees(porcelain: &str) -> Result<RepoWorktrees, String> {
    let mut worktrees = parse_worktrees(porcelain)?.into_iter();
    let main = worktrees.next().ok_or("no worktrees listed")?;
    Ok(RepoWorktrees { main, linked: worktrees.collect() })
}

fn parse_worktrees(porcelain: &str) -> Result<Vec<Worktree>, String> {
    porcelain
        .split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .filter(|block| !block.lines().any(|l| l == "bare" || l.starts_with("prunable")))
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
            Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string())),
            Some(1) => Ok(None),
            _ => Err(process::failed(&cmd, &out)),
        }
    }

    fn count(dir: &Path, range: &[&str]) -> Result<usize, CommandError> {
        let mut cmd = git(dir);
        cmd.args(["rev-list", "--count"]).args(range);
        let count = process::run(&mut cmd)?;
        count.trim().parse().map_err(|_| parse_error(&cmd, count.trim()))
    }

    /// For a branch with commits ahead of the base: did it track its own
    /// branch on a remote that has since been deleted (merged with
    /// auto-delete)? Safe only if nothing was committed after the last push,
    /// which needs the remote-tracking ref to still be present locally.
    fn upstream_state(repo: &RepoRoot, branch: &BranchName, ahead: NonZeroUsize) -> Result<BranchState, CommandError> {
        let unmerged = BranchState::Unmerged { ahead };
        let (Some(remote), Some(merge)) =
            (Self::config(repo, &format!("branch.{branch}.remote"))?, Self::config(repo, &format!("branch.{branch}.merge"))?)
        else {
            return Ok(unmerged);
        };
        if merge != format!("refs/heads/{branch}") || remote == "." {
            return Ok(unmerged);
        }
        let mut ls = git(repo.path());
        ls.args(["ls-remote", "--exit-code", "--heads", &remote, branch.as_str()]);
        let out = process::output(&mut ls)?;
        match out.status.code() {
            Some(0) => return Ok(unmerged),
            Some(2) => {}
            _ => return Err(process::failed(&ls, &out)),
        }
        let tracking = format!("refs/remotes/{remote}/{branch}");
        let mut verify = git(repo.path());
        verify.args(["rev-parse", "--verify", "--quiet", &tracking]);
        if query(verify)? && Self::count(repo.path(), &[&format!("{tracking}..{branch}")])? == 0 {
            return Ok(BranchState::Safe);
        }
        Ok(BranchState::UpstreamGone { ahead })
    }

    fn default_base(repo: &RepoRoot) -> Result<Option<String>, CommandError> {
        let mut cmd = git(repo.path());
        cmd.args(["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]);
        let out = process::output(&mut cmd)?;
        if out.status.success() {
            return Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string()));
        }
        for candidate in ["origin/main", "origin/master"] {
            let mut cmd = git(repo.path());
            cmd.args(["rev-parse", "--verify", "--quiet", candidate]);
            if query(cmd)? {
                return Ok(Some(candidate.into()));
            }
        }
        Ok(None)
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
        RepoRoot::new(worktrees.main.path).map(Some).map_err(|e| parse_error(&cmd, e.to_string()))
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
                name.parse().map(Head::Branch).map_err(|e: agent_core::names::NameError| parse_error(&cmd, e.to_string()))
            }
            Some(1) => Ok(Head::Detached),
            _ => Err(process::failed(&cmd, &out)),
        }
    }

    fn add_worktree(&self, repo: &RepoRoot, at: &AgentWorktree, start: &WorktreeStart) -> Result<(), CommandError> {
        let mut cmd = git(repo.path());
        cmd.args(["-c", "checkout.workers=8", "-c", "checkout.thresholdForParallelism=100", "worktree", "add"]);
        match start {
            WorktreeStart::DefaultBase => {
                cmd.arg("--detach").arg(at.path());
                if let Some(base) = Self::default_base(repo)? {
                    cmd.arg(base);
                }
            }
            WorktreeStart::Branch(local) => {
                cmd.arg(at.path()).arg(local.name().as_str());
            }
            WorktreeStart::TrackRemote(remote) => {
                let branch = remote.name();
                cmd.args(["-b", branch.as_str()]).arg(at.path()).arg(format!("origin/{branch}"));
            }
        }
        let out = process::output_within(&mut cmd, CHECKOUT_TIMEOUT)?;
        if out.status.success() { Ok(()) } else { Err(process::failed(&cmd, &out)) }
    }

    fn prune_worktrees(&self, repo: &RepoRoot) -> Result<(), CommandError> {
        process::run(git(repo.path()).args(["worktree", "prune"])).map(drop)
    }

    fn local_branch(&self, repo: &RepoRoot, branch: &BranchName) -> Result<Option<LocalBranch>, CommandError> {
        let mut cmd = git(repo.path());
        cmd.args(["rev-parse", "--verify", "--quiet"]).arg(format!("refs/heads/{branch}"));
        Ok(query(cmd)?.then(|| LocalBranch::found(branch.clone())))
    }

    fn remote_branch(&self, repo: &RepoRoot, branch: &BranchName) -> Result<Option<RemoteBranch>, CommandError> {
        let mut cmd = git(repo.path());
        cmd.args(["rev-parse", "--verify", "--quiet"]).arg(format!("refs/remotes/origin/{branch}"));
        Ok(query(cmd)?.then(|| RemoteBranch::found(branch.clone())))
    }

    fn branch_state(&self, repo: &RepoRoot, branch: &BranchName) -> Result<BranchState, CommandError> {
        let Some(local) = self.local_branch(repo, branch)? else { return Ok(BranchState::Missing) };
        let branch = local.name();
        let Some(base) = Self::default_base(repo)? else { return Ok(BranchState::Unknown) };
        let ahead = Self::count(repo.path(), &[&format!("{base}..{branch}")])?;
        match NonZeroUsize::new(ahead) {
            None => Ok(BranchState::Safe),
            Some(ahead) => Self::upstream_state(repo, branch, ahead),
        }
    }

    fn dirty_files(&self, worktree: &Path) -> Result<usize, CommandError> {
        let mut cmd = git(worktree);
        cmd.args(["status", "--porcelain=v1", "-z", "--untracked-files=all"]);
        let out = process::run(&mut cmd)?;
        Ok(out.split('\0').filter(|entry| entry.len() > 3).count())
    }

    fn unreachable_commits(&self, worktree: &Path) -> Result<usize, CommandError> {
        Self::count(worktree, &["HEAD", "--not", "--branches", "--remotes"])
    }

    fn delete_branch(&self, repo: &RepoRoot, branch: LocalBranch) -> Result<(), CommandError> {
        process::run(git(repo.path()).args(["branch", "-D", branch.name().as_str()])).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_parses_branches_detached_and_skips_prunable() {
        let porcelain = "worktree /src/api\nHEAD abc\nbranch refs/heads/main\n\n\
                         worktree /w/api/fix\nHEAD def\ndetached\n\n\
                         worktree /w/api/gone\nHEAD 123\nprunable gitdir file points to non-existent location\n\n";
        let worktrees = parse_repo_worktrees(porcelain).unwrap();
        assert_eq!(worktrees.main.head, Head::Branch("main".parse().unwrap()));
        assert_eq!(worktrees.linked.len(), 1);
        assert_eq!(worktrees.linked[0].head, Head::Detached);
    }

    #[test]
    fn git_repo_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let run = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success());
        run(&["init", "-q", "-b", "main"]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"]);

        let dir = dir.canonicalize().unwrap();
        let repo = Git.repo_of(&dir).unwrap().unwrap();
        assert_eq!(repo.path(), dir);
        assert_eq!(Git.head(&dir).unwrap(), Head::Branch("main".parse().unwrap()));
        let main: BranchName = "main".parse().unwrap();
        assert_eq!(Git.local_branch(&repo, &main).unwrap(), Some(LocalBranch::found(main.clone())));
        assert_eq!(Git.local_branch(&repo, &"nope".parse().unwrap()).unwrap(), None);
        assert!(Git.repo_of(Path::new("/")).unwrap().is_none());
        assert_eq!(Git.branch_state(&repo, &main).unwrap(), BranchState::Unknown, "no origin means no base");
    }

    #[test]
    fn only_branches_deleted_from_their_own_remote_count_as_merged() {
        let tmp = tempfile::tempdir().unwrap();
        let sh = |dir: &Path, script: &str| {
            let out = Command::new("sh").arg("-c").arg(script).current_dir(dir).output().unwrap();
            assert!(out.status.success(), "{script}: {}", String::from_utf8_lossy(&out.stderr));
        };
        let commit = "git -c user.name=t -c user.email=t@t commit -q --allow-empty -m";
        sh(tmp.path(), "git init -q --bare -b main origin.git && git init -q -b main work");
        let work = tmp.path().join("work").canonicalize().unwrap();
        sh(&work, &format!("{commit} init && git remote add origin ../origin.git && git push -q -u origin main && git remote set-head origin main"));
        let repo = Git.repo_of(&work).unwrap().unwrap();

        sh(&work, &format!("git checkout -q -b tracks-main origin/main && {commit} wip"));
        let tracks_main: BranchName = "tracks-main".parse().unwrap();
        assert!(matches!(Git.branch_state(&repo, &tracks_main).unwrap(), BranchState::Unmerged { .. }));

        sh(&work, &format!("git checkout -q -b merged main && {commit} done && git push -q -u origin merged && git -C ../origin.git branch -q -D merged"));
        let merged: BranchName = "merged".parse().unwrap();
        assert_eq!(Git.branch_state(&repo, &merged).unwrap(), BranchState::Safe, "tracking ref still local, nothing after it");

        sh(&work, &format!("{commit} after-merge"));
        assert!(matches!(Git.branch_state(&repo, &merged).unwrap(), BranchState::UpstreamGone { .. }), "commits after the push");

        sh(&work, "git fetch -q --prune origin");
        assert!(matches!(Git.branch_state(&repo, &merged).unwrap(), BranchState::UpstreamGone { .. }), "can't tell once the tracking ref is pruned");

        sh(&work, "git checkout -q main");
        let main: BranchName = "main".parse().unwrap();
        assert_eq!(Git.branch_state(&repo, &main).unwrap(), BranchState::Safe);
    }

    #[test]
    fn dirty_files_and_detached_commits_are_counted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let sh = |script: &str| assert!(Command::new("sh").arg("-c").arg(script).current_dir(&dir).status().unwrap().success());
        let commit = "git -c user.name=t -c user.email=t@t commit -q --allow-empty -m";
        sh(&format!("git init -q -b main && {commit} init"));
        assert_eq!(Git.dirty_files(&dir).unwrap(), 0);
        sh("echo x > new.txt && echo y > other.txt");
        assert_eq!(Git.dirty_files(&dir).unwrap(), 2);
        assert_eq!(Git.unreachable_commits(&dir).unwrap(), 0);
        sh(&format!("git checkout -q --detach && {commit} detached"));
        assert_eq!(Git.unreachable_commits(&dir).unwrap(), 1);
    }
}
