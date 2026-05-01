function zj --description "Start or attach to a zellij session in the current dir"
    set -l user_argv $argv
    set -l initial_cmd
    set -l dash_idx (contains -i -- -- $user_argv)
    if test -n "$dash_idx"
        set initial_cmd $user_argv[(math $dash_idx + 1)..-1]
        if test $dash_idx -gt 1
            set user_argv $user_argv[1..(math $dash_idx - 1)]
        else
            set user_argv
        end
    end

    argparse --name=zj 'h/help' 'd/debug' -- $user_argv
    or return

    if set -q _flag_help
        echo "usage: zj [-d] [<name>] [-- <cmd> [args...]]"
        echo ""
        echo "  no name              — list active sessions"
        echo "  <name>               — attach to session <name>, or create it in the current dir"
        echo "  <name> -- <cmd> ...  — same, but if creating, run <cmd> as the first pane"
        echo "  -d, --debug          — print the generated layout before launching"
        return 0
    end

    set -l name $argv[1]
    if test -z "$name"
        zellij list-sessions
        return
    end

    if zellij list-sessions -s 2>/dev/null | string match -q -- $name
        # If attaching from inside wezterm, ask the agents tab to zoom its
        # corresponding pane to full size so zellij's mirror multi-attach
        # renders at this attacher's full window size. Unzoom on detach.
        set -l zoom 0
        if functions -q _term_inside; and _term_inside; and functions -q _term_emit_event
            set zoom 1
            _term_emit_event agent-zoom $name
        end
        zellij attach $name
        set -l exit_status $status
        if test $zoom -eq 1
            _term_emit_event agent-unzoom $name
        end
        return $exit_status
    end

    if test (count $initial_cmd) -eq 0
        zellij attach --create $name
        return
    end

    set -l layout_file /tmp/zj-(random).kdl
    _zj_write_layout $layout_file $initial_cmd

    if set -q _flag_debug
        echo "--- layout file: $layout_file ---" >&2
        cat $layout_file >&2
        echo "--- launching: zellij -s $name -n $layout_file ---" >&2
    end

    zellij -s $name -n $layout_file
    set -l exit_status $status
    rm -f $layout_file
    return $exit_status
end
