function _term_current_pane_id --description "Echo the pane id this shell is running in (empty if not in terminal)"
    if set -q WEZTERM_PANE
        echo $WEZTERM_PANE
    end
end
