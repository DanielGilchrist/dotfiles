function _agent_focused_worktree --description "Print the worktree dir of the currently-focused pane in the agents meta-session, empty if none."
    _agent_meta_exists; or return 1

    set -l pane (_agent_focused_pane); or return 1

    set -l parts (string split \t -- $pane)
    set -l title $parts[1]
    set -l pane_cwd $parts[2]

    # The pane's title is the agent name, and the worktree on disk is the
    # source of truth. `pane_cwd` is only the shell's cwd from spawn time — it
    # goes stale if the worktree was moved or removed under it, or was spawned
    # against the wrong repo namespace before the cross-repo lookup existed.
    set -l worktree (_agent_worktree_path $title)
    if test -n "$worktree"
        echo $worktree
        return 0
    end

    if test -n "$pane_cwd" -a -d "$pane_cwd"
        echo $pane_cwd
        return 0
    end

    return 1
end
