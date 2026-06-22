function _agent_emit_pane --description "Write a named fish-running pane block to a meta-session layout file. Pass --focus to mark this pane as the layout's initial focus."
    argparse 'focus' -- $argv
    or return

    set -l file $argv[1]
    set -l name $argv[2]
    set -l cmd $argv[3]
    set -l indent $argv[4]
    set -l esc (string replace -a '\\' '\\\\' -- $cmd | string replace -a '"' '\\"')

    set -l attrs "name=\"$name\" command=\"fish\""
    set -q _flag_focus; and set attrs "$attrs focus=true"

    echo "$indent"pane $attrs \{ >> $file
    echo "$indent"\ \ \ \ args \"-c\" \"$esc\" >> $file
    echo "$indent"\} >> $file
end
