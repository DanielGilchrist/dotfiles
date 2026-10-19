function _term_emit_event --description "Emit a named event to the terminal config (wezterm: SetUserVar OSC). Triggers a `user-var-changed` lua event."
    set -l name $argv[1]
    set -l value $argv[2]
    test -z "$name"; and return 1
    set -l b64 (printf '%s' "$value" | base64)
    set -l osc (printf '\033]1337;SetUserVar=%s=%s\007' "$name" "$b64")

    # Via sh: fish prints its own warning when a redirect target can't be
    # opened, which 2>/dev/null on the redirect itself doesn't silence.
    if sh -c 'printf "%s" "$1" > /dev/tty' _ $osc 2>/dev/null
        return 0
    end

    # No controlling tty (run from a background command or an agent's tool
    # shell). The calling pane's pty still exists, and writing to its slave
    # device is output to the terminal, so wezterm still parses the OSC.
    set -q WEZTERM_PANE; or return 1
    set -l tty_name (wezterm cli list --format json 2>/dev/null | jq -r --argjson id $WEZTERM_PANE '.[] | select(.pane_id == $id) | .tty_name')
    test -n "$tty_name" -a "$tty_name" != null; or return 1
    printf '%s' $osc > $tty_name
end
