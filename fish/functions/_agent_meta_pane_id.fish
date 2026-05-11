function _agent_meta_pane_id --description "Echo prefixed pane id (terminal_N) of meta-session pane named <branch>, empty if missing"
    set -l branch $argv[1]
    test -z "$branch"; and return 1
    _agent_meta_exists; or return 1

    # Zellij prints error strings to stdout (not stderr), which feeds garbage
    # to jq. Capture and require valid-looking JSON before piping.
    set -l out (zellij --session agents action list-panes --json 2>/dev/null)
    string match -qr '^[\[{]' -- $out[1]; or return 1

    printf '%s\n' $out | jq -r --arg n "$branch" '
        .[] | select(.is_plugin | not) | select(.title == $n) | "terminal_\(.id)"
    ' | head -1
end
