function _term_pane_for_cwd --description "Echo pane id whose cwd matches the given path (anywhere). Empty if none."
    set -l cwd $argv[1]
    test -z "$cwd"; and return 1
    _term_state | jq -r --arg p $cwd '.[] | select(.cwd | test("file://[^/]*" + $p + "/?$")) | .pane_id' | head -1
end
