# Parallel Claude agents — workflow & keybinds

How this dotfiles repo wires up parallel Claude Code agents in worktrees,
surfaced through a single zellij meta-session that wezterm renders in a
dedicated tab.

## Architecture

Two layers of zellij sessions:

- **Layer 1 — per-agent sessions.** One zellij session per agent, named
  after the worktree (`payroll-fix`, etc.). Claude actually runs here. SSH-
  resumable, survives wezterm/laptop restarts. This is the persistence layer.
- **Layer 2 — `agents` meta-session.** A single zellij session whose only
  job is to be a multi-pane viewport. Each pane runs `zj <branch>` against
  a Layer-1 session. Wezterm renders this meta-session as a single pane in
  the agents tab.

Zellij multi-attach is mirror-mode: a Layer-1 session can be attached
simultaneously by the meta-session pane *and* by you running `zj <branch>`
from another wezterm tab or your phone. Same session, multiple viewers.

## Concepts

- **Worktree per agent** at `~/worktrees/<repo>/<name>`, checked out
  detached at `origin/HEAD`. Claude is responsible for creating its own
  branch off that point — the spawn prompt instructs it to
  `git checkout -b <kebab-case-name>` before doing anything else.
- **Meta-session is named `agents`** — the name is reserved (`agent` and
  `agent-rm` refuse it). It serializes via `session_serialization true`,
  so its layout survives wezterm restarts.
- **Single `agents` wezterm tab** — one wezterm pane, running
  `zellij attach agents`. The grid lives inside zellij. `CMD+0` toggles
  to/from this tab; `CMD+W` is refused inside it.
- **Repo root resolves to the *main* worktree** so spawning from inside
  one agent's worktree does not nest under
  `~/worktrees/<branch>/<new-branch>`.
- **Cap is 8 concurrent agents.** Spawning the 9th errors.

## Keybinds

### Wezterm (everywhere except the agents tab pane content)

| Key | Action |
|---|---|
| `CMD+0` | Toggle to/from the agents tab |
| `CMD+Shift+0` | Pin agents tab to position 0 |
| `CMD+Shift+O` | Worktree picker — fuzzy over `~/worktrees/<repo>/<branch>` |
| `CMD+Left` / `CMD+Right` | Cycle tabs, skipping the agents tab |
| `Shift+Alt+{` / `}` | Move tabs (refuses to move the agents tab) |
| `CMD+Alt+h/j/k/l` (or arrows) | Wezterm pane navigation (outside the agents tab) |
| `CMD+W` / `CMD+Shift+W` | Close pane / tab — refused on the agents tab |

### Inside the agents tab (zellij's defaults)

The agents tab content is a zellij session, so it uses zellij keys, not
wezterm keys. The `~/.config/zellij/config.kdl` defaults apply:

| Key | Action |
|---|---|
| `Alt+h/j/k/l` (or arrows) | Move focus between agents |
| `Alt+f` | Toggle floating panes (rarely useful here) |
| `Ctrl+n` then `h/j/k/l` | Resize-mode |
| `Ctrl+g` | Toggle locked mode |
| `Ctrl+o` then `d` | Detach the meta-session |

The wezterm `CMD+...` bindings still work as expected for tab-level
operations (`CMD+0`, `CMD+T`, etc.) — they fire before keystrokes reach
zellij.

## Fish commands

| Command | Description |
|---|---|
| `agent <name>` | Spawn an agent. Opens nvim for a multi-line prompt; `:wq` with content seeds Claude, `:q` empty cancels. **Interactive only.** |
| `agent <name> -e "<prompt>"` | Same but seed inline. Best for short single-line prompts. |
| `agent <name> --seed <file>` | Same but seed from a file. **Preferred for non-trivial prompts.** Seed file is copied to `/tmp/agent-seed-...`; Claude is told to read then `rm` it. |
| `agent <name> --repo <path>` | Override the repo root (defaults to the main worktree). |
| `agent <name> --no-focus` | Don't refocus the calling pane after spawn. |
| `agent <name> --debug` (`-d`) | Print spawn commands and intermediate state to stderr. |
| `agent --restore` | Rebuild the agents grid from live per-agent sessions. Adds a meta-session pane for any session missing one, and spawns the wezterm tab if it's not there. |
| `agent --help` | Usage. |
| `agent-rm <name>` | Force-tear-down: worktree + branch + zellij session + meta-session pane. Infers from cwd if omitted. |
| `agent-rm --all` | Same, for every worktree + per-agent zellij session. Confirms first. |
| `zj` | List active zellij sessions. |
| `zj <name>` | Attach (or create) a zellij session in cwd. If a meta-session pane corresponds to `<name>`, it's auto-fullscreened while you're attached so its render dimensions don't constrain you. |
| `zj <name> -- <cmd> ...` | Same, but if creating, run `<cmd>` as the first pane. |

## Spawning from an LLM (Claude Code's `Bash` tool, etc.)

These functions are **fish functions, not binaries**. From bash or any
non-fish shell, invoke via `fish -c '...'`.

**Rules of thumb for LLM callers:**

1. **Never use the bare form `agent <name>`** with no `-e`/`--seed`. It
   opens nvim and blocks.
2. **For non-trivial prompts, write the seed to a file and pass `--seed
   <path>`.** Use `Write` to drop the seed at e.g.
   `/tmp/agent-seed-<name>.md`, then
   `fish -c 'agent <name> --seed /tmp/agent-seed-<name>.md'`.
3. **Names must be kebab-case and ≤25 chars.** Longer names silently fail
   on macOS (zellij socket-name limit).
4. **`agents` is reserved** as the meta-session name — `agent agents`
   errors.
5. **Cap is 8 concurrent agents.**
6. **Repeat-spawn semantics:**
   - Worktree gone, session gone → fresh spawn.
   - Worktree exists, session gone → restart Claude in existing worktree.
   - Session exists, no meta-pane → adds the pane.
   - Session exists, meta-pane exists → focuses; if `-e/--seed` is also
     provided, the prompt is injected into the running Claude.

## Neovim keybinds (`agent_nvim` plugin, `<leader>a`)

The neovim plugin talks to per-agent (Layer-1) sessions directly — it
doesn't know or care about the meta-session. The `agents` meta-session
is filtered out of pickers.

| Key | Action |
|---|---|
| `<leader>an` | New worktree agent: name prompt → multi-line seed buffer (`<C-s>` submits) → `agent <name> --seed …`. |
| `<leader>as` | New repo session: spawn / attach a claude session rooted at the current repo, named after the repo basename. No prompts. |
| `<leader>ao` | Open/focus the agent for the current worktree. If none, picker over running sessions. |
| `<leader>av` (visual) | Send visual selection. |
| `<leader>ap` | Single-line prompt → submit. |
| `<leader>ak` | Force-kill agent (picker) — runs `agent-rm --force` and closes its tab. |
| `<leader>ar` | **+review**: the review sub-group (all review keys live here). |
| `<leader>arr` | Start / stop review. On start, pick a mode (working tree / branch / since a commit) → inline unified diff (unified.nvim). Prompts before discarding pending comments. |
| `<leader>arf` | Fuzzy-pick a changed file (diff preview); `<CR>` opens it into the inline diff, `<a-m>` marks it reviewed in place. |
| `<leader>arm` | Mark / unmark the current file as reviewed (progress readout). Reviewed files show ✓ and sink to the bottom of the file picker. |
| `<leader>arc` (n/x) | Add a comment on the current line / selection (review mode only). |
| `<leader>arl` | Jump to a pending comment: picker with preview, opens the file into the diff at the comment. |
| `<leader>are` | Edit the comment under the cursor. |
| `<leader>ard` | Delete the comment under the cursor. |
| `<leader>ars` | Page through the pending comments (`]`/`[`), then `<C-s>` to send all to the active agent and clear. |
| `]r` / `[r` | Jump to the next / previous change in the inline diff (review mode). |
| `<C-.>` (n/i/t/x) | Toggle between the agent tab and wherever you were. |

Each agent opens in its own nvim tab page with `tcd` set to the worktree
cwd, a Snacks dashboard on the left, and the zellij attach terminal on
the right. Your main work tab is unaffected. Switch between tabs with
`gt`/`gT` or the `<leader><Tab>` group (`l` for a picker, `r` to rename,
`n`/`p`/`x`/`t`/`o` for next/prev/close/new/only).

## Reviewing an agent's work

The local, iterative alternative to the PR round-trip. Lives in
`agent_nvim/lua/agent/review.lua`; you review on real, LSP-live buffers and
send comments straight to the running session.

1. Open the agent whose work you want to review (`<leader>ao` for a
   worktree agent puts you in its tab, `tcd`'d into the worktree).
2. `<leader>arr` starts review. It first asks which **mode**, i.e. what base
   to diff your working tree against:
   - **Working tree**: uncommitted changes only (base `HEAD`).
   - **Branch**: everything since the fork point (`merge-base HEAD
     origin/HEAD`); hidden on the trunk, where it's meaningless.
   - **Since a commit…**: pick a commit from a log picker; shows everything
     after it (to review one commit, pick its parent on a clean tree).
   The default is preselected (from `config.review.base`), `<cr>` accepts.
   It then renders an **inline unified diff** (unified.nvim) of the current
   file vs that base: removed lines in red, added in green, in sequence,
   GitHub-style, on the real syntax-highlighted buffer. (unified's own file
   tree is disabled in `plugins/unified.lua`; `<leader>arf` is the file
   navigator instead.) `<leader>arr` again stops review (prompts first if
   comments are still pending, since it discards them).
3. `<leader>arf` opens a snacks fuzzy picker over the changed files (with a
   diff preview) as a nicer alternative to the tree; `<CR>` opens one into
   the inline diff. `<leader>arm` (in a buffer) or `<a-m>` (inside the
   picker) marks a file reviewed: it gets a ✓ and sinks to the bottom of the
   picker (nothing is hidden), so the cursor always lands on the next
   unreviewed file. Good for large reviews; marks clear when you stop.
4. Read the code like normal: full LSP, jump to definition, edit, run.
5. `<leader>arc` on a line (or over a visual selection) → type a note in the
   floating editor. It's anchored with an extmark (tracks edits) and shown
   as a gutter sign + virtual note. `<leader>arl` lists them, `<leader>are`
   edits, and `<leader>ard` deletes the one under the cursor. `]r` / `[r`
   jump between changes in the diff. Only allowed while review is active.
6. `<leader>ars` opens a paginated preview: one comment per page, `]`/`[`
   (or `<Tab>`/`<S-Tab>`) to move through them, `<C-s>` to send all, `q` to
   cancel. On send it writes all comments (file:line + quoted code + note)
   to `.agent-review.md` at the repo root, adds it to `.git/info/exclude`,
   and tells the active agent to read it and work through each one. Comments
   clear. (Set `config.review.delivery = "inline"` to push the markdown
   straight into the prompt instead.)

While a review is active the statusline shows `review N/M · K✎` (files
marked reviewed out of changed, and pending comments).

`config.review` (in `agent/config.lua`) tunes it: `base` (`"fork"` |
`"head"`) sets which mode the picker preselects, `delivery` (`"file"` |
`"inline"`), the sign/glyph/highlights, and `review_file`.

## Typical flow

1. From any nvim instance: `<leader>zn` → name → multi-line prompt → `<C-s>`.
2. `agent.fish` resolves the main repo root, creates
   `~/worktrees/<repo>/<name>` detached at `origin/HEAD`, ensures the
   `agents` meta-session exists, and adds a pane to it that runs
   `zj <name> -- claude ...`.
3. Claude reads the seed, picks a sensible kebab-case branch, runs
   `git checkout -b`, removes the seed, and gets to work.
4. `CMD+0` to watch; `CMD+0` again to return.
5. `agent-rm <name>` when done.

## Restore semantics

The meta-session itself persists across wezterm restarts (zellij
serialization). On wezterm startup, the `gui-startup` hook checks for a
live `agents` session and, if found, spawns a passive `agents` tab in
the new window automatically — focus stays on the primary tab.

`agent --restore` covers the rarer case where a meta-session pane was
closed manually (zellij's pane-close) but its Layer-1 session is still
alive: it diffs `zellij list-sessions` against meta-session panes and
adds any missing. Also useful if you want to manually re-spawn the
agents tab in a non-startup wezterm window.

## Caveats

- **MCP prompts**: `disabledMcpjsonServers` in `claude/settings.json`
  silences known project MCP servers. Add new ones there as you encounter
  them.
- **Wezterm `--cwd` is unreliable** (wez/wezterm#4618 canonicalization).
  The agent helpers `cd` inside the spawned shell instead of trusting
  `--cwd`.
- **Auto-pin to position 0** is best-effort (race with mux registration).
  If the agents tab lands at the wrong position, `CMD+Shift+0` pins it.
- **Zellij session-name length** is capped at 25 chars on macOS by the
  unix socket path budget. `agent` rejects longer names upfront.
- **Spawning from a non-interactive shell without `-e`/`--seed`** opens
  nvim and blocks forever. Always pass a seed when scripting.
