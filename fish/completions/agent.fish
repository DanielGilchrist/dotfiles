function __agent_existing
    zellij list-sessions -s 2>/dev/null | string match -v -- agents
    for d in (find $HOME/worktrees -mindepth 2 -maxdepth 2 -type d 2>/dev/null)
        test -e "$d/.git"; and basename $d
    end
end

function __agent_second_token_is
    set -l tokens (commandline -opc)
    test (count $tokens) -ge 2; and test "$tokens[2]" = $argv[1]
end

set -l subs ls attach checkout co rm hide restore reset merged help

complete -c agent -f

complete -c agent -n '__fish_is_first_token' -a "$subs" -d subcommand

for sub in attach rm hide
    complete -c agent -n "__agent_second_token_is $sub" -a '(__agent_existing | sort -u)' -d agent
end

complete -c agent -n '__agent_second_token_is attach' -s e -l prompt -d 'inline prompt' -r
complete -c agent -n '__agent_second_token_is attach' -l seed -d 'prompt from file' -F
complete -c agent -n '__agent_second_token_is attach' -l repo -d 'repo root' -x
complete -c agent -n '__agent_second_token_is attach' -l no-focus -d "don't refocus calling pane"
complete -c agent -n '__agent_second_token_is attach' -l no-prompt -d "skip nvim seed editor on fresh spawn"

function __agent_branches
    git for-each-ref --format='%(refname:short)' refs/heads refs/remotes/origin 2>/dev/null \
        | string replace -r '^origin/' '' | sort -u
end
for sub in checkout co
    complete -c agent -n "__agent_second_token_is $sub" -a '(__agent_branches)' -d branch
end
complete -c agent -n '__agent_second_token_is attach' -l headless -d 'no meta-pane (nvim caller)'
complete -c agent -n '__agent_second_token_is attach' -s d -l debug -d 'debug output'

complete -c agent -n '__agent_second_token_is rm' -s a -l all -d 'every agent (confirms)'
complete -c agent -n '__agent_second_token_is rm' -s f -l force -d 'discard unpushed/unmerged commits'

complete -c agent -n '__agent_second_token_is hide' -s l -l list -d 'print hidden list'

complete -c agent -n '__agent_second_token_is restore' -l include-hidden -d 'clear hidden list first'

complete -c agent -n '__agent_second_token_is merged' -s v -l verbose -d 'annotate with reason'

complete -c agent -n '__agent_second_token_is help' -f
