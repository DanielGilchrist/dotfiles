function _agent_write_meta_layout --description "Write a meta-session bootstrap layout: ≤6-pane 'agents' tabs, arranged as a 2-column row-major grid."
    set -l file $argv[1]
    set -l entries $argv[2..-1]

    test -z "$file"; and return 1
    test (count $entries) -eq 0; and return 1
    test (math (count $entries) % 2) -ne 0; and return 1

    set -l per_tab 6
    set -l pair_count (math (count $entries) / 2)
    set -l tab_count (math --scale=0 "($pair_count + $per_tab - 1) / $per_tab")

    echo "layout {" > $file

    set -l first_agent_emitted 0

    for t in (seq 1 $tab_count)
        set -l start (math "($t - 1) * $per_tab + 1")
        set -l end (math "$t * $per_tab")
        test $end -gt $pair_count; and set end $pair_count

        set -l branches
        set -l cmds
        for i in (seq $start $end)
            set -l b $entries[(math "($i - 1) * 2 + 1")]
            set -l c $entries[(math "($i - 1) * 2 + 2")]
            set -a branches $b
            set -a cmds $c
        end

        set -l n (count $branches)
        echo "    tab name=\"agents\" {" >> $file

        if test $n -eq 1
            if test $first_agent_emitted -eq 0
                _agent_emit_pane --focus $file $branches[1] $cmds[1] "        "
                set first_agent_emitted 1
            else
                _agent_emit_pane $file $branches[1] $cmds[1] "        "
            end
        else
            # Row-major 2-column grid: odd-indexed panes fill the left column,
            # even-indexed fill the right. Visually:
            #   A1 | A2
            #   A3 | A4
            #   A5 | A6
            echo "        pane split_direction=\"vertical\" {" >> $file

            set -l right_count (math --scale=0 "$n / 2")

            echo "            pane split_direction=\"horizontal\" {" >> $file
            for i in (seq 1 2 $n)
                if test $first_agent_emitted -eq 0
                    _agent_emit_pane --focus $file $branches[$i] $cmds[$i] "                "
                    set first_agent_emitted 1
                else
                    _agent_emit_pane $file $branches[$i] $cmds[$i] "                "
                end
            end
            echo "            }" >> $file

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
    echo "    swap_tiled_layout name=\"agents-grid\" {" >> $file
    echo "        tab max_panes=1 {" >> $file
    echo "            pane" >> $file
    echo "        }" >> $file
    echo "        tab max_panes=2 {" >> $file
    echo "            pane split_direction=\"vertical\" {" >> $file
    echo "                pane" >> $file
    echo "                pane" >> $file
    echo "            }" >> $file
    echo "        }" >> $file
    echo "        tab max_panes=3 {" >> $file
    echo "            pane split_direction=\"vertical\" {" >> $file
    echo "                pane split_direction=\"horizontal\" {" >> $file
    echo "                    pane" >> $file
    echo "                    pane" >> $file
    echo "                }" >> $file
    echo "                pane" >> $file
    echo "            }" >> $file
    echo "        }" >> $file
    echo "        tab max_panes=4 {" >> $file
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
    echo "        tab max_panes=5 {" >> $file
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
    echo "        tab max_panes=6 {" >> $file
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
