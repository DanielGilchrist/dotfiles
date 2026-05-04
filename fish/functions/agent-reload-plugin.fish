function agent-reload-plugin --description "Rebuild and hot-reload a local zellij plugin in the agents meta-session. Per-agent state is preserved."
    set -l plugin $argv[1]
    test -z "$plugin"; and set plugin agents-bar

    set -l wasm "$HOME/.config/zellij/plugins/dist/$plugin.wasm"

    if not $HOME/.config/zellij/plugins/build.fish $plugin
        echo "agent-reload-plugin: build failed" >&2
        return 1
    end

    if not _agent_meta_exists
        echo "agent-reload-plugin: agents meta-session not running"
        return 0
    end

    zellij --session agents action start-or-reload-plugin "file:$wasm"
    echo "agent-reload-plugin: reloaded $plugin"
end
