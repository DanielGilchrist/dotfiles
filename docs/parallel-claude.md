# Parallel agents: workflow & keybinds

One Rust binary, `agent` (source in `~/.config/agent-tui`), runs coding
agents (Claude Code today, with pluggable harnesses) in git worktrees, one
zellij session per agent. Bare `agent` opens a TUI that lists every agent
by repo and embeds the selected one. The subcommands are for scripts,
nvim and wezterm.

Install or update it with `~/.config/agent-tui/install.sh`, which builds
it into `~/.local/bin/agent`. Session layouts record that path, so keep it.

## Concepts

- **Worktree per agent** at `~/worktrees/<repo>/<name>`, checked out
  detached at `origin/HEAD`. The agent creates its own branch: the first
  prompt tells it to `git checkout -b <kebab-case-name>` first.
- **Zellij session per agent**, named after the worktree. Sessions persist
  across terminal and laptop restarts and can be attached from anywhere
  (`zellij attach <name>`, another tab, a phone over SSH). After a reboot
  they show as exited. Selecting one in the TUI starts it again through
  `agent` (so it reports its state), resuming the harness's last
  conversation. Sessions `agent` doesn't own are resurrected by zellij.
- **Repo sessions**: `agent repo` runs an agent in the repo's main checkout
  (no worktree), named after the repo.
- **Listed repos** stay in the TUI with nothing running in them, as one row
  for the main checkout, so `⏎` starts the repo session there. A repo is
  listed by `agent add`, the TUI's `p` picker, `agent repo`, or creating an
  agent in it, and `agent forget` drops it. The list lives in
  `~/.local/state/agent/repos`.
- **Catalogue**: `[repos] dirs` in `~/.config/agent/config.toml` names
  directories that are repos or hold repos (`~/repos` offers every clone in
  it). The `p` picker and the `n` form's repo field offer them, and picking
  one lists it. They aren't shown until picked.

  ```toml
  [repos]
  dirs = ["~/repos", "~/.config"]
  ```
- **Names** are ≤20 chars of `[A-Za-z0-9._-]` (the zellij socket path limit
  on macOS). `agents` is reserved.
- **State** lives in `~/.local/state/agent`: per-agent records (harness,
  undelivered first prompt), pins, zellij layouts and the TUI's last
  selection.

## The TUI

`agent` (or `agent tui`). Two modes: NAV moves around the list, AGENT
types into the embedded session. `?` shows every key.

| Key | Action |
|---|---|
| `⏎` / `l` | Attach the selected agent (resurrect or start it if needed) |
| `⌃\` | Back to NAV from an agent. Twice goes to the previous agent |
| `1` `2` `3` | Focus the Agents, turn and Changes panels. Focusing [2] or [3] expands it and shrinks the others, lazygit-style. From [2] or [3], `Esc` goes back to Agents |
| `j k g G`, `[ ]` | Move in the focused panel, previous / next repo |
| `⌃D ⌃U` | Half a page in the focused panel, handy in Changes |
| `Space` / `/` | Switcher (fuzzy), `⌃N ⌃P` to move |
| `m` | Pin / unpin (pinned agents get their own group) |
| `n` | New agent form: prompt (`⌃E` edits it in $EDITOR), name (suggested from the prompt or a Linear/GitHub link), repo (listed, from the catalogue, or a path) |
| `p` | Repo picker: `⏎` focuses a listed repo, or adds one from the catalogue or by a typed path (`~/code/foo`) |
| `e` | Open the agent's worktree in $EDITOR, or in Changes the file under the cursor |
| `r` | Reply without attaching, once its turn is over and nothing is asking |
| `d` / `D` | Kill the session / remove session, worktree and branch (confirms, lists anything that would be lost) |
| `x` | Run a configured action on the selected agent (see below) |
| `a` | Jump to the agent that needs you most (asking, failed, then finished), into AGENT |
| `q` | Quit (agents keep running) |

### What agents are doing

Agents launched by `agent` report their state through Claude Code hooks
passed with `--settings` (your own settings stay untouched), recorded by
`agent event` in `~/.local/state/agent/activity/`.

- `◆` asking you (a permission prompt or a question)
- `!` failed (rate limited, overloaded)
- `✓` finished a turn you haven't looked at
- spinner: working, including a finished turn still waiting on background subagents (`↳n` counts them). It stops, dimmed with `?`, after 5 minutes of silence with no tool running
- `●` idle, `○` exited, `·` no session

Overview opens with a **Needs you** group, the status bar and terminal
title show `◆ ! ✓` counts, and a bell rings when an agent you're not
typing into starts waiting. Each agent's card has a second line saying
what it's doing or what it said (the question, the tool, the last
message), when there's room. Under the list, panel [2] is the turn: your
last prompt, the agent's message or question in full, and the subagents
it's waiting on. When it doesn't fit the last row says `+k more` and `2`
focuses the panel to read it all. Panel [3] is the work: whether it's
ready to merge (`2 uncommitted · ↑2`), the files it changed, a `∩` on any
file another agent in the repo changed too, and `∩ n` in the title
counting them. Focus [3] and put the cursor on one and the border says
who.
Agents started before this existed report nothing until restarted: `d`
then `⏎` in the TUI restarts one through `agent`, resuming its conversation.

### Actions

Actions live in `~/.config/agent/config.toml`. They see the agent only
through the `{name}`, `{path}` (its checkout) and `{repo}` (main checkout)
placeholders in their arguments, and `{{`/`}}` are literal braces. Each one
does one of these:

- `run`: starts a command (an argv with no shell) detached, with failures
  shown in the status bar.
- `interactive`: hands the TUI's terminal to a command until it exits.
- `terminal_event`: sends the terminal a user variable (`OSC 1337
  SetUserVar`) for its config to handle.


```toml
[[action]]
key = "d"
label = "dev server"
terminal_event = { name = "agent-spawn-dev", value = "{path}" }

[[action]]
key = "g"
label = "lazygit"
interactive = ["lazygit"]
cwd = "{path}"
```

Commands run in `cwd` if it's set, otherwise in the directory `agent` was
started in.

The key that leaves an agent lives in the same file. It must be `ctrl-`
plus a letter or one of `\ ] ^ _`:

```toml
[keys]
nav = "ctrl-\\"
``` Keys are one printable character (`Esc` closes
the overlay). The config is checked at startup, and mistakes stop `agent` with
the reason.

## Wezterm

| Key | Action |
|---|---|
| `CMD+0` | Toggle the agents tab (runs the TUI, started on first use) |
| `CMD+Shift+0` | Pin the agents tab to position 0 |
| `CMD+Shift+O` | Worktree picker: a shell tab in a `~/worktrees/<repo>/<branch>` dir |
| `CMD+Left` / `CMD+Right` | Cycle tabs, skipping the agents tab |
| `Shift+Alt+{` / `}` | Move tabs (the agents tab stays put) |

## Commands

`agent <cmd> --help` has the details. Fish completions come from
`agent completions fish`.

| Command | Description |
|---|---|
| `agent attach <name>` | Create or attach the agent. A new one asks for its first prompt in $EDITOR when interactive. Without a terminal (scripts, an LLM's shell) it starts the session detached instead of attaching. |
| `agent attach <name> -e "<prompt>"` | Same, with the prompt inline. If the agent is running, the prompt is typed into it. |
| `agent attach <name> --seed <file>` | Same, prompt from a file. Preferred for non-trivial prompts. |
| `agent attach <name> --no-prompt` | Start without a prompt. |
| `agent attach <name> --repo <path>` | Repo for a new worktree (default: the one containing the cwd). |
| `agent attach … --headless` | Prepare everything but print `headless_session:`, `headless_cwd:` and `headless_cmd:` (fish-quoted) instead of attaching. |
| `agent checkout <branch>` / `agent co` | Attach the worktree for a branch, creating it from the local or `origin/` branch. Name is the branch with `/` → `-`. |
| `agent repo` | Attach or start the repo session. Run inside an agent worktree, it attaches that agent instead, even with `--repo`. |
| `agent rm [<name>]` | Remove session, worktree and branch (name inferred from the cwd). Refuses if anything would be lost: uncommitted files, unpushed or unmerged commits. Run from inside the agent's own session, it finishes in the background. |
| `agent rm --force [<name>]` | Same, discarding work. |
| `agent rm --all [-f]` | Every agent worktree. Confirms first. |
| `agent ls` | Agents by repo with their state. |
| `agent add [<path>]` | Keep the repo holding the path (default: the cwd) listed in the TUI with nothing running in it. |
| `agent forget [<path>]` | Drop it from the list. It stays while sessions or worktrees belong to it. |
| `agent merged` | Worktrees whose branch can be removed without losing work. |
| `agent prompt <name> <text>` / `--file` | Type a prompt into a running agent and submit it. |
| `agent dump <name>` | Print an agent's screen with ANSI styling. |
| `agent pin [<name>]` / `agent unpin` | Pin to the top of the TUI. |
| `agent --harness <id> …` | Harness for new agents (default `claude`, and `$AGENT_CLAUDE` overrides its program). |

## Spawning from an LLM (Claude Code's `Bash` tool, etc.)

`agent` is a binary on PATH, so call it from any shell.

1. **Pass `-e`, `--seed` or `--no-prompt`** on a fresh spawn. Without a
   terminal, `agent attach` starts the session detached, prints
   `agent: <name> is running` and returns.
2. **For non-trivial prompts, write the seed to a file and pass `--seed`**,
   e.g. `/tmp/agent-seed-<name>.md`, then
   `agent attach <name> --seed /tmp/agent-seed-<name>.md`. Don't pass
   `--headless`: it only prepares the session for the caller to run.
3. **Names must fit the rules above.** `agent` rejects others with the reason.
4. **Repeat-attach semantics:** a running agent gets `-e`/`--seed` typed
   into it, and an exited session or idle worktree gets a new session that
   resumes its conversation.

## Neovim keybinds (`agent_nvim` plugin, `<leader>a`)

The neovim plugin runs the `agent` binary for spawning, removing and
previews, and attaches to the zellij sessions directly.

| Key | Action |
|---|---|
| `<leader>an` | New worktree agent: name prompt → multi-line seed buffer (`<C-s>` submits) → `agent attach <name> --seed … --headless`. The session runs in an nvim tab and also shows up in the TUI. |
| `<leader>aN` | Same as `<leader>an` but skips the seed buffer, so Claude starts with no prompt. |
| `<leader>ac` | Branch picker (local + remote). Attaches an existing worktree for the pick, or creates one and spawns a bare agent. |
| `<leader>as` | Repo session: `agent repo --headless`, an agent in the repo's main checkout named after the repo. No prompts. From inside an agent's worktree it opens that agent. |
| `<leader>ao` | Open/focus the agent for the current worktree. If none, picker over running sessions. |
| `<leader>av` (visual) | Send visual selection. |
| `<leader>ap` | Single-line prompt → submit. |
| `<leader>ak` | Force-remove agent (picker): runs `agent rm --force` and closes its tab. |
| `<leader>ar` | **+review**: the review sub-group (all review keys live here). |
| `<leader>arr` | Start / stop review. On start, pick a mode (working tree / branch / since a commit) → inline unified diff (unified.nvim), or resume a review left open from a previous session. Prompts before discarding pending comments. |
| `<leader>arf` | Fuzzy-pick a changed file (diff preview). `<CR>` opens it into the inline diff, `<a-m>` marks it reviewed and jumps to the next file. |
| `<leader>arm` | Mark / unmark the current file as reviewed (progress readout). Reviewed files show ✓ and sink to the bottom of the file picker. |
| `<leader>arc` (n/x) | Add a comment on the current line / selection (review mode only). |
| `<leader>arC` | Add a whole-file comment (not anchored to a line) on the current buffer. |
| `<leader>arg` | Write / edit an overview message for the whole review (prepended when sent). |
| `<leader>arl` | Jump to a pending comment: picker with preview, opens the file into the diff at the comment. |
| `<leader>are` | Edit the comment under the cursor. |
| `<leader>ard` | Delete the comment under the cursor. |
| `<leader>ars` | Page through the pending comments (`]`/`[`), then `<C-s>` to send all to the active agent. Sending ends the review. |
| `]r` / `[r` | Jump to the next / previous change in the inline diff (review mode). |
| `<C-.>` (n/i/t/x) | Toggle between the agent tab and wherever you were. |

Each agent opens in its own nvim tab page with `tcd` set to the worktree
cwd, a Snacks dashboard on the left, and the zellij attach terminal on
the right. Your main work tab is unaffected. Switch between tabs with
`gt`/`gT` or the `<leader><Tab>` group (`l` for a picker, `r` to rename,
`n`/`p`/`x`/`t`/`o` for next/prev/close/new/only).

## Reviewing an agent's work

The local, iterative alternative to the PR round-trip. Lives in
`agent_nvim/lua/agent/review.lua`. You review on real, LSP-live buffers and
send comments straight to the running session.

1. Open the agent whose work you want to review (`<leader>ao` for a
   worktree agent puts you in its tab, `tcd`'d into the worktree).
2. `<leader>arr` starts review. It first asks which **mode**, i.e. what base
   to diff your working tree against:
   - **Working tree**: uncommitted changes only (base `HEAD`).
   - **Branch**: everything since the fork point (`merge-base HEAD
     origin/HEAD`), hidden on the trunk, where it's meaningless.
   - **Since a commit…**: pick a commit from a log picker to see everything
     after it (to review one commit, pick its parent on a clean tree).
   The default is preselected (from `config.review.base`), `<cr>` accepts.
   It then renders an **inline unified diff** (unified.nvim) of the current
   file vs that base: removed lines in red, added in green, in sequence,
   GitHub-style, on the real syntax-highlighted buffer. (unified's own file
   tree is disabled in `plugins/unified.lua`, and `<leader>arf` is the file
   navigator instead.) `<leader>arr` again stops review (prompts first if
   comments are still pending, since it discards them).
3. `<leader>arf` opens a snacks fuzzy picker over the changed files (with a
   diff preview) as a nicer alternative to the tree. `<CR>` opens one into
   the inline diff. `<leader>arm` (in a buffer) or `<a-m>` (inside the
   picker) marks a file reviewed: it gets a ✓ and sinks to the bottom of the
   picker (nothing is hidden), so the cursor always lands on the next
   unreviewed file. Good for large reviews. Marks clear when you stop.
4. Read the code like normal: full LSP, jump to definition, edit, run.
5. `<leader>arc` on a line (or over a visual selection) → type a note in the
   floating editor. It's anchored with an extmark (tracks edits) and shown
   as a gutter sign + virtual note. `<leader>arC` adds a whole-file comment
   (not tied to a line, rendered as a header above line 1) for feedback about
   the file as a whole. `<leader>arg` writes an overview message for the
   review as a whole (prepended to what's sent). `<leader>arl` lists comments,
   `<leader>are` edits, and `<leader>ard` deletes the one under the cursor.
   `]r` / `[r` jump between changes in the diff. Only allowed while review is
   active.
6. `<leader>ars` opens a paginated preview: the overview message (if any) then
   one comment per page, `]`/`[` (or `<Tab>`/`<S-Tab>`) to move through them,
   `<C-s>` to send all, `q` to cancel. On send it writes the overview plus all
   comments (file:line + quoted code + note, with whole-file comments as headers)
   to `.agent-review.md` at the repo root, adds it to `.git/info/exclude`, and
   tells the active agent to read it and work through each one. Sending ends
   the review (the diff closes and pending state clears). (Set
   `config.review.delivery = "inline"` to push the markdown straight into the
   prompt instead.)

While a review is active the statusline shows `review N/M · K✎` (files
marked reviewed out of changed, and pending comments).

A review (base, comments, marked files) is saved on exit and restored per
repo: if you close nvim mid-review, the next `<leader>arr` in that repo
offers to resume it. Stopping a review clears its saved state. While the
comment editor is open, the lines you're commenting on are highlighted in
the buffer above (`config.review.editing_hl`).

`config.review` (in `agent/config.lua`) tunes it: `base` (`"fork"` |
`"head"`) sets which mode the picker preselects, `delivery` (`"file"` |
`"inline"`), the sign/glyph/highlights, and `review_file`.

## Typical flow

1. `CMD+Shift+A` (or `n` in the TUI, or `<leader>an` in nvim) → prompt →
   name (suggested) → repo.
2. `agent` creates `~/worktrees/<repo>/<name>` detached at `origin/HEAD`,
   writes the session layout and starts the harness with the prompt.
3. The agent picks a branch name, runs `git checkout -b` and gets to work.
4. `CMD+0` to watch it in the TUI, and `CMD+0` again to return.
5. `D` in the TUI (or `agent rm <name>`) when done.
