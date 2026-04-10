function dft --description "Git diff with difftastic"
    set -l difftool_cmd git difftool --no-symlinks

    switch (count $argv)
        case 0
            # Working tree changes (unstaged)
            $difftool_cmd
        case '*'
            $difftool_cmd $argv
    end
end
