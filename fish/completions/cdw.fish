complete -c cdw -f -a '(
    for d in $HOME/worktrees/*/*/
        string replace -- "$HOME/worktrees/" "" (string trim -r -c / -- $d)
    end
)'
