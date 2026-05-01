function __agent_rm_existing
    zellij list-sessions -s 2>/dev/null
    for d in (find $HOME/worktrees -mindepth 1 -maxdepth 2 -type d 2>/dev/null)
        if test -e "$d/.git"
            basename $d
        end
    end
end

complete -c agent-rm -f
complete -c agent-rm -n '__fish_is_first_token' -a '(__agent_rm_existing | sort -u)' -d 'agent'
complete -c agent-rm -s a -l all -d 'remove every agent (confirms)'
complete -c agent-rm -s h -l help -d 'show help'
