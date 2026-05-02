function _agent_meta_tab_pane --description "Echo the wezterm pane_id of the agents tab (empty if no such tab)"
    wezterm cli list --format json 2>/dev/null \
        | jq -r '.[] | select(.tab_title == "agents") | .pane_id' \
        | head -1
end
