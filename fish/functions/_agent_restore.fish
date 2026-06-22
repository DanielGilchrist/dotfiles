function _agent_restore --description "Rebuild the agents grid: ensure each live per-agent session has a meta-session pane and a wezterm tab."
    if not _term_inside
        echo "agent --restore: not running inside wezterm" >&2
        return 1
    end

    set -l branches (zellij list-sessions -s 2>/dev/null | string match -v -- agents)

    if test (count $branches) -eq 0
        echo "agent --restore: no per-agent sessions to restore"
        return 0
    end

    if not _agent_meta_exists
        # Build a layout file with one pane per live session and bootstrap the meta-session via a wezterm tab.
        set -l layout_file (mktemp -t agents-restore-layout).kdl

        # If the per-agent zellij session is gone (server didn't survive
        # whatever — reboot, kill, etc.), the resurrection cache puts panes
        # into a "Waiting to run" stub. Wipe that and pass `claude --continue`
        # as the initial pane command so the agent picks up its latest
        # conversation in the worktree automatically.
        set -l claude_cmd "claude --continue --permission-mode acceptEdits"

        set -l layout_args
        for b in $branches
            set -l wp (find $HOME/worktrees -mindepth 2 -maxdepth 2 -name $b -type d 2>/dev/null | head -1)
            # Drop any stale resurrection cache so `zj` creates fresh with
            # claude as the first pane rather than resurrecting a stub.
            set -l prep "zellij delete-session $b 2>/dev/null; or true"
            set -l cmd
            if test -n "$wp"
                set -l safe_cwd (string escape -- $wp)
                set cmd "cd $safe_cwd; and $prep; and zj $b -- $claude_cmd"
            else
                set cmd "$prep; and zj $b -- $claude_cmd"
            end
            set -a layout_args $b $cmd
        end

        _agent_write_meta_layout $layout_file $layout_args

        # Keep the layout file on disk for post-mortem if zellij refuses it.
        # Stderr is captured to a sibling log file. Drop to a shell on failure
        # instead of letting the wezterm tab disappear silently.
        set -l err_log $layout_file.err
        set -l boot_cmd "zellij -s agents -n $layout_file 2> $err_log; or begin; echo 'zellij bootstrap failed — layout: '$layout_file' stderr: '$err_log; exec fish; end; rm -f $layout_file $err_log"
        set -l new_pane (_term_spawn_tab --title agents $HOME $boot_cmd)
        if test -z "$new_pane"
            rm -f $layout_file
            echo "agent --restore: failed to spawn agents tab" >&2
            return 1
        end
        _term_emit_event agents-tab-spawned $new_pane

        # Toggle pane frames ON. The user's global config has `pane_frames false`,
        # but agent panes need titles. Poll briefly until the session is ready
        # to accept the action — zellij needs a moment after `-n <layout>`.
        for i in (seq 1 20)
            if _agent_meta_exists
                zellij --session agents action toggle-pane-frames >/dev/null 2>&1
                break
            end
            sleep 0.1
        end

        # No consolidate here: the layout file places panes correctly, and
        # calling consolidate before zellij has actually started would race.
        echo "agent --restore: rebuilt meta-session with "(count $branches)" agent(s)"
        return 0
    end

    # Meta-session already alive — diff and add missing panes.
    set -l added 0
    for b in $branches
        set -l existing (_agent_meta_pane_id $b)
        if test -n "$existing"
            continue
        end

        set -l wp (find $HOME/worktrees -mindepth 2 -maxdepth 2 -name $b -type d 2>/dev/null | head -1)
        set -l cwd_args
        set -l pane_cmd "zj $b"
        if test -n "$wp"
            set cwd_args --cwd $wp
            set -l safe_cwd (string escape -- $wp)
            set pane_cmd "cd $safe_cwd; and zj $b"
        end

        zellij --session agents action new-pane --name $b $cwd_args -- fish -c $pane_cmd
        or continue
        set added (math $added + 1)
    end

    _agent_ensure_meta_tab >/dev/null
    _agent_consolidate

    if test $added -eq 0
        echo "agent --restore: meta-session in sync — no panes added"
    else
        echo "agent --restore: added $added missing pane(s)"
    end
end
