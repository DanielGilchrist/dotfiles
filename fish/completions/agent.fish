function __agent_existing
    # Live zellij sessions
    zellij list-sessions -s 2>/dev/null
    # Existing worktree dirs (real git worktrees only)
    for d in (find $HOME/worktrees -mindepth 1 -maxdepth 2 -type d 2>/dev/null)
        if test -e "$d/.git"
            basename $d
        end
    end
end

complete -c agent -f
complete -c agent -n '__fish_is_first_token' -a '(__agent_existing | sort -u)' -d 'agent'
complete -c agent -s e -l prompt -d 'inline prompt' -r
complete -c agent -l seed -d 'prompt from file' -F
complete -c agent -l repo -d 'repo root' -x
complete -c agent -s d -l debug -d 'debug output'
complete -c agent -s h -l help -d 'show help'
