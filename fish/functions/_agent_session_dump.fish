function _agent_session_dump --argument-names name --description "Dump the focused terminal pane of zellij session <name> to stdout (with ANSI styling preserved)."
    test -z "$name"; and return 1

    # dump-screen without --pane-id targets the *client*'s focused pane, which
    # is empty for a detached session. Find the in-session focused terminal
    # pane and target it explicitly.
    set -l json (zellij --session $name action list-panes --json 2>/dev/null)
    string match -qr '^[\[{]' -- $json[1]; or return 1

    set -l pane_id (printf '%s\n' $json | jq -r '
        ([.[] | select(.is_plugin | not) | select(.is_focused == true)] | first | .id // empty) //
        ([.[] | select(.is_plugin | not)] | first | .id // empty)
    ')
    test -z "$pane_id"; and return 1

    zellij --session $name action dump-screen --ansi --pane-id terminal_$pane_id 2>/dev/null
end
