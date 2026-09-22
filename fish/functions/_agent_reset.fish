function _agent_reset --description "agent reset — kill the agents meta-session and hide every live per-agent session. Grid becomes empty; per-agent sessions and worktrees are left alone."
    argparse --name='agent reset' 'h/help' -- $argv
    or return

    if set -q _flag_help
        echo "usage: agent reset"
        echo ""
        echo "Adds every live per-agent zellij session to the hidden list, kills"
        echo "the agents meta-session, and closes its wezterm tab if present."
        echo "Per-agent sessions and worktrees stay alive — un-hide with"
        echo "\`agent attach <name>\` or bulk with \`agent restore --include-hidden\`."
        return 0
    end

    set -l f (_agent_hidden_file)
    mkdir -p (dirname $f)
    set -l live (zellij list-sessions -s 2>/dev/null | string match -v -- agents)
    set -l hidden (_agent_hidden_list)
    set -l all (printf '%s\n' $hidden $live | string match -rv '^$' | sort -u)
    printf '%s\n' $all > $f
    set -l hidden_count (count $live)

    zellij delete-session --force agents 2>/dev/null
    zellij delete-session agents 2>/dev/null

    set -l closed_pane 0
    if _term_inside
        for p in (wezterm cli list --format json 2>/dev/null | jq -r '.[] | select(.tab_title == "agents") | .pane_id')
            wezterm cli kill-pane --pane-id $p 2>/dev/null; and set closed_pane 1
        end
    end

    echo "agent reset: hid $hidden_count agent(s), killed meta-session"(test $closed_pane -eq 1; and echo ' + wezterm tab'; or echo)
end
