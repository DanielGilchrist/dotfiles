function _agent_nudge_clients --description "Force a real pty resize in a per-agent session so the TUI inside (claude) re-measures after a client with a different viewport detached."
    set -l branch $argv[1]
    test -n "$branch"; or return 1
    pkill -WINCH -f "^zellij attach $branch\$" 2>/dev/null
    # Claude Code only reflows when the pty dimensions actually change, so
    # briefly split the pane and let the split close again.
    zellij --session $branch action new-pane --direction right --close-on-exit -- true >/dev/null 2>&1
    return 0
end
