function _agent_minimise_focused --description "Close the focused agent's meta-session pane only. Per-agent zellij session stays alive; bring it back with `agent --restore` or `agent <name>`."
    _agent_meta_exists; or return 1
    zellij --session agents action close-pane 2>/dev/null
    _agent_consolidate
end
