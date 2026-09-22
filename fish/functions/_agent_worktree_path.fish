function _agent_worktree_path --description "Echo the on-disk worktree dir for agent <name> (~/worktrees/<repo>/<name> containing .git). Empty + non-zero if none."
    set -l name $argv[1]
    test -z "$name"; and return 1

    for candidate in (find $HOME/worktrees -mindepth 2 -maxdepth 2 -name $name -type d 2>/dev/null)
        if test -e "$candidate/.git"
            echo $candidate
            return 0
        end
    end

    return 1
end
