function _term_agents_tab_id --description "Echo the tab_id of the agents tab, or empty if missing/stale."
    set -l marker /tmp/wezterm-agents-tab
    test -f $marker; or return 0

    set -l id (cat $marker 2>/dev/null | string trim)
    test -z "$id"; and return 0

    # Validate it still exists in the live mux state.
    set -l live (_term_state | jq -r --arg id $id '[.[] | select(.tab_id == ($id|tonumber))] | first | .tab_id // empty')
    test -z "$live"; and return 0

    echo $id
end
