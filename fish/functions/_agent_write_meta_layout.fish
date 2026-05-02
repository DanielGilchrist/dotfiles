function _agent_write_meta_layout --description "Write a meta-session bootstrap layout: one tab named 'agents' with one fish pane per branch, and a 2-column swap_tiled_layout for 1-6 panes."
    set -l file $argv[1]
    set -l entries $argv[2..-1]

    test -z "$file"; and return 1
    test (count $entries) -eq 0; and return 1
    test (math (count $entries) % 2) -ne 0; and return 1

    echo "layout {" > $file
    echo "    tab name=\"agents\" {" >> $file

    set -l first 1
    for i in (seq 1 2 (count $entries))
        set -l branch $entries[$i]
        set -l cmd $entries[(math $i + 1)]
        # Prepend a one-shot toggle-pane-frames to the first pane so the
        # meta-session shows pane borders (active-pane highlight). Only the
        # first pane needs it — the toggle is session-scoped and persists.
        if test $first -eq 1
            set cmd "zellij action toggle-pane-frames; and $cmd"
            set first 0
        end
        set -l esc (string replace -a '\\' '\\\\' -- $cmd | string replace -a '"' '\\"')
        echo "        pane name=\"$branch\" command=\"fish\" {" >> $file
        echo "            args \"-c\" \"$esc\"" >> $file
        echo "        }" >> $file
    end

    echo "    }" >> $file

    # 2-column tiled grid for 1-6 panes. Zellij swaps to the matching template
    # automatically as panes are added/removed (auto_layout=true is default).
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
