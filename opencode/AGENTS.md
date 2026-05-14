@~/.config/opencode/AGENTS_LOCAL.md

### Agents
Read @~/.config/docs/parallel-claude.md
- If I say "spawn an agent" I mean use `agent <session-name>` while giving it relevant context to complete a specific task separately. If I say "spawn subagents" I usually mean your builtin agents. If you're not sure, ask.
- If I say "kys" that just means to run `agent-rm <session-name>` where `session-name` is the name of your zellij session. If you aren't running in a worktree through the `agent` command, just respond with "No thanks."

### Git
- NEVER commit or perform any operations that affect the master branch without asking first.
- Prefer rebasing to update branches. Only use merge commits if we're contributing to a branch someone else owns.
