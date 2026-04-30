function _term_spawn_tab --description "Spawn a new tab running <cmd> in <cwd>, optional title. Echoes new pane id."
    argparse 'title=' -- $argv
    or return

    set -l cwd $argv[1]
    set -l cmd $argv[2..-1]

    # Don't pass --cwd to wezterm — its mux canonicalizes (wez/wezterm#4618)
    # and trips on hidden dirs / firmlinks under ~/worktrees. cd inside the
    # spawned shell instead.
    set -l safe_cwd (string escape -- $cwd)
    set -l combined "cd $safe_cwd; and $cmd[1]"
    set -l pane_id (wezterm cli spawn -- fish -c $combined)
    if test -z "$pane_id"
        return 1
    end

    if set -q _flag_title
        wezterm cli set-tab-title --pane-id $pane_id $_flag_title >/dev/null 2>&1
    end

    echo $pane_id
end
