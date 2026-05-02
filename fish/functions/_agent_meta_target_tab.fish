function _agent_meta_target_tab --description "Echo tab_id of first meta-session tab with <6 user panes. Empty if all full or meta missing."
    set -l json (zellij --session agents action list-panes --json 2>/dev/null)
    test -z "$json"; and return 1

    echo $json | jq -r '
        group_by(.tab_id)
        | map({tab_id: .[0].tab_id, count: ([.[] | select(.is_plugin | not)] | length)})
        | sort_by(.tab_id)
        | map(select(.count < 6))
        | first
        | .tab_id // empty
    '
end
