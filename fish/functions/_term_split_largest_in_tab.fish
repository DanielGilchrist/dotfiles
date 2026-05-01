function _term_split_largest_in_tab --description "Add a pane to <tab_id> via 2-column balanced grid (left first, then right, fill rows top-to-bottom)."
    set -l tab_id $argv[1]
    set -l cwd $argv[2]
    set -l cmd $argv[3]

    test -z "$tab_id"; and return 1

    set -l state (_term_state | jq -c --argjson t $tab_id '[.[] | select(.tab_id == $t)]')
    set -l count (echo $state | jq 'length')

    if test $count -eq 0
        return 1
    end

    # Belt-and-braces: pass --cwd AND prepend `cd` inside the script. Wezterm
    # cli's --cwd has known bugs (e.g. wez/wezterm#4121) where it can be
    # silently dropped, leaving cwd at HOME. The cd inside guarantees it.
    set -l safe_cwd (string escape -- $cwd)
    set -l combined "cd $safe_cwd; and $cmd"

    if test $count -eq 1
        set -l target (echo $state | jq -r '.[0] | .pane_id')
        wezterm cli split-pane --pane-id $target --right --percent 50 -- fish -c $combined
        return $status
    end

    set -l min_x (echo $state | jq '[.[] | .left_col] | min')
    set -l max_x (echo $state | jq '[.[] | .left_col] | max')

    if test $min_x = $max_x
        # Only a single column exists — fall back to splitting right of the bottom pane.
        set -l target (echo $state | jq -r 'sort_by(.top_row) | last | .pane_id')
        wezterm cli split-pane --pane-id $target --right --percent 50 -- fish -c $combined
        return $status
    end

    set -l left_count (echo $state | jq --argjson x $min_x '[.[] | select(.left_col == $x)] | length')
    set -l right_count (echo $state | jq --argjson x $max_x '[.[] | select(.left_col == $x)] | length')

    set -l target_pane
    if test $left_count -le $right_count
        set target_pane (echo $state | jq -r --argjson x $min_x '[.[] | select(.left_col == $x)] | sort_by(.top_row) | last | .pane_id')
    else
        set target_pane (echo $state | jq -r --argjson x $max_x '[.[] | select(.left_col == $x)] | sort_by(.top_row) | last | .pane_id')
    end

    wezterm cli split-pane --pane-id $target_pane --bottom --percent 50 -- fish -c $combined
end
