function _agent_unhide_quiet --description "Silently drop a branch (or --all) from the hidden list. No output, no meta-pane manipulation."
    set -l f (_agent_hidden_file)
    test -f "$f"; or return 0

    if test "$argv[1]" = --all
        : > $f
        return 0
    end

    set -l branch $argv[1]
    test -z "$branch"; and return 0
    set -l remaining (_agent_hidden_list | string match -v -- $branch)
    printf '%s\n' $remaining > $f
end
