function _agent_focused_worktree --description "Print the worktree cwd of the currently-focused pane in the agents meta-session, empty if none."
    _agent_meta_exists; or return 1

    # zellij 0.44's `list-panes --json` reports `is_focused: true` for EVERY
    # connected client's focused pane, so we'd ambiguously pick whichever
    # came first. `list-clients` instead tells us the focused pane id per
    # client. Take the first row (we only ever have one real client — the
    # wezterm pane attached to the meta-session).
    set -l client_line (zellij --session agents action list-clients 2>/dev/null | awk 'NR==2 {print $2}')
    test -z "$client_line"; and return 1

    # Strip the `terminal_` prefix; jq matches on the raw integer id.
    set -l pane_id (string replace -r '^terminal_' '' -- $client_line)

    set -l pane_json (zellij --session agents action list-panes --json 2>/dev/null)
    string match -qr '^[\[{]' -- $pane_json[1]; or return 1

    printf '%s\n' $pane_json | jq -r --argjson p $pane_id '
        .[] | select(.is_plugin | not) | select(.id == $p) | .pane_cwd
    ' | head -1
end
