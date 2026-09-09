function _agent_hidden_file --description "Path to the persistent hidden-agents list."
    set -l base $XDG_STATE_HOME
    test -z "$base"; and set base $HOME/.local/state
    echo $base/agents/hidden
end
