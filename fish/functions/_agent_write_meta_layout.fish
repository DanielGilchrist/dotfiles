function _agent_write_meta_layout --description "Write a meta-session bootstrap layout: ≤6-pane 'agents' tabs, each with an agents-bar plugin pane on top + a 2-column row-major grid below."
    set -l file $argv[1]
    set -l entries $argv[2..-1]

    test -z "$file"; and return 1
    test (count $entries) -eq 0; and return 1
    test (math (count $entries) % 2) -ne 0; and return 1

    set -l per_tab 6
    set -l pair_count (math (count $entries) / 2)
    set -l tab_count (math --scale=0 "($pair_count + $per_tab - 1) / $per_tab")

    set -l indicator "pane size=1 borderless=true { plugin location=\"file:$HOME/.config/zellij/plugins/dist/agents-bar.wasm\"; }"

    echo "layout {" > $file

    set -l first 1
    for t in (seq 1 $tab_count)
        set -l start (math "($t - 1) * $per_tab + 1")
        set -l end (math "$t * $per_tab")
        test $end -gt $pair_count; and set end $pair_count

        # Collect this tab's branches + commands in arrival order. The first
        # pane in the whole layout gets a one-shot toggle-pane-frames prepend
        # so the meta-session shows pane borders.
        set -l branches
        set -l cmds
        for i in (seq $start $end)
            set -l b $entries[(math "($i - 1) * 2 + 1")]
            set -l c $entries[(math "($i - 1) * 2 + 2")]
            if test $first -eq 1
                set c "zellij action toggle-pane-frames; and $c"
                set first 0
            end
            set -a branches $b
            set -a cmds $c
        end

        set -l n (count $branches)
        echo "    tab name=\"agents\" {" >> $file
        echo "        $indicator" >> $file

        if test $n -eq 1
            _agent_emit_pane $file $branches[1] $cmds[1] "        "
        else
            # Row-major 2-column grid: odd-indexed panes fill the left column,
            # even-indexed fill the right. Visually:
            #   A1 | A2
            #   A3 | A4
            #   A5 | A6
            echo "        pane split_direction=\"vertical\" {" >> $file

            set -l right_count (math --scale=0 "$n / 2")

            # Left column: panes at positions 1, 3, 5...
            echo "            pane split_direction=\"horizontal\" {" >> $file
            for i in (seq 1 2 $n)
                _agent_emit_pane $file $branches[$i] $cmds[$i] "                "
            end
            echo "            }" >> $file

            # Right column.
            if test $right_count -eq 1
                _agent_emit_pane $file $branches[2] $cmds[2] "            "
            else
                echo "            pane split_direction=\"horizontal\" {" >> $file
                for i in (seq 2 2 $n)
                    _agent_emit_pane $file $branches[$i] $cmds[$i] "                "
                end
                echo "            }" >> $file
            end

            echo "        }" >> $file
        end

        echo "    }" >> $file
    end

    # Session-wide swap layouts re-tile a tab as agents are added/removed.
    # max_panes counts ALL panes including the indicator, so values are
    # (agent_count + 1). Each template starts with the indicator pane.
    echo "    swap_tiled_layout name=\"agents-grid\" {" >> $file
    echo "        tab max_panes=2 {" >> $file
    echo "            pane" >> $file
    echo "            pane" >> $file
    echo "        }" >> $file
    echo "        tab max_panes=3 {" >> $file
    echo "            pane" >> $file
    echo "            pane split_direction=\"vertical\" {" >> $file
    echo "                pane" >> $file
    echo "                pane" >> $file
    echo "            }" >> $file
    echo "        }" >> $file
    echo "        tab max_panes=4 {" >> $file
    echo "            pane" >> $file
    echo "            pane split_direction=\"vertical\" {" >> $file
    echo "                pane split_direction=\"horizontal\" {" >> $file
    echo "                    pane" >> $file
    echo "                    pane" >> $file
    echo "                }" >> $file
    echo "                pane" >> $file
    echo "            }" >> $file
    echo "        }" >> $file
    echo "        tab max_panes=5 {" >> $file
    echo "            pane" >> $file
    echo "            pane split_direction=\"vertical\" {" >> $file
    echo "                pane split_direction=\"horizontal\" {" >> $file
    echo "                    pane" >> $file
    echo "                    pane" >> $file
    echo "                }" >> $file
    echo "                pane split_direction=\"horizontal\" {" >> $file
    echo "                    pane" >> $file
    echo "                    pane" >> $file
    echo "                }" >> $file
    echo "            }" >> $file
    echo "        }" >> $file
    echo "        tab max_panes=6 {" >> $file
    echo "            pane" >> $file
    echo "            pane split_direction=\"vertical\" {" >> $file
    echo "                pane split_direction=\"horizontal\" {" >> $file
    echo "                    pane" >> $file
    echo "                    pane" >> $file
    echo "                    pane" >> $file
    echo "                }" >> $file
    echo "                pane split_direction=\"horizontal\" {" >> $file
    echo "                    pane" >> $file
    echo "                    pane" >> $file
    echo "                }" >> $file
    echo "            }" >> $file
    echo "        }" >> $file
    echo "        tab max_panes=7 {" >> $file
    echo "            pane" >> $file
    echo "            pane split_direction=\"vertical\" {" >> $file
    echo "                pane split_direction=\"horizontal\" {" >> $file
    echo "                    pane" >> $file
    echo "                    pane" >> $file
    echo "                    pane" >> $file
    echo "                }" >> $file
    echo "                pane split_direction=\"horizontal\" {" >> $file
    echo "                    pane" >> $file
    echo "                    pane" >> $file
    echo "                    pane" >> $file
    echo "                }" >> $file
    echo "            }" >> $file
    echo "        }" >> $file
    echo "    }" >> $file
    echo "}" >> $file
end
