function _agent_inject_prompt --description "Inject a prompt into a running claude in zellij session <name>"
    set -l name $argv[1]
    set -l seed_file $argv[2]

    set -l contents (cat $seed_file)
    test -z "$contents"; and return

    zellij --session $name action write-chars "$contents" >/dev/null 2>&1
    zellij --session $name action write 13 >/dev/null 2>&1
end
