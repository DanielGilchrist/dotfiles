function _agent_ls --description "agent ls — list agents (live per-agent zellij sessions) with worktree, meta-pane, and hidden state."
    argparse --name='agent ls' 'h/help' -- $argv
    or return

    if set -q _flag_help
        echo "usage: agent ls"
        echo ""
        echo "Columns:"
        echo "  NAME     zellij session name (= branch = worktree dir)"
        echo "  STATE    live · hidden · orphan (worktree exists, session gone)"
        echo "  META     'yes' if a pane is up in the agents meta-session"
        echo "  WORKTREE path under ~/worktrees/<repo>/<name>, or '-' if missing"
        return 0
    end

    set -l live (zellij list-sessions -s 2>/dev/null | string match -v -- agents)
    set -l hidden (_agent_hidden_list)
    set -l meta_up 0
    _agent_meta_exists; and set meta_up 1

    set -l worktree_names
    for d in (find $HOME/worktrees -mindepth 2 -maxdepth 2 -type d 2>/dev/null)
        test -e "$d/.git"; or continue
        set -a worktree_names (basename $d)
    end

    set -l all (printf '%s\n' $live $worktree_names | sort -u | string match -rv '^$')
    if test (count $all) -eq 0
        echo "no agents"
        return 0
    end

    set -l rows
    set -a rows "NAME|STATE|META|WORKTREE"
    for name in $all
        set -l state live
        contains -- $name $live; or set state orphan
        contains -- $name $hidden; and set state hidden

        set -l meta -
        if test $meta_up -eq 1
            _agent_meta_pane_id $name >/dev/null 2>&1; and set meta yes; or set meta no
        end

        set -l wp (find $HOME/worktrees -mindepth 2 -maxdepth 2 -name $name -type d 2>/dev/null | head -1)
        test -z "$wp"; and set wp -
        set wp (string replace $HOME '~' -- $wp)

        set -a rows "$name|$state|$meta|$wp"
    end

    printf '%s\n' $rows | column -t -s '|'
end
