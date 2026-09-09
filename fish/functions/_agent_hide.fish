function _agent_hide --description "agent hide — persist a branch in the hidden list; close its meta-pane if present. Session and worktree untouched."
    argparse --name='agent hide' 'h/help' 'l/list' -- $argv
    or return

    if set -q _flag_help
        echo "usage: agent hide [<branch-name>]"
        echo "       agent hide --list"
        echo ""
        echo "  no arg          — infer branch from current cwd (must be inside a worktree)"
        echo "  <branch-name>   — explicit"
        echo "  -l, --list      — print the current hidden list, one per line"
        echo ""
        echo "To un-hide: attach the agent (\`agent attach <name>\`) or rebuild the grid"
        echo "wholesale with \`agent restore --include-hidden\`."
        return 0
    end

    if set -q _flag_list
        _agent_hidden_list
        return 0
    end

    set -l branch (_agent_infer_branch $argv)
    if test -z "$branch"
        echo "agent hide: not inside a worktree under ~/worktrees/, pass branch name explicitly" >&2
        return 1
    end
    if test "$branch" = agents
        echo "agent hide: refusing to hide the meta-session itself" >&2
        return 1
    end

    set -l f (_agent_hidden_file)
    mkdir -p (dirname $f)
    touch $f
    if _agent_hidden_list | string match -q -- $branch
        echo "agent hide: $branch already hidden"
    else
        echo $branch >> $f
        echo "agent hide: hidden $branch"
    end

    if _agent_meta_exists
        set -l info (zellij --session agents action list-panes --json 2>/dev/null \
            | jq -r --arg n "$branch" '.[] | select(.is_plugin | not) | select(.title == $n) | "terminal_\(.id) \(.tab_id)"' 2>/dev/null \
            | head -1)
        if test -n "$info"
            set -l parts (string split " " $info)
            zellij --session agents action close-pane --pane-id $parts[1] 2>/dev/null
            set -l remaining (zellij --session agents action list-panes --json 2>/dev/null \
                | jq --argjson t $parts[2] '[.[] | select(.tab_id == $t and (.is_plugin | not))] | length' 2>/dev/null)
            if test "$remaining" = 0
                zellij --session agents action close-tab-by-id $parts[2] 2>/dev/null
            end
            _agent_consolidate
        end
    end
end
