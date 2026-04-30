function _term_focus --description "Focus a pane by id"
    set -l pane_id $argv[1]
    test -z "$pane_id"; and return 1
    wezterm cli activate-pane --pane-id $pane_id >/dev/null 2>&1
end
