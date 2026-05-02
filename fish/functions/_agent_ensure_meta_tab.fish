function _agent_ensure_meta_tab --description "Ensure a wezterm 'agents' tab exists, attached to the meta-session. Echoes its pane id."
    set -l existing (_agent_meta_tab_pane)
    if test -n "$existing"
        echo $existing
        return 0
    end

    set -l new_pane (_term_spawn_tab --title agents $HOME "zellij attach agents")
    test -z "$new_pane"; and return 1

    _term_emit_event agents-tab-spawned $new_pane
    echo $new_pane
end
