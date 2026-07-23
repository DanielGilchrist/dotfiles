function agent-merged --description "List agent worktrees whose branch is safe to remove (no commits ahead of base, or pushed + upstream deleted). Prints one name per line."
    argparse --name=agent-merged 'h/help' 'v/verbose' -- $argv
    or return

    if set -q _flag_help
        echo "usage: agent-merged [-v]"
        echo ""
        echo "Lists agents whose branch is clean per `_agent_branch_clean`:"
        echo "  - no commits ahead of the default base, OR"
        echo "  - was pushed to origin AND the remote branch no longer exists"
        echo "    (our org auto-deletes origin branches on merge)."
        echo ""
        echo "  -v, --verbose  — annotate each name with a reason"
        return 0
    end

    for worktree in (find $HOME/worktrees -mindepth 2 -maxdepth 2 -type d 2>/dev/null | sort)
        test -e "$worktree/.git"; or continue
        set -l branch (basename $worktree)

        set -l main_repo (git -C $worktree worktree list --porcelain 2>/dev/null | head -1 | string replace -r '^worktree ' '')
        test -z "$main_repo"; and continue

        # The branch checked out inside the worktree (may not match the
        # worktree dir name — claude picks its own kebab-case names).
        set -l in_worktree_branch (git -C $worktree symbolic-ref --quiet --short HEAD 2>/dev/null)
        set -l candidates $in_worktree_branch $branch
        set candidates (printf '%s\n' $candidates | string match -rv '^$' | sort -u)

        set -l all_clean 1
        for b in $candidates
            git -C $main_repo rev-parse --verify --quiet "refs/heads/$b" >/dev/null; or continue
            _agent_branch_clean $main_repo $b; or set all_clean 0
        end

        if test $all_clean -eq 1
            if set -q _flag_verbose
                echo "$branch  ("(string join "," $candidates)")"
            else
                echo $branch
            end
        end
    end
end
