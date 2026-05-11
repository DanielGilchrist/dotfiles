function _agent_meta_exists --description "True if the agents meta-session is alive (not EXITED/resurrection-candidate)"
    # `list-sessions -s` includes resurrection-candidate names and would lie.
    # Action probes (query-tab-names etc.) tend to auto-resurrect dead sessions
    # which both lies AND has side effects. Parse the full list-sessions output
    # and accept only a row that doesn't have the EXITED marker. sed strips
    # ANSI colour codes that zellij emits.
    zellij list-sessions 2>/dev/null \
        | sed 's/\x1b\[[0-9;]*m//g' \
        | awk '/^agents / && !/EXITED/ { found = 1 } END { exit !found }'
end
