function _agent_emit_pane --description "Write a named fish-running pane block to a meta-session layout file"
    set -l file $argv[1]
    set -l name $argv[2]
    set -l cmd $argv[3]
    set -l indent $argv[4]
    set -l esc (string replace -a '\\' '\\\\' -- $cmd | string replace -a '"' '\\"')
    echo "$indent"pane name=\"$name\" command=\"fish\" \{ >> $file
    echo "$indent"\ \ \ \ args \"-c\" \"$esc\" >> $file
    echo "$indent"\} >> $file
end
