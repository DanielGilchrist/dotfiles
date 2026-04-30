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
        # If attaching to an existing session from inside wezterm, ask the agents
        # tab to zoom its corresponding pane to full size so zellij's mirror
        # multi-attach renders at the new attacher's full window size instead of
        # the smaller split's size. Unzoom on detach.
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

    set -l cmd_bin $initial_cmd[1]
    set -l cmd_args $initial_cmd[2..-1]

    set -l layout_base (mktemp -t zj-layout)
    set -l layout_file "$layout_base.kdl"
    mv $layout_base $layout_file

    set -l status_bar '        pane size=1 borderless=true {
            plugin location="zellij:compact-bar"
        }
'

    if test (count $cmd_args) -eq 0
        printf 'layout {\n    tab {\n        pane command="%s"\n%s    }\n}\n' "$cmd_bin" "$status_bar" > $layout_file
    else
        set -l quoted
        for a in $cmd_args
            set -l esc (string replace -a '\\' '\\\\' -- $a | string replace -a '"' '\\"' | string replace -a \n '\\n' | string replace -a \t '\\t')
            set -a quoted "\"$esc\""
        end
        printf 'layout {\n    tab {\n        pane command="%s" {\n            args %s\n        }\n%s    }\n}\n' "$cmd_bin" (string join " " $quoted) "$status_bar" > $layout_file
    end

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
