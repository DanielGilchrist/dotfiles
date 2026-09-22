function _agent_focused_worktree --description "Print the worktree dir of the currently-focused pane in the agents meta-session, empty if none."
    _agent_meta_exists; or return 1

    # zellij 0.44's `list-panes --json` reports `is_focused: true` for EVERY
    # connected client's focused pane, so we'd ambiguously pick whichever
    # came first. `list-clients` instead tells us the focused pane id per
    # client. Take the first row (we only ever have one real client — the
    # wezterm pane attached to the meta-session).
    set -l client_line (zellij --session agents action list-clients 2>/dev/null | awk 'NR==2 {print $2}')
    test -z "$client_line"; and return 1

    # Strip the `terminal_` prefix; jq matches on the raw integer id.
    set -l pane_id (string replace -r '^terminal_' '' -- $client_line)
    string match -qr '^\d+$' -- $pane_id; or return 1

    set -l pane_json (zellij --session agents action list-panes --json 2>/dev/null)
    string match -qr '^[\[{]' -- $pane_json[1]; or return 1

    set -l pane (printf '%s\n' $pane_json | jq -r --argjson p $pane_id '
        .[] | select(.is_plugin | not) | select(.id == $p) | "\(.title)\t\(.pane_cwd // "")"
    ' | head -1)
    test -z "$pane"; and return 1

    set -l parts (string split \t -- $pane)
    set -l title $parts[1]
    set -l pane_cwd $parts[2]

    # The pane's title is the agent name, and the worktree on disk is the
    # source of truth. `pane_cwd` is only the shell's cwd from spawn time — it
    # goes stale if the worktree was moved or removed under it, or was spawned
    # against the wrong repo namespace before the cross-repo lookup existed.
    set -l worktree (_agent_worktree_path $title)
    if test -n "$worktree"
        echo $worktree
        return 0
    end

    if test -n "$pane_cwd" -a -d "$pane_cwd"
        echo $pane_cwd
        return 0
    end

    return 1
end
