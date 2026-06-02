function agent --description "Spawn a Claude agent in a worktree, as a pane in the agents meta-session"
    argparse --name=agent 'h/help' 'd/debug' 'e/prompt=' 'seed=' 'repo=' 'no-focus' 'restore' -- $argv
    or return

    if set -q _flag_help
        echo "usage: agent <name> [-e <prompt> | --seed <file>] [--repo <path>]"
        echo "       agent --restore"
        echo ""
        echo "  <name>               — required. Used as worktree dir, git branch, and zellij session name."
        echo "  -e, --prompt <text>  — inline prompt seeded into Claude as the first message"
        echo "  --seed <file>        — multi-line prompt from a file (same as -e)"
        echo "  --repo <path>        — repo root (defaults to git rev-parse from cwd)"
        echo "  --restore            — rebuild the agents grid from live per-agent sessions"
        echo "  --no-focus           — don't refocus the calling pane after spawn"
        echo "  -d, --debug          — print spawn commands to stderr"
        return 0
    end

    if set -q _flag_restore
        _agent_restore
        return $status
    end

    if test (count $argv) -lt 1
        echo "agent: <name> is required (e.g. \`agent payroll-fix\`)" >&2
        echo "       see \`agent --help\` for full usage" >&2
        return 1
    end

    set -l branch $argv[1]

    if test "$branch" = agents
        echo "agent: 'agents' is reserved (used for the meta-session)" >&2
        return 1
    end

    # Zellij session-name length is capped by the macOS unix-socket budget (~25 chars).
    if test (string length -- $branch) -gt 25
        echo "agent: name too long ("(string length -- $branch)" chars); zellij caps session names around 25 chars on macOS." >&2
        return 1
    end

    if test -n "$_flag_prompt" -a -n "$_flag_seed"
        echo "agent: pass either -e or --seed, not both" >&2
        return 1
    end

    set -l repo_root $_flag_repo
    if test -z "$repo_root"
        # Resolve to the MAIN worktree, not whichever worktree we're currently
        # inside — otherwise spawning from one agent's worktree creates nested
        # worktrees under ~/worktrees/<branch-name>/<new-branch>.
        set repo_root (git worktree list --porcelain 2>/dev/null | head -1 | string replace -r '^worktree ' '')
    end
    if test -z "$repo_root"
        echo "agent: not in a git repo and --repo not given" >&2
        return 1
    end
    set -l repo_name (basename $repo_root)

    set -l worktrees_dir "$HOME/worktrees/$repo_name"
    set -l worktree_path "$worktrees_dir/$branch"

    set -l worktree_exists 0
    test -d "$worktree_path"; and set worktree_exists 1

    set -l session_exists 0
    zellij list-sessions -s 2>/dev/null | string match -q -- $branch; and set session_exists 1

    set -q _flag_debug; and echo "[debug] worktree_exists=$worktree_exists session_exists=$session_exists" >&2

    # Seed handling. -e/--seed always honoured. If neither AND it's a fresh
    # spawn, open nvim for a multi-line prompt. Reconnect cases skip the editor.
    if test -n "$_flag_prompt"
        set _flag_seed (mktemp -t agent-prompt)
        printf "%s" $_flag_prompt > $_flag_seed
    else if test -z "$_flag_seed" -a $worktree_exists -eq 0 -a $session_exists -eq 0; and status --is-interactive
        set -l editor_path /tmp/agent-prompt-$branch-(random).md
        touch $editor_path
        nvim $editor_path
        if test -s $editor_path
            set _flag_seed $editor_path
        else
            rm -f $editor_path
            echo "agent: cancelled (empty prompt)"
            return 0
        end
    end

    if test $worktree_exists -eq 0
        mkdir -p $worktrees_dir
        git -C $repo_root worktree prune 2>/dev/null

        set -l base (git -C $repo_root symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)
        if test -z "$base"
            for candidate in main master
                if git -C $repo_root rev-parse --verify --quiet "origin/$candidate" >/dev/null
                    set base "origin/$candidate"
                    break
                end
            end
        end
        set -q _flag_debug; and echo "[debug] base ref: $base" >&2

        set -l err_log
        if set -q _flag_debug
            set err_log (mktemp -t agent-git-err)
        else
            set err_log /dev/null
        end

        set -l add_status 1
        if test -n "$base"
            if git -C $repo_root worktree add --detach $worktree_path $base 2>$err_log
                set add_status 0
            end
        end
        if test $add_status -ne 0
            if git -C $repo_root worktree add --detach $worktree_path 2>$err_log
                set add_status 0
            end
        end

        if test $add_status -ne 0
            echo "agent: git worktree add failed for $branch" >&2
            if set -q _flag_debug; and test -s "$err_log"
                echo "[debug] git stderr:" >&2
                cat $err_log >&2
            end
            return 1
        end
    end

    # Build the command that runs in the meta-session pane. `zj` handles
    # create-or-attach for the per-agent session.
    set -l per_agent_cmd
    if test $session_exists -eq 1
        set per_agent_cmd "zj $branch"
    else
        if test -n "$_flag_seed" -a -f "$_flag_seed"
            set -l seed_path /tmp/agent-seed-$branch-(random).md
            cp $_flag_seed $seed_path
            set -l meta "This worktree is detached at the repo's default branch. Before doing anything else, in order: (1) read $seed_path to understand your task, (2) create a branch with \`git checkout -b <kebab-case-name>\` named for the task, (3) \`trash $seed_path\`."
            set -l escaped (string escape -- $meta)
            set per_agent_cmd "zj $branch -- claude --add-dir /tmp --permission-mode acceptEdits $escaped"
        else
            set -l meta "This worktree is detached at the repo's default branch. Once you understand the task, create a branch with \`git checkout -b <kebab-case-name>\` named for it before making any changes."
            set -l escaped (string escape -- $meta)
            set per_agent_cmd "zj $branch -- claude --permission-mode acceptEdits $escaped"
        end
    end

    # Always cd into the worktree first; per-agent session inherits the cwd.
    set -l safe_cwd (string escape -- $worktree_path)
    set -l pane_cmd "cd $safe_cwd; and $per_agent_cmd"

    set -q _flag_debug; and echo "[debug] pane_cmd: $pane_cmd" >&2

    if not _term_inside
        echo "agent: not running inside wezterm; worktree at $worktree_path"
        echo "       to start manually: cdw $branch; and $per_agent_cmd"
        return 0
    end

    set -l current_pane (_term_current_pane_id)

    # Pane already exists in the meta-session → focus, optionally inject prompt.
    set -l existing_pane_id
    if _agent_meta_exists
        set existing_pane_id (_agent_meta_pane_id $branch)
    end
    if test -n "$existing_pane_id"
        _agent_ensure_meta_tab >/dev/null
        if test -n "$_flag_seed" -a -f "$_flag_seed" -a $session_exists -eq 1
            _agent_inject_prompt $branch $_flag_seed
            echo "agent: sent prompt to existing $branch"
        else
            echo "agent: $branch already in agents tab (CMD+0 to view)"
        end
        if not set -q _flag_no_focus
            _term_focus $current_pane
        end
        return 0
    end

    if _agent_meta_exists
        # Spillover: place the new pane in the first meta-tab with room
        # (<6 user panes). If all tabs are full, create a new tab with the
        # agent as its initial pane.
        set -l target_tab (_agent_meta_target_tab)
        if test -n "$target_tab"
            zellij --session agents action new-pane --tab-id $target_tab --name $branch --cwd $worktree_path -- fish -c $pane_cmd
            or begin
                echo "agent: failed to add pane to meta-session" >&2
                return 1
            end
        else
            # New tab: use a temp layout so the page-indicator pane and
            # swap_tiled_layout apply (zellij doesn't inherit swap layouts
            # across tabs created by the bare `new-tab` action).
            set -l spill_layout (mktemp -t agents-spill).kdl
            _agent_write_meta_layout $spill_layout $branch $pane_cmd
            zellij --session agents action new-tab --layout $spill_layout >/dev/null
            or begin
                rm -f $spill_layout
                echo "agent: failed to spawn new tab in meta-session" >&2
                return 1
            end
            rm -f $spill_layout
        end

        _agent_ensure_meta_tab >/dev/null
    else
        # Bootstrap meta-session via a wezterm tab. Prepend a one-shot
        # toggle-pane-frames to the first agent pane so the meta-session
        # shows pane borders (active-pane highlight). Session-scoped, so it
        # persists for subsequent panes.
        set -l layout_file (mktemp -t agents-layout).kdl
        _agent_write_meta_layout $layout_file $branch "zellij action toggle-pane-frames; and $pane_cmd"

        set -q _flag_debug; and begin
            echo "[debug] meta layout:" >&2
            cat $layout_file >&2
        end

        set -l boot_cmd "zellij -s agents -n $layout_file; rm -f $layout_file"
        set -l new_pane (_term_spawn_tab --title agents $HOME $boot_cmd)
        if test -z "$new_pane"
            echo "agent: failed to spawn agents tab" >&2
            rm -f $layout_file
            return 1
        end
        _term_emit_event agents-tab-spawned $new_pane
    end

    if not set -q _flag_no_focus
        _term_focus $current_pane
    end

    if test $session_exists -eq 1
        echo "agent: reattached existing session $branch (CMD+0 to view)"
    else if test $worktree_exists -eq 1
        echo "agent: restarted $branch in existing worktree (CMD+0 to view)"
    else
        echo "agent: spawned $branch in agents tab (CMD+0 to view)"
    end
end
