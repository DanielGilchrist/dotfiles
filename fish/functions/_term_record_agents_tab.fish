function _term_record_agents_tab --description "Persist the agents tab id, derived from a pane in that tab."
    set -l pane_id $argv[1]
    test -z "$pane_id"; and return 1

    set -l tab_id (_term_state | jq -r --arg p $pane_id '[.[] | select(.pane_id == ($p|tonumber))] | first | .tab_id // empty')
    test -z "$tab_id"; and return 1

    # Disk: cold-start fallback for both fish (subsequent agent invocations)
    # and lua (when wezterm.GLOBAL is empty after a wezterm restart).
    echo $tab_id > /tmp/wezterm-agents-tab

    # Event: pushes the id straight into wezterm.GLOBAL so the lua side
    # never has to hit disk on hot paths (tab-bar render, key events, etc).
    _term_emit_event agents-tab-id $tab_id
end
