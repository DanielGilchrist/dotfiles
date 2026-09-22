function _agent_attach --description "agent attach — create-or-attach a per-agent zellij session; add a meta-pane if inside wezterm."
    argparse --name='agent attach' 'h/help' 'd/debug' 'e/prompt=' 'seed=' 'repo=' 'no-focus' 'no-prompt' 'headless' -- $argv
    or return

    if set -q _flag_help
        echo "usage: agent attach <name> [-e <prompt> | --seed <file>] [--repo <path>]"
        echo ""
        echo "  <name>               — used as worktree dir, git branch, and zellij session name"
        echo "  -e, --prompt <text>  — inline prompt seeded into Claude as the first message"
        echo "  --seed <file>        — multi-line prompt from a file (same as -e)"
        echo "  --repo <path>        — repo root (defaults to git worktree list --porcelain from cwd)"
        echo "  --no-focus           — don't refocus the calling pane after spawn"
        echo "  --no-prompt          — fresh spawn without opening the nvim editor for a seed (starts Claude with no prompt)"
        echo "  --headless           — no agents-tab pane; prints 'headless_cwd:'/'headless_cmd:' for the caller"
        echo "  -d, --debug          — print spawn commands to stderr"
        return 0
    end

    if test (count $argv) -lt 1
        echo "agent attach: <name> is required (e.g. \`agent attach payroll-fix\`)" >&2
        return 1
    end

    set -l branch $argv[1]

    if test "$branch" = agents
        echo "agent attach: 'agents' is reserved (used for the meta-session)" >&2
        return 1
    end

    _agent_unhide_quiet $branch

    if test -n "$_flag_prompt" -a -n "$_flag_seed"
        echo "agent attach: pass either -e or --seed, not both" >&2
        return 1
    end

    set -l repo_root $_flag_repo
    if test -z "$repo_root"
        set repo_root (git worktree list --porcelain 2>/dev/null | head -1 | string replace -r '^worktree ' '')
    end
    if test -z "$repo_root"
        echo "agent attach: not in a git repo and --repo not given" >&2
        return 1
    end
    set -l resolved_main (git -C $repo_root worktree list --porcelain 2>/dev/null | head -1 | string replace -r '^worktree ' '')
    test -n "$resolved_main"; and set repo_root $resolved_main
    set -l repo_name (basename $repo_root)

    set -l worktrees_dir "$HOME/worktrees/$repo_name"
    set -l worktree_path "$worktrees_dir/$branch"

    # Look for ANY worktree named <branch> across every repo namespace.
    # `agent attach <name>` is an "I want that agent" verb, so if a
    # worktree already exists — under this repo or another — prefer it
    # over creating a duplicate. If multiple exist we refuse and force
    # the caller to disambiguate (or `agent rm` the wrong ones).
    set -l existing
    for candidate in (find $HOME/worktrees -mindepth 2 -maxdepth 2 -name $branch -type d 2>/dev/null)
        test -e "$candidate/.git"; and set -a existing $candidate
    end

    set -l worktree_exists 0
    if test (count $existing) -gt 1
        echo "agent attach: multiple worktrees named $branch — remove the duplicates first:" >&2
        for p in $existing
            echo "  $p" >&2
        end
        return 1
    else if test (count $existing) -eq 1
        set worktree_path $existing[1]
        set worktrees_dir (dirname $existing[1])
        set worktree_exists 1
    end

    set -l session_exists 0
    zellij list-sessions -s 2>/dev/null | string match -q -- $branch; and set session_exists 1

    if test $session_exists -eq 1 -a $worktree_exists -eq 0
        echo "agent attach: session $branch is alive but its worktree is gone — its Claude is running in a deleted directory." >&2
        echo "       creating a fresh worktree at $worktree_path and reattaching; run \`agent rm $branch\` first if you want a clean start." >&2
    end

    if test $session_exists -eq 0; and test (string length -- $branch) -gt 20
        echo "agent attach: name too long ("(string length -- $branch)" chars) for a new session; zellij session names must be ≤ 20 chars." >&2
        return 1
    end

    set -q _flag_debug; and echo "[debug] worktree_exists=$worktree_exists session_exists=$session_exists" >&2

    if test -n "$_flag_prompt"
        set _flag_seed (mktemp -t agent-prompt)
        printf "%s" $_flag_prompt > $_flag_seed
    else if test -z "$_flag_seed" -a $worktree_exists -eq 0 -a $session_exists -eq 0; and status --is-interactive; and not set -q _flag_no_prompt
        set -l editor_path /tmp/agent-prompt-$branch-(random).md
        touch $editor_path
        nvim $editor_path
        if test -s $editor_path
            set _flag_seed $editor_path
        else
            rm -f $editor_path
            echo "agent attach: cancelled (empty prompt)"
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
            echo "agent attach: git worktree add failed for $branch" >&2
            if set -q _flag_debug; and test -s "$err_log"
                echo "[debug] git stderr:" >&2
                cat $err_log >&2
            end
            return 1
        end
    end

    set -l per_agent_cmd
    if test $session_exists -eq 1
        set per_agent_cmd "zj $branch"
    else
        if test -n "$_flag_seed" -a -f "$_flag_seed"
            set -l seed_path /tmp/agent-seed-$branch-(random).md
            cp $_flag_seed $seed_path
            set -l meta "This worktree is detached at the repo's default branch. Before doing anything else, in order: (1) read $seed_path to understand your task, (2) create a branch with \`git checkout -b <kebab-case-name>\` named for the task, (3) \`trash $seed_path\`."
            set -l escaped (string escape -- $meta)
            set per_agent_cmd "zj $branch -- claude --add-dir /tmp --permission-mode auto $escaped"
        else if set -q _flag_no_prompt
            set per_agent_cmd "zj $branch -- claude --permission-mode auto"
        else
            set -l meta "This worktree is detached at the repo's default branch. Once you understand the task, create a branch with \`git checkout -b <kebab-case-name>\` named for it before making any changes."
            set -l escaped (string escape -- $meta)
            set per_agent_cmd "zj $branch -- claude --permission-mode auto $escaped"
        end
    end

    if set -q _flag_headless
        echo "headless_cwd:$worktree_path"
        echo "headless_cmd:$per_agent_cmd"
        return 0
    end

    set -l safe_cwd (string escape -- $worktree_path)
    set -l pane_cmd "cd $safe_cwd; and $per_agent_cmd"

    set -q _flag_debug; and echo "[debug] pane_cmd: $pane_cmd" >&2

    if not _term_inside
        echo "agent attach: not running inside wezterm; worktree at $worktree_path"
        echo "       to start manually: cdw $branch; and $per_agent_cmd"
        return 0
    end

    set -l current_pane (_term_current_pane_id)

    set -l existing_pane_id
    if _agent_meta_exists
        set existing_pane_id (_agent_meta_pane_id $branch)
    end
    if test -n "$existing_pane_id"
        _agent_ensure_meta_tab >/dev/null
        if test -n "$_flag_seed" -a -f "$_flag_seed" -a $session_exists -eq 1
            _agent_inject_prompt $branch $_flag_seed
            echo "agent attach: sent prompt to existing $branch"
        else
            echo "agent attach: $branch already in agents tab (CMD+0 to view)"
        end
        if not set -q _flag_no_focus
            _term_focus $current_pane
        end
        return 0
    end

    if _agent_meta_exists
        set -l target_tab (_agent_meta_target_tab)
        if test -n "$target_tab"
            zellij --session agents action new-pane --tab-id $target_tab --name $branch --cwd $worktree_path -- fish -c $pane_cmd
            or begin
                echo "agent attach: failed to add pane to meta-session" >&2
                return 1
            end
        else
            set -l spill_layout (mktemp -t agents-spill).kdl
            _agent_write_meta_layout $spill_layout $branch $pane_cmd
            zellij --session agents action new-tab --layout $spill_layout >/dev/null
            or begin
                rm -f $spill_layout
                echo "agent attach: failed to spawn new tab in meta-session" >&2
                return 1
            end
            rm -f $spill_layout
        end

        _agent_ensure_meta_tab >/dev/null
    else
        # A prior failed bootstrap can leave a wezterm "agents" tab open with
        # no live meta-session. Kill any such orphans so we don't stack tabs.
        for p in (wezterm cli list --format json 2>/dev/null | jq -r '.[] | select(.tab_title == "agents") | .pane_id')
            wezterm cli kill-pane --pane-id $p 2>/dev/null
        end

        set -l layout_file (mktemp -t agents-layout).kdl
        _agent_write_meta_layout $layout_file $branch "zellij action toggle-pane-frames; and $pane_cmd"

        set -q _flag_debug; and begin
            echo "[debug] meta layout:" >&2
            cat $layout_file >&2
        end

        # Clear any resurrection cache first — otherwise `zellij -s agents -n`
        # refuses because a serialised session still exists on disk. On failure
        # drop to fish so the wezterm tab doesn't disappear silently.
        set -l err_log $layout_file.err
        set -l boot_cmd "zellij delete-session agents 2>/dev/null; or true; zellij -s agents -n $layout_file 2> $err_log; or begin; echo 'zellij bootstrap failed — layout: '$layout_file' stderr: '$err_log; exec fish; end; rm -f $layout_file $err_log"
        set -l new_pane (_term_spawn_tab --title agents $HOME $boot_cmd)
        if test -z "$new_pane"
            echo "agent attach: failed to spawn agents tab" >&2
            rm -f $layout_file
            return 1
        end
        _term_emit_event agents-tab-spawned $new_pane
    end

    if not set -q _flag_no_focus
        _term_focus $current_pane
    end

    if test $session_exists -eq 1
        echo "agent attach: reattached existing session $branch (CMD+0 to view)"
    else if test $worktree_exists -eq 1
        echo "agent attach: restarted $branch in existing worktree (CMD+0 to view)"
    else
        echo "agent attach: spawned $branch in agents tab (CMD+0 to view)"
    end
end
