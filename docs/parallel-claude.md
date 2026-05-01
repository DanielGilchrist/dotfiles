# Parallel Claude agents — workflow & keybinds

How this dotfiles repo wires up parallel Claude Code agents in worktrees, surfaced through a dedicated wezterm tab.

## Concepts

- **Worktree per agent** at `~/worktrees/<repo>/<name>`, checked out detached at `origin/HEAD` (master/main). Claude is responsible for creating its own branch off that point — the spawn prompt instructs it to `git checkout -b <kebab-case-name>` before doing anything else.
- **Zellij session per agent** named the same as the worktree dir, so the session is SSH-resumable from anywhere. The session runs `claude --permission-mode acceptEdits` (no `~/.claude.json` trust mutation).
- **Single `agents` wezterm tab** holds every agent pane; capped at 8 via a 2-column smart-split (always splits the largest pane). `CMD+0` toggles to it from anywhere; `CMD+0` again returns. `CMD+W` is refused inside this tab so you don't accidentally kill an agent.
- **Neovim is a thin remote control**: it doesn't own Claude sessions, just talks to zellij sessions by name and embeds them in a Snacks terminal.
- **Repo root is resolved to the *main* worktree** (first entry of `git worktree list`), so spawning from inside one agent's worktree does **not** nest under `~/worktrees/<branch>/<new-branch>`.

## Spawning agents from an LLM (Claude Code's `Bash` tool, etc.)

These functions are **fish functions, not binaries**. From bash or any non-fish shell — including Claude Code's `Bash` tool — invoke via `fish -c '...'`. Calling `agent ...` directly fails with `command not found`.

**Rules of thumb for LLM callers:**

1. **Never use the bare form `agent <name>`** with no `-e`/`--seed`. It opens nvim for an interactive prompt and will block forever in a non-interactive shell.
2. **For any non-trivial prompt, write the seed to a file and pass `--seed <path>`.** Inline `-e "..."` works for short single-line prompts but is fragile with markdown, backticks, quotes, or newlines. The recommended pattern: use the `Write` tool to drop the seed at e.g. `/tmp/agent-seed-<name>.md`, then `fish -c 'agent <name> --seed /tmp/agent-seed-<name>.md'`. The agent will copy the seed to a `/tmp/agent-seed-<branch>-<rand>.md` path, instruct Claude to read it then `rm` it, so cleanup is automatic.
3. **Names must be kebab-case and ≤25 chars.** macOS unix-socket path budget caps zellij session names; longer names silently fail.
4. **Cap is 8 concurrent agents.** Spawning the 9th errors with "reached max 8 agents — `agent-rm <name>` first".
5. **Spawning only renders a pane when called from inside a wrapped wezterm/zellij session.** From a bare shell it'll create the worktree and print the manual `cdw $branch; and zj $branch -- claude ...` command instead of spawning a pane. (LLMs running inside Claude Code's Bash tool *are* inside the wrapper, so this is rarely a problem.)
6. **Repeat-spawn semantics:**
   - Worktree gone, session gone → fresh spawn.
   - Worktree exists, session gone → restart Claude in existing worktree (skips the seed editor).
   - Session exists → reattach. If `-e/--seed` is also provided, the prompt is injected into the running Claude via `_agent_inject_prompt`.

## Fish commands

| Command | Description |
|---|---|
| `agent <name>` | Spawn an agent. Opens nvim for a multi-line prompt; `:wq` with content seeds Claude, `:q` empty cancels. **Interactive only — do not call from a non-interactive shell.** |
| `agent <name> -e "<prompt>"` | Same but seed inline. Best for short single-line prompts. |
| `agent <name> --seed <file>` | Same but seed from a file. **Preferred for non-trivial prompts.** Seed file is copied to `/tmp/agent-seed-...`; Claude is told to read then `rm` it. |
| `agent <name> --repo <path>` | Override the repo root (defaults to the main worktree of the repo containing cwd). |
| `agent <name> --no-focus` | Don't refocus the calling pane after spawn. |
| `agent <name> --debug` (`-d`) | Print spawn commands and intermediate state to stderr. |
| `agent --help` | Usage. |
| `agent-rm <name>` | Force-tear-down: worktree + branch + zellij session + wezterm pane. Infers from cwd if omitted. |
| `agent-rm --all` | Same, for every worktree + zellij session. Confirms first (must type `yes`). |
| `agent-rm --help` | Usage. |
| `zj` | List active zellij sessions. |
| `zj <name>` | Attach (or create) a zellij session in cwd. From inside wezterm, auto-zooms the matching agents-tab pane to the attacher's window size; unzooms on detach. |
| `zj <name> -- <cmd> ...` | Same, but if creating, run `<cmd>` as the first pane via a generated layout. |
| `zj -d ... -- ...` | Print the generated layout file before launching. |

## Wezterm keybinds

| Key | Action |
|---|---|
| `CMD+0` | Toggle to/from the agents tab. |
| `CMD+Shift+0` | Manually pin the agents tab to position 0 (auto-pin runs after spawn but is best-effort). |
| `CMD+Shift+O` | Worktree picker — fuzzy over `~/worktrees/<repo>/<branch>`; opens a new tab `cd`'d there. |
| `CMD+Left` / `CMD+Right` | Cycle tabs, skipping the agents tab. |
| `Shift+Alt+{` / `}` | Move tabs (refuses to move the agents tab; skips it as a swap target). |
| `CMD+Alt+h/j/k/l` (or arrows) | Navigate between panes. |
| `CMD+R` | Resize-mode key table; `h/j/k/l` repeat-resize, `Esc`/`Enter`/`Ctrl+C` exits. |
| `CMD+Shift+Z` | Zoom/unzoom active pane. |
| `CMD+Shift+M` | Swap panes interactively. |
| `CMD+Shift+P` | Activate-by-pane-select. |
| `CMD+W` | Close active pane — **refused inside the agents tab**. |
| `CMD+Shift+W` | Close active tab — refused for the agents tab. |

## Neovim keybinds (`agent_nvim` plugin, leader is `<space>`, prefix `<leader>z`)

| Key | Action |
|---|---|
| `<leader>zn` | New agent: prompts for a name, then opens a multi-line prompt buffer. `<C-s>` submits, `q` / `<C-c>` cancels. After spawn, attaches a Snacks terminal to the new session. |
| `<leader>zo` | Open/focus the agent for the current worktree. If none, picker over running sessions (with "new agent" entry). |
| `<leader>zs` | Send the current buffer's path as `@<abs-path>` to the targeted session. |
| `<leader>zv` (visual) | Send visual selection. |
| `<leader>zp` | Single-line prompt → submit. |
| `<leader>zt` | Switch the buffer-local target session. |
| `<leader>zk` | Force-kill the targeted session. |
| `<C-.>` (n/i/t/x) | Toggle the active session's terminal (detaches via clean close so the wezterm pane re-renders at full width). |

Default targeting: cwd → branch → session name (auto, no manual switching needed in the common case).

Commands also exposed: `:AgentNew`, `:AgentOpen`, `:AgentSend`, `:AgentPrompt`, `:AgentSwitch`, `:AgentKill`.

## Typical flow (human)

1. From any nvim instance: `<leader>zn` → name (kebab-case) → multi-line prompt → `<C-s>`.
2. `agent.fish` resolves the main repo root, creates `~/worktrees/<repo>/<name>` detached at `origin/HEAD`, spawns a wezterm pane in the agents tab, and starts Claude with an initial prompt pointing at the seed file.
3. Claude reads the seed, picks a sensible kebab-case branch name based on the task, `git checkout -b`s it, `rm`s the seed, and gets to work.
4. `CMD+0` to watch; `CMD+0` again to return.
5. `agent-rm <name>` (or from inside the worktree, no arg) when done.

## Typical flow (LLM driving via `Bash`)

```sh
# 1. Write the seed to a temp file (use the Write tool, not heredoc — quoting is a trap).
#    Path: /tmp/agent-seed-<name>.md, contents: full task brief in markdown.

# 2. Spawn:
fish -c 'agent <name> --seed /tmp/agent-seed-<name>.md'

# 3. (optional) Send a follow-up prompt to the running session later:
fish -c 'agent <name> -e "follow-up instruction"'
#    — this routes through _agent_inject_prompt because the session already exists.

# 4. Tear down when done:
fish -c 'agent-rm <name>'
```

## Caveats

- **MCP prompts**: `disabledMcpjsonServers` in `claude/settings.json` silences known project MCP servers (e.g. `chrome-devtools`). Add new ones there as you encounter them.
- **Wezterm `--cwd` is unreliable** (wez/wezterm#4618 canonicalization). The agent helpers `cd` inside the spawned shell instead of trusting `--cwd`.
- **Auto-pin to position 0** is best-effort (race with mux registration). If the agents tab lands at the wrong position, `CMD+Shift+0` pins it.
- **Zellij session-name length** is capped at 25 chars on macOS by the unix socket path budget. Over-long names silently fail; `agent` rejects them upfront.
- **Spawning from a non-interactive shell without `-e`/`--seed`** opens nvim and blocks forever. Always pass a seed when scripting.
