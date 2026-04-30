function _term_state --description "Echo the terminal's full pane/tab/window state as JSON"
    wezterm cli list --format json 2>/dev/null
end
