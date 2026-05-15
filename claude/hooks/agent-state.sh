#!/bin/bash
# Claude Code hook: pushes the current agent's state to the zellij
# agents-bar plugin via `zellij pipe`. Only fires inside agent worktrees
# under ~/worktrees/, so stray runs of `claude` outside don't push noise.
#
# Wired as Notification (= "claude wants user input") and Stop (= "task
# done") hooks in claude/settings.json.
#
# Usage: agent-state.sh <state>
#   states: awaiting | idle
#
# Notification fires for both permission prompts AND the 60s-idle
# reminder ("Claude is waiting for your input"). We only want the
# former, so we read the JSON payload on stdin and filter on the
# `message` field.

set -euo pipefail

state="${1:-unknown}"

case "$PWD" in
    "$HOME/worktrees/"*) ;;
    *) exit 0 ;;
esac

if [ "$state" = awaiting ]; then
    msg="$(jq -r '.message // ""' 2>/dev/null)"
    case "$msg" in
        *permission*) ;;
        *) exit 0 ;;
    esac
fi

agent="$(basename "$PWD")"
zellij --session agents pipe --name agent-state -- "$agent	$state" >/dev/null 2>&1 || true
