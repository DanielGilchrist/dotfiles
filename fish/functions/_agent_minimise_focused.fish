function _agent_minimise_focused --description "Hide the focused agent: close its meta-session pane and persist it in the hidden list so `agent restore` leaves it out. Per-agent zellij session stays alive; bring it back with `agent attach <name>`."
    _agent_meta_exists; or return 1
    set -l worktree (_agent_focused_worktree)
    if test -z "$worktree"
        zellij --session agents action close-pane 2>/dev/null
        _agent_consolidate
        return 0
    end
    _agent_hide (basename $worktree)
end
