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
        # External-attach auto-fullscreen: when `zj a8` is run from outside the
        # meta-session (e.g. another wezterm tab), fullscreen the corresponding
        # meta-pane so its dimensions don't constrain the new attacher's view.
        # Skip when invoked from inside the meta-session itself — that's the
        # meta-pane attaching to its own per-agent session, no fullscreen wanted.
        set -l meta_pane
        if test "$name" != agents
            and test "$ZELLIJ_SESSION_NAME" != agents
            and _agent_meta_exists
            set meta_pane (_agent_meta_pane_id $name)
            if test -n "$meta_pane"
                zellij --session agents action toggle-fullscreen --pane-id $meta_pane 2>/dev/null
            end
        end

        zellij attach $name
        set -l exit_status $status

        if test -n "$meta_pane"
            zellij --session agents action toggle-fullscreen --pane-id $meta_pane 2>/dev/null
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
