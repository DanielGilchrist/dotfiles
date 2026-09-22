function agent --description "Parallel Claude agents in worktrees. Dispatcher for subcommands (attach/rm/hide/restore/merged/reload-plugin/ls)."
    if test (count $argv) -eq 0
        _agent_ls
        return $status
    end

    set -l sub $argv[1]
    set -l rest $argv[2..-1]

    switch $sub
        case ls list
            _agent_ls $rest
        case attach
            _agent_attach $rest
        case checkout co
            _agent_checkout $rest
        case rm
            _agent_rm $rest
        case hide
            _agent_hide $rest
        case restore
            _agent_restore $rest
        case merged
            _agent_merged $rest
        case reload-plugin
            _agent_reload_plugin $rest
        case help -h --help
            _agent_help
        case '*'
            echo "agent: unknown subcommand '$sub'" >&2
            echo "" >&2
            _agent_help >&2
            return 1
    end
end
