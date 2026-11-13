# Parallel agents: workflow & keybinds

One Rust binary, `agent` (source in `~/.config/agent-tui`), runs coding agents (Claude Code today, with pluggable harnesses) in git worktrees, one zellij session per agent. Bare `agent` opens a TUI that lists every agent by repo and embeds the selected one. The subcommands are for scripts, nvim and wezterm.

Install or update it with `~/.config/agent-tui/install.sh`, which builds it into `~/.local/bin/agent`. Session layouts record that path, so keep it.

## Concepts

- **Worktree per agent** at `~/worktrees/<repo>/<name>`, checked out detached at `origin/HEAD`. The agent creates its own branch: the first prompt tells it to `git checkout -b <kebab-case-name>` first. While git is still checking the files out the worktree isn't listed, so a big repo takes a moment to appear.
- **Zellij session per agent**, named after the worktree. Sessions persist across terminal and laptop restarts and can be attached from anywhere (`zellij attach <name>`, another tab, a phone over SSH). After a reboot they show as exited. Selecting one in the TUI starts it again through `agent` (so it reports its state), resuming the harness's last conversation. Sessions `agent` doesn't own are resurrected by zellij.
- **Background Claude sessions.** A Claude session survives its terminal only if Claude Code moved it to the background. An ordinary session ends with its zellij session. Moving one to the background forks it: the worker has a new session id, runs on under Claude Code's own supervision, and the original transcript ends in `continued-in` records naming the fork. For such an agent `d` (killing the zellij session) leaves the worker running and `⏎` reattaches to it with everything intact. `agent` follows the `continued-in` records to the conversation's newest transcript, so the activity shows the live transcript and a resume uses the id that holds the messages. It finds the worker in `~/.claude/sessions/*.json` (a `kind: "bg"` entry that is not a spare, whose session id is in the conversation and whose process still has the start time the entry records, preferring one in the agent's directory) and attaches or stops it by its short job id, which `claude attach` and `claude stop` need in place of the session id. A running worker with no usable job id can't be attached or stopped, and `agent` says why and names its pid. A resurrect with a prompt for launch stops it first and resumes, since `claude attach` can't carry one. A restart (`r`, `agent restart`) always stops it: `claude stop <jobId>`, then SIGTERM to its verified pid if that fails or the process stays, and it errors if the process is still there, so the new session runs today's binary. To really quit every session: `claude stop <jobId>` for each `bg` entry in `~/.claude/sessions/*.json`, then kill the zellij sessions.
- **Repo sessions**: `agent repo` runs an agent in the repo's main checkout (no worktree), named after the repo.
- **Listed repos** stay in the TUI with nothing running in them, as one row for the main checkout, so `⏎` starts the repo session there. A repo is listed by `agent add`, the TUI's `p` picker, `agent repo`, or creating an agent in it, and `agent forget` drops it. The list lives in `~/.local/state/agent/repos`.
- **Catalogue**: `[repos] dirs` in `~/.config/agent/config.toml` names directories that are repos or hold repos (`~/repos` offers every clone in it). The `p` picker and the `n` form's repo field offer them, and picking one lists it. They aren't shown until picked.

  ```toml
  [repos]
  dirs = ["~/repos", "~/.config"]
  ```
- **Names** are ≤20 chars of `[A-Za-z0-9._-]` (the session host's limit: zellij's socket path budget on macOS). `agents` is reserved. A worktree already named longer than the limit is still listed and can be removed, and `⏎` on it says why it can't start.
- **State** lives in `~/.local/state/agent`: per-agent records (harness, undelivered first prompt), pins, zellij layouts and the TUI's last selection.

## The TUI

`agent` (or `agent tui`). Two modes: NAV moves around the list, AGENT types into the embedded session. `?` shows every key.

| Key | Action |
|---|---|
| `⏎` | In [1], attach the agent (start or resurrect it if needed) or focus the repo. On an agent that is also open in another window (a terminal tab, an editor, SSH, tmux) it reads `move here`: [0] is paused there, showing the activity under `⇄ in nvim · ⏎ move here` (or `over SSH`, `in tmux`, `in wezterm`, `in 3 windows` when they aren't all in one place, `detached window` for a client its window left behind, `elsewhere` when the place isn't known), and `⏎`, `0` or `i` in [1] on that row attaches, then closes the other window. Nothing else (`Esc`, the nav key twice, `a`, the switcher, `i` from another panel) does. In [2] and [3], open the item in [0] (on a folder in [3], open or close it). In the peek, attach |
| `i` | Attach the selected agent from any panel (on a paused agent only from [1], where it moves here) |
| `0` | Same as `⏎` in [1], [2] and [3] |
| `s` | Send a message without attaching. It is typed at once, even while the agent works, because Claude Code queues it. It is refused only while typing could land in a menu: the agent is asking (a question or permission prompt is open), booting, or its session ended, and what you typed stays for the next `s` (`Esc` keeps it, `⌃U` clears it). The box wraps and grows with the text: `⌥⏎`, `⌃J` or `⇧⏎` add a line, `⏎` sends, arrows, `Home` and `End` move the caret, `⌃W` deletes a word. Nothing is sent later (`t` says `send is s now`). A permission prompt the agent hasn't reported yet and menus opened by `/` or `!` commands aren't detected. With review comments drafted for the agent it reads `send review`: the comments go as one message after your typed overview. With the entry cursor on an entry in [0]'s activity or a subagent's log it reads `reply`: the entry's first line shows above the input and goes before your message, only when you typed one (a call quotes its tool and target, a folded phase its heading, a subagent's entry is marked `(subagent …)`). Backspace on empty text drops the quote. Drafted review comments still go after your message |
| `⏎` | With [0] on the activity: fold the entry under the cursor. On an Agent call that started a subagent it opens that subagent's log, and on an edit call or a touched file it opens the file's diff in [3]. `Esc` goes back to the activity |
| `[` `]` | With [0] on the activity: jump the entry cursor to the previous / next phase (its first entry), staying put at the ends. In a subagent's log they jump between messages |
| `y` | With [0] focused: copy to the clipboard through the terminal (OSC 52): `path:line` or the selected lines of a diff, a commit's sha, the agent's message, a subagent's log entry |
| `⏎` | With [0] on the branch: open the commit (its files and diffs) or uncommitted file under the cursor, `Esc` goes back |
| `j` `k` `[` `]` `J` `K` `V` `e` `c` | With [0] on a diff: move a line cursor, jump hunks and files, select a range (`V`), open the file at the line in $EDITOR, and comment on the line or range (`c`). Comments collect in a per-agent draft, marked `✎`, until `s` sends them |
| `v` | Peek: a read-only float of the agent's terminal. `⏎` or `i` attaches, `s` sends, `Esc` or `v` closes. On an agent open elsewhere the float names the place (`⇄ in nvim`) and is read-only through `zellij watch`, so it never resizes the other window, and `⏎` or `i` in it close it and leave the agent paused (or attach, if the other window has gone). The float closes by itself, saying `peek at X ended` and why when it can, if the session ends. It follows the other window being resized |
| `⌃\` | Back to NAV from an agent. Twice goes to the previous agent |
| `h` `l`, `⇧Tab` `Tab` | Previous / next left panel ([1], [2], [3]), also from [0] |
| `1` `2` `3` | Focus the list [1], the activity and Changes panels. Focusing [2] or [3] expands it only as far as its content needs: from the other lower panel's spare rows first, then the list, and only then does the other panel collapse to its title line (at the smallest sizes the collapse can leave it a few blank rows) |
| `Esc` | Back one level: [0] to its panel, [2] or [3] to [1], and from [1] to the agent you left |
| `j k g G`, `↓ ↑ Home End` | Move in the focused panel, or scroll [0] when it has the keys |
| `J` `K` | Scroll [0] from [2] or [3] without focusing it |
| `⌃D ⌃U`, `PgDn PgUp` | Half a page / a page in the focused panel |
| `[` `]` | Previous / next tab of the focused panel: [1] (agents, repo, pinned, opens on agents) or [3] (`Uncommitted`, then `Branch` or `Unpushed`, opens on uncommitted) |
| `{` `}` | Previous / next repo |
| `+` `_` | [0] full width / back |
| `.` | In [1], show every checkout / only those with a session |
| `Space` / `/` | Switcher (fuzzy), `⌃N ⌃P` to move |
| `m` | Pin / unpin (pinned agents get their own tab) |
| `n` | New agent form: prompt (`⌃E` edits it in $EDITOR), name (suggested from the prompt or a Linear/GitHub link), repo (listed, from the catalogue, or a path) |
| `c` | In [1], [2] and [3]: check out for review: a branch name or a GitHub PR link, name (suggested from it), repo (a PR link picks the repo it names). Fetches the branch if git doesn't know it, asks `gh` for a PR's branch (a fork's PR lands on a `pr/N` branch), and attaches instead if an agent already has the branch checked out |
| `p` | Repo picker: `⏎` focuses a listed repo, or adds one from the catalogue or by a typed path (`~/code/foo`) |
| `` ` `` | In [3], switch between the directory tree (default) and the flat file list |
| `-` `=` | In [3]'s tree, close / open every folder |
| `e` | Edit: the agent's checkout in $EDITOR, in [3] the file (or folder) under the cursor, in [2] a subagent's transcript, and in [0] on a subagent's log or the activity, the transcript as Markdown at the entry under the cursor |
| `d` / `D` | Kill the session / remove session, worktree and branch (confirms, lists anything that would be lost) |
| `r` / `R` | Restart the agent / every agent: kill the session, stop the harness process that outlived it so the installed version runs, then start it again resuming its conversation. `r` asks only when it would interrupt the agent or close another window showing it. `R` always asks, and `i` in it restarts only the idle agents. An exited agent whose harness still runs has it stopped and stays exited. Its card reads `restarting` until the restart finishes |
| `x` | Run a configured action on the selected agent (see below) |
| `a` | Attach the agent that needs you most (asking, failed, then finished) |
| `?` | Keybindings: the focused panel's keys first, then the global ones. Type to filter, `⏎` keeps the filter, `Esc` clears it while typing, and `?`, `q` or `Esc` close |
| `q` | Quit (agents keep running) |

The footer shows as many hints as fit, most important for the focused panel first.

### What agents are doing

Agents launched by `agent` report their state through Claude Code hooks passed with `--settings` (your own settings stay untouched), recorded by `agent event` in `~/.local/state/agent/activity/`.

- `◆` asking you (a permission prompt or a question)
- `!` failed (rate limited, overloaded)
- `✓` finished an activity you haven't looked at
- spinner: working, including a finished activity still waiting on background subagents (`↳n` counts them). A launched agent that has reported nothing yet shows the spinner and `starting` for at most 30 seconds, and for 5 seconds after Claude Code reports its session starting. A subagent with no tool running and no question open that nothing has been heard from for 5 minutes is lost: it drops out of `↳n`, and an agent waiting only on lost ones says `↳n lost`. It stops, dimmed with `?`, after 5 minutes of silence with no tool running
- `⌁n` after the name: background commands the agent has running (Claude Code's background shells, such as a dev server or a watcher). It never changes the state, so a finished agent stays `✓` while they run. The activity's status row counts them too (`↳2 · ⌁1`). An idle agent with some says so on its second line, `⌁ cargo test --workspace · 4m+` (the `+` means at least that long, as of the last listing: an idle agent isn't listed again, so it may have ended), and an exited agent shows none.
- `■` interrupted: you pressed Esc and nothing has followed (Claude Code fires no hook for it, so `agent` reads it from the transcript)
- `●` idle, `○` exited, `·` no session

The Agents tab opens with a **Needs you** group, the status bar and terminal title show `◆ ! ✓` counts, and a bell rings when an agent you're not typing into starts waiting. Each agent's card has a second line saying what it's doing or what it said (the question, the tool, the last message), when there's room. Under the list, panel [2] lists the activity, its subagents and, under a `commands` heading, its background commands, and panel [3] lists the work in two tabs, `[` `]` to switch: `Uncommitted` (the default) lists only files with uncommitted state counted against the last commit, and `Branch` what this branch changes compared with where it left origin/main, like a PR plus your uncommitted work (on the trunk the tab is `Unpushed`: commits not yet on origin plus uncommitted work, which a push empties). Both panels stay on screen while an agent is selected, and with nothing to list a panel's one row says why: `no changes`, `reading git…`, git's error, or `no activity recorded`. The second tab's first row is a short branch summary (`↑27 · 4 uncommitted`, `↓n` when behind), and hovering it shows the branch in [0]: titled `Unpushed · main vs origin/main` or `Branch · fix-thing vs origin/main`, then its commits and uncommitted files. `Uncommitted` lists only its files and the panel's corner counts them. Files carry lazygit's status code (staged letter green, unstaged red, `??` untracked, nothing for committed-only files, a `●` on folders holding uncommitted files), a `∩` on any file another agent in the repo changed too, and `∩ n` in the title counting them. The right pane [0] shows what the cursor in the focused panel is on: the agent's terminal for [1], the activity document, a subagent's log or a background command's full command, age and id for [2] (a status row, the prompt, what it said and a row per call with failures spelled out, its report first once finished; it follows the tail until you scroll up and `G` resumes; with [0] focused `j` `k` move over entries, `⏎` expands one and `=` `-` expand or collapse all). The activity row reads the main transcript the same way: its current activity cut at your last prompt, the agent's words and calls with every phase but the latest folded to one `▸` row, the whole question pinned on top while it asks, its closing message once done, and a `since you looked` marker. An agent whose harness keeps no transcript shows the prompt, message and subagents instead, and the branch, a file's diff, or a folder's files with each diff under its own header (up to 20 files) for [3], with who else changed the file in its title. Clicking a row of [2] or [3] puts the cursor there, and the wheel scrolls. When the selected agent asks while you read, a banner at the top of [0] says so and `i` answers it (the activity view pins the whole question instead). An agent whose terminal isn't showing still counts for `a`, the bell and the status counts, and doesn't clear its `✓` while you read its diff. Agents started before this existed report nothing until restarted: `r` in the TUI (or `agent restart <name>`) restarts one through `agent`, resuming its conversation. `d` then `⏎` doesn't, because if Claude Code kept the session running in the background it reattaches to the old process.

### Actions

Actions live in `~/.config/agent/config.toml`. They see the agent only through the `{name}`, `{path}` (its checkout) and `{repo}` (main checkout) placeholders in their arguments, and `{{`/`}}` are literal braces. Each one does one of these:

- `run`: starts a command (an argv with no shell) detached, with failures shown in the status bar.
- `interactive`: hands the TUI's terminal to a command until it exits.
- `terminal_event`: sends the terminal a user variable (`OSC 1337 SetUserVar`) for its config to handle.


```toml
[[action]]
key = "d"
label = "dev server"
terminal_event = { name = "agent-spawn-dev", value = "{path}" }

[[action]]
key = "t"
label = "nvim in tab"
terminal_event = { name = "agent-edit", value = "tab|{path}" }

[[action]]
key = "g"
label = "lazygit"
interactive = ["lazygit"]
cwd = "{path}"
```

Commands run in `cwd` if it's set, otherwise in the directory `agent` was started in.

The key that leaves an agent lives in the same file. It must be `ctrl-` plus a letter or one of `\ ] ^ _`:

```toml
[keys]
nav = "ctrl-\\"
```

Keys are one printable character (`Esc` closes the overlay). The config is checked at startup, and mistakes stop `agent` with the reason.

### Theme

Colours follow the terminal's palette: the defaults are its ANSI cyan, yellow, red and green. Secondary text is your foreground blended into your background, which `agent` asks the terminal for at startup (`OSC 10`/`OSC 11`), with enough foreground to stay readable on light themes. It's asked once, so restart `agent` after changing theme. A terminal that doesn't answer, or doesn't set `COLORTERM=truecolor`, gets its faint attribute instead. `[theme]` overrides any role with `#rrggbb`, `default` (the terminal's foreground) or an ANSI name (`cyan`, `bright-blue`):

```toml
[theme]
accent = "#89b4fa"   # focused borders and titles, the selection, badges
dim = "#6c7086"      # secondary text, or "faint" for the terminal's own
warning = "yellow"   # asking you, shared terminals, files others changed
error = "red"        # failures, errors, removed lines
success = "green"    # finished activities, added lines
```

`NO_COLOR` turns every colour off, whatever `[theme]` says.

## Wezterm

| Key | Action |
|---|---|
| `CMD+0` | Toggle the `agent` tab (runs the TUI, started on first use) |
| `CMD+Shift+0` | Pin the `agent` tab to position 0 |
| `CMD+Shift+O` | Worktree picker: a shell tab in a `~/worktrees/<repo>/<branch>` dir |
| `CMD+Left` / `CMD+Right` | Cycle tabs, skipping the `agent` tab |
| `Shift+Alt+{` / `}` | Move tabs (the `agent` tab stays put) |

The `agent-edit` event (`wezterm/agent_edit.lua`) backs the `t` (nvim in tab) and `r` (nvim right) actions in `~/.config/agent/config.toml`, run with `x` then the key. The payload is `<tab|right>|<cwd>`, and `$EDITOR` opens there in a new tab or in a split right of the TUI. The handler runs on the machine whose wezterm draws the TUI and refuses a `cwd` that isn't a directory there, so over SSH these actions only say so.

## Commands

`agent <cmd> --help` has the details. Fish completions come from `agent completions fish`.

| Command | Description |
|---|---|
| `agent attach <name>` | Create or attach the agent. A new one asks for its first prompt in $EDITOR when interactive. Without a terminal (scripts, an LLM's shell) it starts the session detached instead of attaching. |
| `agent attach <name> -e "<prompt>"` | Same, with the prompt inline. If the agent is running, the prompt is typed into it, or nothing is sent and it exits non-zero while the agent is asking, booting or ended. |
| `agent attach <name> --seed <file>` | Same, prompt from a file. Preferred for non-trivial prompts. |
| `agent attach <name> --no-prompt` | Start without a prompt. |
| `agent attach <name> --repo <path>` | Repo for a new worktree (default: the one containing the cwd). |
| `agent attach … --headless` | Prepare everything but print `headless_session:`, `headless_cwd:` and `headless_cmd:` (fish-quoted) instead of attaching. |
| `agent checkout <branch>` / `agent co` | Attach the worktree for a branch, creating it from the local or `origin/` branch (fetched first if unknown). The name is the branch's words joined with `-`, cut at the last word that fits 20 characters (`--name` overrides). |
| `agent repo` | Attach or start the repo session. Run inside an agent worktree, it attaches that agent instead, even with `--repo`. |
| `agent restart [<name>] [--force]` | The same as `r`, for the agent in the cwd or the name. Without a terminal a busy agent is refused unless `--force`. Run inside the agent's own session, it restarts that agent last from a detached process whose failures go to the error log. At a terminal that asks first when the own session is busy, and without one it always restarts it. Refused while a process of the conversation is open in another terminal |
| `agent restart --all [--force \| --idle]` | The same as `R`, and it restarts the own session last (`restart 2 · this session last`). At a terminal it lists what it would do and asks. Without one it proceeds only if no agent is busy, unless `--force` (interrupt them) or `--idle` (leave out the busy ones, the own session included). Exits 1 if any agent failed |
| `agent rm [<name>]` | Remove session, worktree and branch (name inferred from the cwd). Refuses if anything would be lost: uncommitted files, unpushed or unmerged commits. Run from inside the agent's own session, it finishes in the background. |
| `agent rm --force [<name>]` | Same, discarding work. |
| `agent rm --all [-f]` | Every agent worktree. Confirms first. |
| `agent ls` | Agents by repo with their state. |
| `agent add [<path>]` | Keep the repo holding the path (default: the cwd) listed in the TUI with nothing running in it. |
| `agent forget [<path>]` | Drop it from the list. It stays while sessions or worktrees belong to it. |
| `agent merged` | Worktrees whose branch can be removed without losing work. |
| `agent prompt <name> <text>` / `--file` | Type a prompt into a running agent and submit it. Exits non-zero with nothing sent while the agent is asking, booting or ended. |
| `agent dump <name>` | Print an agent's screen with ANSI styling. |
| `agent pin [<name>]` / `agent unpin` | Pin to the top of the TUI. |
| `agent --harness <id> …` | Harness for new agents (default `claude`, and `$AGENT_CLAUDE` overrides its program). |

## Spawning from an LLM (Claude Code's `Bash` tool, etc.)

`agent` is a binary on PATH, so call it from any shell.

1. **Pass `-e`, `--seed` or `--no-prompt`** on a fresh spawn. Without a terminal, `agent attach` starts the session detached, prints `agent: <name> is running` and returns.
2. **For non-trivial prompts, write the seed to a file and pass `--seed`**, e.g. `/tmp/agent-seed-<name>.md`, then `agent attach <name> --seed /tmp/agent-seed-<name>.md`. Don't pass `--headless`: it only prepares the session for the caller to run.
3. **Names must fit the rules above.** `agent` rejects others with the reason.
4. **Repeat-attach semantics:** a running agent gets `-e`/`--seed` typed into it, and an exited session or idle worktree gets a new session that resumes its conversation.

## Neovim keybinds (`agent_nvim` plugin, `<leader>a`)

The neovim plugin runs the `agent` binary for spawning, removing and previews, and attaches to the zellij sessions directly.

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

Each agent opens in its own nvim tab page with `tcd` set to the worktree cwd, a Snacks dashboard on the left, and the zellij attach terminal on the right. Your main work tab is unaffected. Switch between tabs with `gt`/`gT` or the `<leader><Tab>` group (`l` for a picker, `r` to rename, `n`/`p`/`x`/`t`/`o` for next/prev/close/new/only).

## Reviewing an agent's work

The local, iterative alternative to the PR round-trip. Lives in `agent_nvim/lua/agent/review.lua`. You review on real, LSP-live buffers and send comments straight to the running session.

1. Open the agent whose work you want to review (`<leader>ao` for a worktree agent puts you in its tab, `tcd`'d into the worktree).
2. `<leader>arr` starts review. It first asks which **mode**, i.e. what base to diff your working tree against:
   - **Working tree**: uncommitted changes only (base `HEAD`).
   - **Branch**: everything since the fork point (`merge-base HEAD origin/HEAD`), hidden on the trunk, where it's meaningless.
   - **Since a commit…**: pick a commit from a log picker to see everything after it (to review one commit, pick its parent on a clean tree). The default is preselected (from `config.review.base`), `<cr>` accepts. It then renders an **inline unified diff** (unified.nvim) of the current file vs that base: removed lines in red, added in green, in sequence, GitHub-style, on the real syntax-highlighted buffer. (unified's own file tree is disabled in `plugins/unified.lua`, and `<leader>arf` is the file navigator instead.) `<leader>arr` again stops review (prompts first if comments are still pending, since it discards them).
3. `<leader>arf` opens a snacks fuzzy picker over the changed files (with a diff preview) as a nicer alternative to the tree. `<CR>` opens one into the inline diff. `<leader>arm` (in a buffer) or `<a-m>` (inside the picker) marks a file reviewed: it gets a ✓ and sinks to the bottom of the picker (nothing is hidden), so the cursor always lands on the next unreviewed file. Good for large reviews. Marks clear when you stop.
4. Read the code like normal: full LSP, jump to definition, edit, run.
5. `<leader>arc` on a line (or over a visual selection) → type a note in the floating editor. It's anchored with an extmark (tracks edits) and shown as a gutter sign + virtual note. `<leader>arC` adds a whole-file comment (not tied to a line, rendered as a header above line 1) for feedback about the file as a whole. `<leader>arg` writes an overview message for the review as a whole (prepended to what's sent). `<leader>arl` lists comments, `<leader>are` edits, and `<leader>ard` deletes the one under the cursor. `]r` / `[r` jump between changes in the diff. Only allowed while review is active.
6. `<leader>ars` opens a paginated preview: the overview message (if any) then one comment per page, `]`/`[` (or `<Tab>`/`<S-Tab>`) to move through them, `<C-s>` to send all, `q` to cancel. On send it writes the overview plus all comments (file:line + quoted code + note, with whole-file comments as headers) to `.agent-review.md` at the repo root, adds it to `.git/info/exclude`, and tells the active agent to read it and work through each one. Sending ends the review (the diff closes and pending state clears). (Set `config.review.delivery = "inline"` to push the markdown straight into the prompt instead.)

While a review is active the statusline shows `review N/M · K✎` (files marked reviewed out of changed, and pending comments).

A review (base, comments, marked files) is saved on exit and restored per repo: if you close nvim mid-review, the next `<leader>arr` in that repo offers to resume it. Stopping a review clears its saved state. While the comment editor is open, the lines you're commenting on are highlighted in the buffer above (`config.review.editing_hl`).

`config.review` (in `agent/config.lua`) tunes it: `base` (`"fork"` | `"head"`) sets which mode the picker preselects, `delivery` (`"file"` | `"inline"`), the sign/glyph/highlights, and `review_file`.

## Typical flow

1. `CMD+Shift+A` (or `n` in the TUI, or `<leader>an` in nvim) → prompt → name (suggested) → repo.
2. `agent` creates `~/worktrees/<repo>/<name>` detached at `origin/HEAD`, writes the session layout and starts the harness with the prompt.
3. The agent picks a branch name, runs `git checkout -b` and gets to work.
4. `CMD+0` to watch it in the TUI, and `CMD+0` again to return.
5. `D` in the TUI (or `agent rm <name>`) when done.
