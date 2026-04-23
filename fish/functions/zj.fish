function zj --description "Start or attach to a zellij session in the current dir"
    argparse --name=zj 'h/help' -- $argv
    or return

    if set -q _flag_help
        echo "usage: zj [<name>]"
        echo ""
        echo "  no name  — list active sessions"
        echo "  <name>   — attach to session <name>, or create it in the current dir"
        return 0
    end

    set -l name $argv[1]
    if test -z "$name"
        zellij list-sessions
        return
    end

    zellij attach --create $name
end
