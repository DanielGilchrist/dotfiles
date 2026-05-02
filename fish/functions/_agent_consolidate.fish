function _agent_consolidate --description "Pack agent panes into the fewest meta-session tabs by moving panes from later tabs into earlier tabs with room. Loops until no further moves are possible."
    while true
        set -l json (zellij --session agents action list-panes --json 2>/dev/null)
        test -z "$json"; and return 0

        # First tab (lowest tab_id) with room for one more user pane.
        set -l target (echo $json | jq -r '
            group_by(.tab_id)
            | map({tab_id: .[0].tab_id, count: ([.[] | select(.is_plugin | not)] | length)})
            | sort_by(.tab_id)
            | map(select(.count < 6))
            | first
            | .tab_id // empty
        ')
        test -z "$target"; and return 0

        # Source pane: any user pane in a tab with a higher tab_id than the
        # target. If none exists, we're already consolidated.
        set -l source (echo $json | jq -r --argjson t $target '
            [.[] | select(.is_plugin | not) | select(.tab_id > $t)]
            | sort_by(.tab_id, .id)
            | last
            | if . == null then empty else "\(.title)\t\(.tab_id)\t\(.id)\t\(.pane_cwd)" end
        ')
        test -z "$source"; and return 0

        set -l parts (string split \t $source)
        set -l branch $parts[1]
        set -l src_tab $parts[2]
        set -l src_id $parts[3]
        set -l src_cwd $parts[4]

        # Recreate in target tab. Per-agent session already exists, so `zj`
        # just attaches — Claude state is preserved across the move.
        set -l safe_cwd (string escape -- $src_cwd)
        set -l pane_cmd "cd $safe_cwd; and zj $branch"

        zellij --session agents action new-pane --tab-id $target --name $branch --cwd $src_cwd -- fish -c $pane_cmd >/dev/null 2>&1
        or return 1

        zellij --session agents action close-pane --pane-id terminal_$src_id 2>/dev/null

        # If the source tab is now empty of user panes, close it.
        set -l remaining (zellij --session agents action list-panes --json 2>/dev/null \
            | jq --argjson t $src_tab '[.[] | select(.tab_id == $t and (.is_plugin | not))] | length')
        if test "$remaining" = 0
            zellij --session agents action close-tab-by-id $src_tab 2>/dev/null
        end
    end
end
