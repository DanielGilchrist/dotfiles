function _term_panes_in_tab --description "Echo count of panes in the given tab id"
    set -l tab_id $argv[1]
    test -z "$tab_id"; and echo 0; and return
    _term_state | jq --argjson t $tab_id '[.[] | select(.tab_id == $t)] | length'
end
