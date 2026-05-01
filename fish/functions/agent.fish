function agent --description "Spawn a Claude agent in a worktree + zellij session + agents tab pane"
    argparse --name=agent 'h/help' 'd/debug' 'e/prompt=' 'seed=' 'repo=' 'no-focus' -- $argv
    or return

    if set -q _flag_help
        echo "usage: agent <name> [-e <prompt> | --seed <file>] [--repo <path>]"
        echo ""
        echo "  <name>               — required. Used as worktree dir, git branch, and zellij session name."
        echo "  -e, --prompt <text>  — inline prompt seeded into Claude as the first message"
        echo "  --seed <file>        — multi-line prompt from a file (same as -e)"
        echo "  --repo <path>        — repo root (defaults to git rev-parse from cwd)"
        echo "  -d, --debug          — print spawn commands to stderr"
        return 0
    end

    if test (count $argv) -lt 1
        echo "agent: <name> is required (e.g. \`agent payroll-fix\`)" >&2
        echo "       see \`agent --help\` for full usage" >&2
        return 1
    end

    set -l branch $argv[1]
    set -l agents_tab_title agents
    set -l max_agents 8

    # Zellij's session name length is capped by the unix-socket path budget
    # (macOS sockaddr_un limit ~= 104 chars). On macOS the leftover for the
    # session name is ~25 chars; longer names hard-error from zellij later
    # which manifests as a silently-dying spawn.
    if test (string length -- $branch) -gt 25
        echo "agent: name too long ("(string length -- $branch)" chars); zellij caps session names around 25 chars on macOS." >&2
        echo "       try a shorter name." >&2
        return 1
    end

    if test -n "$_flag_prompt" -a -n "$_flag_seed"
        echo "agent: pass either -e or --seed, not both" >&2
        return 1
    end

    set -l repo_root $_flag_repo
    if test -z "$repo_root"
        set repo_root (git rev-parse --show-toplevel 2>/dev/null)
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
    # spawn (no worktree, no session), open nvim for a multi-line prompt.
    # Reconnect cases (worktree or session already there) skip the editor.
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
        # Drop any stale worktree registrations whose dirs no longer exist.
        git -C $repo_root worktree prune 2>/dev/null

        # Branch off the repo's default branch (origin/HEAD), not whatever the
        # user happens to be on right now.
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

        # Worktree is detached at the default branch (origin/master or origin/main).
        # Claude is responsible for creating an appropriately-named branch off
        # this point before making changes (instructed via the initial prompt).
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

    set -l zj_cmd
    if test $session_exists -eq 1
        set zj_cmd "zj $branch"
    else
        set -l branch_instruction "This worktree is checked out detached at the repo's default branch. Before making any changes, create a branch with `git checkout -b <kebab-case-name>` describing the task."
        if test -n "$_flag_seed" -a -f "$_flag_seed"
            set -l seed_path /tmp/agent-seed-$branch-(random).md
            cp $_flag_seed $seed_path
            set -l meta "$branch_instruction Then read $seed_path for your task, and `rm $seed_path` before doing anything else."
            set -l escaped (string escape -- $meta)
            set zj_cmd "zj $branch -- claude --add-dir /tmp --permission-mode acceptEdits $escaped"
        else
            set -l escaped (string escape -- $branch_instruction)
            set zj_cmd "zj $branch -- claude --permission-mode acceptEdits $escaped"
        end
    end

    set -q _flag_debug; and echo "[debug] zj_cmd: $zj_cmd" >&2

    if not _term_inside
        echo "agent: not running inside a wrapped terminal; worktree at $worktree_path"
        echo "       to start: cdw $branch; and $zj_cmd"
        return 0
    end

    set -l existing_pane (_term_pane_for_cwd $worktree_path)
    if test -n "$existing_pane"
        _term_focus $existing_pane
        if test -n "$_flag_seed" -a -f "$_flag_seed" -a $session_exists -eq 1
            _agent_inject_prompt $branch $_flag_seed
            echo "agent: sent prompt to existing $branch"
        else
            echo "agent: reconnected to existing pane for $branch"
        end
        return 0
    end

    set -l agents_tab_id (_term_tab_with_title $agents_tab_title)
    set -q _flag_debug; and echo "[debug] agents_tab_id='$agents_tab_id'" >&2

    set -l current_pane (_term_current_pane_id)

    if test -z "$agents_tab_id"
        set -l new_pane (_term_spawn_tab --title $agents_tab_title $worktree_path $zj_cmd)
        if test -z "$new_pane"
            echo "agent: failed to spawn agents tab" >&2
            return 1
        end
        _term_emit_event agent-action pin-agents-tab
    else
        set -l count (_term_panes_in_tab $agents_tab_id)
        if test $count -ge $max_agents
            echo "agent: reached max $max_agents agents — \`agent-rm <name>\` first" >&2
            return 1
        end

        set -q _flag_debug; and echo "[debug] split-largest in tab $agents_tab_id" >&2
        _term_split_largest_in_tab $agents_tab_id $worktree_path $zj_cmd >/dev/null
        or begin
            echo "agent: split failed" >&2
            return 1
        end
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
