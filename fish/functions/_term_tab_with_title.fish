function _term_tab_with_title --description "Echo tab id with the given title. Empty if not found."
    set -l title $argv[1]
    test -z "$title"; and return 1
    _term_state | jq -r --arg t $title '[.[] | select(.tab_title == $t)] | first | .tab_id // empty'
end
