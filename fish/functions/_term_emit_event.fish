function _term_emit_event --description "Emit a named event to the terminal config (wezterm: SetUserVar OSC). Triggers a `user-var-changed` lua event."
    set -l name $argv[1]
    set -l value $argv[2]
    test -z "$name"; and return 1
    set -l b64 (printf '%s' "$value" | base64)
    printf '\033]1337;SetUserVar=%s=%s\007' "$name" "$b64" > /dev/tty
end
