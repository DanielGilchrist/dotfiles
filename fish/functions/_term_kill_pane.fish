function _term_kill_pane --description "Kill a pane by id"
    set -l pane_id $argv[1]
    test -z "$pane_id"; and return 1
    wezterm cli kill-pane --pane-id $pane_id >/dev/null 2>&1
end
