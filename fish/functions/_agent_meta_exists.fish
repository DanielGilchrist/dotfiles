function _agent_meta_exists --description "True if the agents meta-session server is alive and responsive."
    # `list-sessions` marks sessions EXITED when no client is currently
    # attached, even if the server is running. So we probe the socket
    # directly — list-panes is a cheap, idempotent action that only succeeds
    # against a live server. Output is suppressed because zellij prints to
    # stdout on failure.
    set -l out (zellij --session agents action list-panes --json 2>/dev/null)
    test -n "$out"; and string match -qr '^\s*\[' -- $out[1]
end
