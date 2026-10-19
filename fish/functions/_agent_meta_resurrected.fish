function _agent_meta_resurrected --description "True if the agents meta-session is a resurrected stub grid (panes held at 'Waiting to run')."
    zellij --session agents action list-panes --json 2>/dev/null \
        | jq -e '[.[] | select(.is_plugin | not)] | any(.is_held)' >/dev/null 2>&1
end
