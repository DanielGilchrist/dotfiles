function _agent_session_cwd --description 'Print the cwd of the first terminal pane in per-agent session <name>, empty if the session is not live.'
    set -l name $argv[1]
    test -z "$name"; and return 1

    set -l cwd (zellij --session $name action list-panes --json 2>/dev/null \
        | jq -r '[.[] | select(.is_plugin | not) | .pane_cwd // empty][0] // empty' 2>/dev/null)
    test -n "$cwd"; or return 1
    echo $cwd
end
