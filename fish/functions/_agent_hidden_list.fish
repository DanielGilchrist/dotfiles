function _agent_hidden_list --description "Print the hidden agents, one per line. Empty output if none."
    set -l f (_agent_hidden_file)
    test -f "$f"; or return 0
    sort -u $f | string match -rv '^\s*$'
end
