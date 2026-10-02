function _agent_pane_cmd --description "Echo the per-agent pane command: zj <branch> -- agent-claude --permission-mode auto [extra claude args...]"
    set -l branch $argv[1]
    set -l extra $argv[2..-1]
    echo "zj $branch -- $HOME/.config/bin/agent-claude --permission-mode auto $extra"
end
