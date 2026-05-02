function _agent_meta_pane_id --description "Echo prefixed pane id (terminal_N) of meta-session pane named <branch>, empty if missing"
    set -l branch $argv[1]
    test -z "$branch"; and return 1
    zellij --session agents action list-panes --json 2>/dev/null \
        | jq -r --arg n "$branch" '.[] | select(.is_plugin | not) | select(.title == $n) | "terminal_\(.id)"' \
        | head -1
end
