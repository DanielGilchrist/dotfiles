function _agent_consolidate --description "Rebalance agent panes across meta-session tabs (≤6 per tab), splitting overcrowded tabs and packing sparse ones in pane order."
    _agent_meta_exists; or return 0

    set -l per_tab 6

    # Zellij prints errors to stdout, not stderr, so verify the captured
    # output looks like JSON before feeding jq.
    set -l json (zellij --session agents action list-panes --json 2>/dev/null)
    test -z "$json"; and return 0
    string match -qr '^[\[{]' -- $json[1]; or return 0

    # All user panes in stable order (current tab_id, then pane id).
    set -l panes (printf '%s' $json | jq -rc '
        [.[] | select(.is_plugin | not)]
        | sort_by(.tab_id, .id)
        | .[]
        | "\(.title)\t\(.tab_id)\t\(.id)\t\(.pane_cwd)"
    ')

    set -l count (count $panes)
    test $count -eq 0; and return 0

    set -l needed (math --scale=0 "($count + $per_tab - 1) / $per_tab")

    # Top up with new tabs until we have at least `needed` of them. Re-list
    # after each creation since zellij hands out the new tab_id, not us.
    set -l tab_ids (printf '%s' $json | jq -r '[.[] | .tab_id] | unique | sort | .[]')
    while test (count $tab_ids) -lt $needed
        zellij --session agents action new-tab >/dev/null 2>&1
        or break
        set tab_ids (zellij --session agents action list-panes --json 2>/dev/null \
            | jq -r '[.[] | .tab_id] | unique | sort | .[]')
    end

    # Reassign each pane to its target tab by position. Moves = new-pane in
    # target + close source; per-agent zellij session preserves claude state.
    set -l i 1
    for pane in $panes
        set -l parts (string split \t $pane)
        set -l branch $parts[1]
        set -l src_tab $parts[2]
        set -l src_id $parts[3]
        set -l src_cwd $parts[4]

        set -l target_idx (math --scale=0 "($i + $per_tab - 1) / $per_tab")
        set -l target $tab_ids[$target_idx]

        if test "$src_tab" != "$target"
            set -l safe_cwd (string escape -- $src_cwd)
            set -l pane_cmd "cd $safe_cwd; and zj $branch"
            zellij --session agents action new-pane --tab-id $target --name $branch --cwd $src_cwd -- fish -c $pane_cmd >/dev/null 2>&1
            and zellij --session agents action close-pane --pane-id terminal_$src_id 2>/dev/null
        end

        set i (math $i + 1)
    end

    # Close any tabs left empty after the shuffle.
    set -l empty_tabs (zellij --session agents action list-panes --json 2>/dev/null | jq -r '
        group_by(.tab_id)
        | map({tab_id: .[0].tab_id, count: ([.[] | select(.is_plugin | not)] | length)})
        | map(select(.count == 0))
        | .[]
        | .tab_id
    ')
    for t in $empty_tabs
        zellij --session agents action close-tab-by-id $t 2>/dev/null
    end
end
