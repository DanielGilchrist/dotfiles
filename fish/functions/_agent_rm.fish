function _agent_rm --description "agent rm — tear down an agent (worktree + zellij session + meta-pane). Refuses on dirty branches unless --force."
    argparse --name='agent rm' 'h/help' 'a/all' 'f/force' -- $argv
    or return

    if set -q _flag_help
        echo "usage: agent rm [-f] [<branch-name>]"
        echo "       agent rm --all [-f]"
        echo ""
        echo "  no arg          — infer branch from current cwd (must be inside a worktree)"
        echo "  <branch-name>   — explicit"
        echo "  -a, --all       — every agent (confirms first)"
        echo "  -f, --force     — tear down even if the branch has unpushed/unmerged commits"
        return 0
    end

    if set -q _flag_all
        set -l all_names
        for d in (find $HOME/worktrees -mindepth 1 -maxdepth 2 -type d 2>/dev/null)
            if test -e "$d/.git"
                set -a all_names (basename $d)
            end
        end
        set all_names (printf '%s\n' $all_names | sort -u)
        if test -z "$all_names"
            echo "agent rm: nothing to remove"
            return 0
        end

        echo "agent rm --all will remove:"
        for name in $all_names
            echo "  - $name"
        end
        read --prompt-str "type 'yes' to proceed: " --local confirm
        if test "$confirm" != "yes"
            echo "agent rm: aborted"
            return 1
        end

        set -l forward_flags
        set -q _flag_force; and set -a forward_flags --force
        set -l removed 0
        set -l skipped 0
        for name in $all_names
            if _agent_rm $forward_flags $name >/dev/null
                set removed (math $removed + 1)
            else
                set skipped (math $skipped + 1)
            end
        end
        _agent_unhide_quiet --all
        zellij delete-session --force agents 2>/dev/null
        if _term_inside
            for p in (wezterm cli list --format json 2>/dev/null | jq -r '.[] | select(.tab_title == "agents") | .pane_id')
                wezterm cli kill-pane --pane-id $p 2>/dev/null
            end
        end
        if test $skipped -gt 0
            echo "agent rm: removed $removed agent(s), skipped $skipped (see --force)"
        else
            echo "agent rm: removed $removed agent(s)"
        end
        return 0
    end

    set -l branch
    if test (count $argv) -gt 0
        set branch $argv[1]
    else
        set -l cwd (pwd)
        set branch (string match -r "$HOME/worktrees/[^/]+/([^/]+)" $cwd)[2]
        if test -z "$branch"
            echo "agent rm: not inside a worktree under ~/worktrees/, pass branch name explicitly" >&2
            return 1
        end
    end

    if test "$branch" = agents
        echo "agent rm: refusing to remove the meta-session itself" >&2
        return 1
    end

    set -l worktree_path
    for candidate in (find $HOME/worktrees -mindepth 1 -maxdepth 2 -name $branch -type d 2>/dev/null)
        if test -e "$candidate/.git"
            set worktree_path $candidate
            break
        end
    end

    set -l candidate_branches
    set -l main_repo
    if test -n "$worktree_path"
        set main_repo (git -C $worktree_path worktree list --porcelain 2>/dev/null | head -1 | string replace -r '^worktree ' '')
        set -l in_worktree_branch (git -C $worktree_path symbolic-ref --quiet --short HEAD 2>/dev/null)
        test -n "$in_worktree_branch"; and set -a candidate_branches $in_worktree_branch
    end
    set -a candidate_branches $branch

    set -l target_repo $main_repo
    test -z "$target_repo"; and set target_repo (git rev-parse --show-toplevel 2>/dev/null)

    if not set -q _flag_force; and test -n "$target_repo"
        set -l dirty
        for b in $candidate_branches
            git -C $target_repo rev-parse --verify --quiet "refs/heads/$b" >/dev/null
            or continue
            _agent_branch_clean $target_repo $b; and continue
            set -a dirty $b
        end
        if test (count $dirty) -gt 0
            echo "agent rm: refusing to remove $branch — branch(es) with unpushed/unmerged commits: "(string join ", " $dirty)" — re-run with --force to discard" >&2
            return 1
        end
    end

    zellij delete-session --force $branch 2>/dev/null
    zellij delete-session $branch 2>/dev/null

    if test -n "$worktree_path"
        if test -n "$main_repo" -a -d "$main_repo"
            git -C $main_repo worktree remove --force $worktree_path 2>/dev/null
        end
        rm -rf $worktree_path 2>/dev/null
    end

    if test -n "$target_repo"
        for b in $candidate_branches
            git -C $target_repo rev-parse --verify --quiet "refs/heads/$b" >/dev/null
            or continue
            git -C $target_repo branch -D $b 2>/dev/null
        end
    end

    if _agent_meta_exists
        set -l info (zellij --session agents action list-panes --json 2>/dev/null \
            | jq -r --arg n "$branch" '.[] | select(.is_plugin | not) | select(.title == $n) | "terminal_\(.id) \(.tab_id)"' 2>/dev/null \
            | head -1)
        if test -n "$info"
            set -l parts (string split " " $info)
            zellij --session agents action close-pane --pane-id $parts[1] 2>/dev/null
            set -l remaining (zellij --session agents action list-panes --json 2>/dev/null \
                | jq --argjson t $parts[2] '[.[] | select(.tab_id == $t and (.is_plugin | not))] | length' 2>/dev/null)
            if test "$remaining" = 0
                zellij --session agents action close-tab-by-id $parts[2] 2>/dev/null
            end
        end
        _agent_consolidate
    end

    rm -f /tmp/agent-state-$branch 2>/dev/null
    _agent_unhide_quiet $branch

    echo "agent rm: removed $branch"
end
