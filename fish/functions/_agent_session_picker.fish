function _agent_session_picker --description "fzf-pick a per-agent zellij session that's currently NOT shown in the agents tab. Preview shows the session's current viewport."
    # `list-sessions` marks sessions EXITED when no client is currently
    # attached, even if the server is alive. Use the short-form name list
    # instead — it includes all server-running sessions regardless of
    # attach state — then drop the meta-session itself.
    set -l live (zellij list-sessions -s 2>/dev/null | string match -v -- agents)

    if test (count $live) -eq 0
        echo "agent: no per-agent sessions" >&2
        return 1
    end

    # Sessions already present as panes in the agents meta-session — exclude.
    set -l shown
    if _agent_meta_exists
        set -l json (zellij --session agents action list-panes --json 2>/dev/null)
        if string match -qr '^[\[{]' -- $json[1]
            set shown (printf '%s\n' $json | jq -r '.[] | select(.is_plugin | not) | .title')
        end
    end

    set -l candidates
    for s in $live
        if not contains -- $s $shown
            set -a candidates $s
        end
    end

    if test (count $candidates) -eq 0
        echo "agent: all sessions are already in the agents tab" >&2
        return 1
    end

    printf '%s\n' $candidates | fzf \
        --preview "fish -c '_agent_session_dump {1}'" \
        --preview-window=right:70%:wrap \
        --height=90% --reverse \
        --prompt='agent> '
end
