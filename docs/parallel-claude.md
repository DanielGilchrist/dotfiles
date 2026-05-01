# Parallel Claude agents — workflow & keybinds

How this dotfiles repo wires up parallel Claude Code agents in worktrees, surfaced through a dedicated wezterm tab.

## Concepts

- **Worktree per agent** at `~/worktrees/<repo>/<name>`, checked out detached at `origin/HEAD` (master/main). Claude is responsible for creating its own branch off that point.
- **Zellij session per agent** named the same as the worktree dir, so the session is SSH-resumable from anywhere.
- **Single `agents` wezterm tab** holds every agent pane; capped at 8 via a 2-column smart-split layout. `CMD+0` toggles to it from anywhere; `CMD+0` again returns.
- **Neovim is a thin remote control**: it doesn't own Claude sessions, just talks to zellij sessions by name and embeds them in a Snacks terminal.

## Fish functions

These are **fish functions, not binaries**. From bash (or any non-fish shell, including Claude Code's `Bash` tool) invoke them via `fish -c '...'` — calling `agent ...` directly will fail with `command not found`.

| Command | Description |
|---|---|
| `agent <name>` | Spawn an agent. Opens nvim for a multi-line prompt; `:wq` with content seeds Claude, `:q` empty cancels. **Do not use this form from a non-interactive shell — the nvim instance will block forever; use `-e` or `--seed` instead.** Names cap at 25 chars (zellij socket limit on macOS). |
| `agent <name> -e "..."` | Same but seed inline. Best for short single-line prompts; quoting multi-line markdown inline is fragile. |
| `agent <name> --seed <file>` | Same but seed from a file. **Preferred when the seed is long or contains markdown / quotes / backticks** — write the seed to a temp file with the `Write` tool and pass the path. |
| `agent-rm <name>` | Force-tear-down: worktree + branch + session + pane. Infers from cwd if omitted. |
| `agent-rm --all` | Same, for every worktree + zellij session. Confirms first. |
| `zj <name>` | Attach (or create) a zellij session. Tab-completes from running sessions. |
| `zj <name> -- <cmd>` | Same, but if creating, run `<cmd>` as the first pane. |

## Wezterm keybinds

| Key | Action |
|---|---|
| `CMD+0` | Toggle to/from the agents tab. |
| `CMD+Shift+0` | Manually pin the agents tab to position 0 (auto-pin runs after spawn but is best-effort). |
| `CMD+Shift+O` | Worktree picker — fuzzy over `~/worktrees/<repo>/<branch>`; opens a new tab `cd`'d there. |
| `CMD+Left` / `CMD+Right` | Cycle tabs, skipping the agents tab. |
| `Shift+Alt+{` / `}` | Move tabs (refuses to move the agents tab; skips it as a swap target). |
| `CMD+Alt+h/j/k/l` (or arrows) | Navigate between panes. |
| `CMD+R` | Resize-mode key table; `h/j/k/l` repeat-resize, `Esc` exits. |
| `CMD+Shift+Z` | Zoom/unzoom active pane. |
| `CMD+Shift+M` | Swap panes interactively. |

## Neovim keybinds (`agent_nvim` plugin, leader is `<space>`)

| Key | Action |
|---|---|
| `<leader>zn` | New agent: prompts for a name, then opens a multi-line prompt buffer. `<C-s>` submits, `q` / `<C-c>` cancels. After spawn, attaches a Snacks terminal to the new session. |
| `<leader>zo` | Open/focus the agent for the current worktree. If none, picker over running sessions (with "new agent" entry). |
| `<leader>zs` | Send the current buffer's path as `@<abs-path>` to the targeted session. |
| `<leader>zv` (visual) | Send visual selection. |
| `<leader>zp` | Single-line prompt → submit. |
| `<leader>zt` | Switch the buffer-local target session. |
| `<leader>zk` | Force-kill the targeted session. |
| `<C-.>` | Toggle the active session's terminal (detaches via clean close so the wezterm pane re-renders at full width). |

Default targeting: cwd → branch → session name (auto, no manual switching needed in the common case).

Commands also exposed: `:AgentNew`, `:AgentOpen`, `:AgentSend`, `:AgentPrompt`, `:AgentSwitch`, `:AgentKill`.

## Typical flow

1. From any nvim instance: `<leader>zn` → name (kebab-case) → multi-line prompt → `<C-s>`.
2. agent.fish: creates `~/worktrees/<repo>/<name>` detached at `origin/HEAD`, marks it trusted in `~/.claude.json`, spawns a wezterm pane in the agents tab, starts Claude with an initial prompt that says "branch first, then read the seed".
3. Claude reads the seed, deletes it, creates a branch off master with a sensible name, gets to work.
4. `CMD+0` to watch; `CMD+0` again to return.
5. `agent-rm <name>` (or from inside the worktree) when done.

## Caveats

- **MCP prompts**: `disabledMcpjsonServers` in `claude/settings.json` silences known project MCP servers (e.g. `chrome-devtools`). Add new ones there as you encounter them.
- **Wezterm `--cwd` is unreliable** (wez/wezterm#4618 canonicalization). The agent helpers `cd` inside the spawned shell instead of trusting `--cwd`.
- **Auto-pin to position 0** is best-effort (race with mux registration). If the agents tab lands at the wrong position, `CMD+Shift+0` pins it.
