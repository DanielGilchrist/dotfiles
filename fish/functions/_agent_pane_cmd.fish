function _agent_pane_cmd --description "Echo the per-agent pane command: zj <branch> -- agent-claude --add-dir /tmp --permission-mode auto [prompt]"
    set -l branch $argv[1]
    set -l extra $argv[2..-1]
    # --add-dir is variadic, so it must come before --permission-mode or a
    # trailing prompt argument gets swallowed as another directory.
    echo "zj $branch -- $HOME/.config/bin/agent-claude --add-dir /tmp --permission-mode auto $extra"
end
