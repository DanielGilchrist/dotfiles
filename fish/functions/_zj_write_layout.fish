function _zj_write_layout --description "Write a zellij layout file: one command pane (with optional args)."
    set -l file $argv[1]
    set -l cmd_bin $argv[2]
    set -l cmd_args $argv[3..-1]

    set -l pane_block "        pane command=\"$cmd_bin\""
    if test (count $cmd_args) -gt 0
        set -l quoted
        for a in $cmd_args
            set -l esc (string replace -a '\\' '\\\\' -- $a | string replace -a '"' '\\"' | string replace -a \n '\\n' | string replace -a \t '\\t')
            set -a quoted "\"$esc\""
        end
        set pane_block "        pane command=\"$cmd_bin\" {
            args "(string join ' ' $quoted)"
        }"
    end

    echo "layout {
    tab {
$pane_block
        pane size=1 borderless=true {
            plugin location=\"zellij:compact-bar\"
        }
    }
}" > $file
end
