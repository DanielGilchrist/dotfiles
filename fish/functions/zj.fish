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

    # A session that died (reboot, server crash, force close) is listed as
    # EXITED and `attach` resurrects its old pane commands. When the caller
    # supplied a command, start fresh with that instead of the stale one.
    if zellij list-sessions --no-formatting 2>/dev/null | string match -qr -- "^$name \\[.*EXITED"
        and test (count $initial_cmd) -gt 0
        zellij delete-session $name >/dev/null 2>&1
    else if zellij list-sessions -s 2>/dev/null | string match -q -- $name
        zellij attach $name
        set -l exit_status $status
        # Zellij sizes a session to its smallest client and doesn't always
        # re-fit when that client leaves. TUIs like Claude Code only reflow
        # when the pty size really changes, so briefly split the pane.
        pkill -WINCH -f "^zellij attach $name\$" 2>/dev/null
        zellij --session $name action new-pane --direction right --close-on-exit -- true >/dev/null 2>&1
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
