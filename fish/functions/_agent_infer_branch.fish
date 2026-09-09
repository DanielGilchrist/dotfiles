function _agent_infer_branch --description "Infer an agent branch name: arg if given, else from cwd (~/worktrees/<repo>/<branch>)."
    if test (count $argv) -gt 0
        echo $argv[1]
        return 0
    end
    set -l cwd (pwd)
    set -l branch (string match -r "$HOME/worktrees/[^/]+/([^/]+)" $cwd)[2]
    if test -z "$branch"
        return 1
    end
    echo $branch
end
