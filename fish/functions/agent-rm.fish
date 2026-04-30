function agent-rm --description "Force-tear-down an agent: worktree + zellij session + wezterm pane"
    argparse --name=agent-rm 'h/help' 'a/all' -- $argv
    or return

    if set -q _flag_help
        echo "usage: agent-rm [<branch-name>]"
        echo "       agent-rm --all"
        echo ""
        echo "  no arg          — infer branch from current cwd (must be inside a worktree)"
        echo "  <branch-name>   — explicit"
        echo "  -a, --all       — nuke every worktree + zellij session + agents pane (confirms first)"
        echo ""
        echo "Always force-removes. Worktree, branch, zellij session, wezterm pane all go."
        return 0
    end

    if set -q _flag_all
        set -l branches
        for d in (find $HOME/worktrees -mindepth 1 -maxdepth 2 -type d 2>/dev/null)
            if test -e "$d/.git"
                set -a branches (basename $d)
            end
        end
        set -l sessions (zellij list-sessions -s 2>/dev/null)
        set -l all_names (printf '%s\n' $branches $sessions | sort -u)
        if test -z "$all_names"
            echo "agent-rm: nothing to remove"
            return 0
        end

        echo "agent-rm --all will remove:"
        for name in $all_names
            echo "  - $name"
        end
        read --prompt-str "type 'yes' to proceed: " --local confirm
        if test "$confirm" != "yes"
            echo "agent-rm: aborted"
            return 1
        end

        for name in $all_names
            agent-rm $name >/dev/null 2>&1
        end
        echo "agent-rm: removed "(count $all_names)" agents"
        return 0
    end

    set -l branch
    if test (count $argv) -gt 0
        set branch $argv[1]
    else
        set -l cwd (pwd)
        set branch (string match -r "$HOME/worktrees/[^/]+/([^/]+)" $cwd)[2]
        if test -z "$branch"
            echo "agent-rm: not inside a worktree under ~/worktrees/, pass branch name explicitly" >&2
            return 1
        end
    end

    set -l worktree_path
    for candidate in (find $HOME/worktrees -mindepth 1 -maxdepth 2 -name $branch -type d 2>/dev/null)
        if test -e "$candidate/.git"
            set worktree_path $candidate
            break
        end
    end

    if test -n "$worktree_path"
        set -l main_repo (git -C $worktree_path worktree list --porcelain 2>/dev/null | head -1 | string replace -r '^worktree ' '')
        if test -n "$main_repo" -a -d "$main_repo"
            git -C $main_repo worktree remove --force $worktree_path 2>/dev/null
            git -C $main_repo branch -D $branch 2>/dev/null
        end
        # Fallback: physically remove if anything's left.
        rm -rf $worktree_path 2>/dev/null
    end

    # Also try to delete the branch from the current repo if any (handles orphan
    # branches left behind when a previous worktree was removed manually).
    set -l current_repo (git rev-parse --show-toplevel 2>/dev/null)
    if test -n "$current_repo"
        git -C $current_repo branch -D $branch 2>/dev/null
    end

    zellij delete-session --force $branch 2>/dev/null

    if _term_inside; and test -n "$worktree_path"
        set -l pane_id (_term_pane_for_cwd $worktree_path)
        if test -n "$pane_id"
            _term_kill_pane $pane_id
        end
    end

    echo "agent-rm: removed $branch"
end
