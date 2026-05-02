function _agent_meta_pane_count --description "Echo number of agent panes in the meta-session (0 if missing or unresponsive). Excludes plugin panes."
    set -l json (zellij --session agents action list-panes --json 2>/dev/null)
    if test -z "$json"
        echo 0
        return 0
    end
    set -l n (echo $json | jq '[.[] | select(.is_plugin | not)] | length' 2>/dev/null)
    if test -z "$n"
        echo 0
    else
        echo $n
    end
end
