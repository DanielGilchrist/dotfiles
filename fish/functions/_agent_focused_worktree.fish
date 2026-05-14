function _agent_focused_worktree --description "Print the worktree cwd of the currently-focused pane in the agents meta-session, empty if none."
    _agent_meta_exists; or return 1

    # zellij's `is_focused` flag in list-panes is per-tab, so multiple panes
    # carry it. Use current-tab-info to nail down which tab the user is on,
    # then pick the focused pane within that tab.
    set -l tab_json (zellij --session agents action current-tab-info --json 2>/dev/null)
    string match -qr '^[\[{]' -- $tab_json[1]; or return 1
    set -l tab_id (printf '%s\n' $tab_json | jq -r '.tab_id // empty')
    test -z "$tab_id"; and return 1

    set -l pane_json (zellij --session agents action list-panes --json 2>/dev/null)
    string match -qr '^[\[{]' -- $pane_json[1]; or return 1

    printf '%s\n' $pane_json | jq -r --argjson t $tab_id '
        .[] | select(.is_plugin | not) | select(.tab_id == $t and .is_focused == true) | .pane_cwd
    ' | head -1
end
