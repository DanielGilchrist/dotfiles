function _agent_checkout --description "agent checkout — attach the existing worktree for a branch, or create one from local/remote and spawn (no prompt)."
    argparse --name='agent checkout' 'h/help' 'no-focus' 'headless' 'repo=' -- $argv
    or return

    if set -q _flag_help
        echo "usage: agent checkout <branch>"
        echo ""
        echo "  <branch>        — branch to check out. Resolution order:"
        echo "                    1. existing worktree with this branch → attach"
        echo "                    2. local branch (no worktree) → create worktree, attach"
        echo "                    3. origin/<branch>            → track locally, create worktree, attach"
        echo ""
        echo "The zellij session + worktree dir use a sanitized name (slashes → dashes, capped at 25 chars)."
        echo "Attach is always --no-prompt: Claude starts with no initial message."
        return 0
    end

    set -l branch $argv[1]
    if test -z "$branch"
        echo "agent checkout: <branch> required" >&2
        return 1
    end

    set -l repo_root $_flag_repo
    if test -z "$repo_root"
        set repo_root (git worktree list --porcelain 2>/dev/null | head -1 | string replace -r '^worktree ' '')
    end
    if test -z "$repo_root"
        echo "agent checkout: not in a git repo and --repo not given" >&2
        return 1
    end
    set -l resolved_main (git -C $repo_root worktree list --porcelain 2>/dev/null | head -1 | string replace -r '^worktree ' '')
    test -n "$resolved_main"; and set repo_root $resolved_main
    set -l repo_name (basename $repo_root)

    set -l sanitized (string replace -a '/' '-' -- $branch | string sub -l 20)
    if test "$sanitized" != "$branch"
        echo "agent checkout: branch name too long — using session/worktree name '$sanitized'" >&2
    end
    if test -z "$sanitized"
        echo "agent checkout: sanitized name empty for '$branch'" >&2
        return 1
    end

    set -l existing_wt
    set -l cur
    for line in (git -C $repo_root worktree list --porcelain 2>/dev/null)
        set -l parts (string split ' ' -- $line)
        if test "$parts[1]" = worktree
            set cur $parts[2]
        else if test "$parts[1]" = branch; and test "$parts[2]" = "refs/heads/$branch"
            set existing_wt $cur
            break
        end
    end

    set -l forward_flags --no-prompt --repo $repo_root
    set -q _flag_no_focus; and set -a forward_flags --no-focus
    set -q _flag_headless; and set -a forward_flags --headless

    if test -n "$existing_wt"
        set -l existing_name (basename $existing_wt)
        echo "agent checkout: found existing worktree for $branch at $existing_wt"
        _agent_attach $existing_name $forward_flags
        return $status
    end

    set -l worktree_path "$HOME/worktrees/$repo_name/$sanitized"
    if test -d "$worktree_path"
        echo "agent checkout: worktree path $worktree_path already exists but branch $branch is not checked out there" >&2
        return 1
    end
    mkdir -p (dirname $worktree_path)

    if git -C $repo_root rev-parse --verify --quiet "refs/heads/$branch" >/dev/null
        if not git -C $repo_root worktree add $worktree_path $branch 2>&1
            echo "agent checkout: git worktree add failed for local branch $branch" >&2
            return 1
        end
        _agent_attach $sanitized $forward_flags
        return $status
    end

    if git -C $repo_root rev-parse --verify --quiet "refs/remotes/origin/$branch" >/dev/null
        if not git -C $repo_root worktree add -b $branch $worktree_path "origin/$branch" 2>&1
            echo "agent checkout: git worktree add -b $branch failed" >&2
            return 1
        end
        _agent_attach $sanitized $forward_flags
        return $status
    end

    echo "agent checkout: branch '$branch' not found locally or on origin (try \`git fetch\`)" >&2
    return 1
end
