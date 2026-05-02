function _agent_meta_exists --description "True if the agents meta-session is alive"
    zellij list-sessions -s 2>/dev/null | string match -q -- agents
end
