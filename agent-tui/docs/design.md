# agent TUI design

Distilled from two research briefs (interaction, visual) on lazygit, helix, k9s, yazi, zellij, tmux, gitui, television and atuin. Owner decisions override the briefs where they conflict, and are marked **(owner)**. Every visual or interaction change is reviewed against this doc by adversarial design reviewers.

## Principles

1. **One hint line, chosen by context. The full map lives behind `?`.** lazygit's `showBottomLine`, k9s's per-view menu, gitui's "context based help". Never a static hint block in a sidebar.
2. **The mode is stated in text, not only colour.** helix `NOR`/`INS`, zellij's status bar. We have two modes: `AGENT` (keys go to the pty) and `NAV` (keys go to the app).
3. **The escape key is a prefix, like tmux's `C-b`.** `nav` (default `Ctrl+\`, configurable under `[keys]` so a harness that claims it can be worked around) enters NAV, and any NAV action that targets an agent drops back into AGENT on that agent. Twice in a row means "previous agent" (tmux `last-window`). The owner tried a one-shot mode plus a second sustained key, like AeroSpace's, and dropped it: no second Ctrl chord was easier to reach than the first.
4. **Escaping is free to undo.** `Esc` in NAV returns to the agent you left.
5. **Attention is a per-agent flag plus a jump key** (tmux `#`/`!` flags, `next-window -a`). Source: hooks the harness is launched with (Claude: `--settings` carrying `UserPromptSubmit`, `PreToolUse`/`PostToolUse`, `PermissionRequest`, `Stop`, `StopFailure`, `SessionEnd`), reported through `agent event` into the state dir. `idle_prompt` is ignored because it fires while background work still runs.
6. **Confirmation strength matches what is lost.** One vocabulary: `y` confirms, `Y` confirms data loss, `Esc`/`n` cancels.
7. **Respect the user's palette.** Named ANSI colours only, `Color::Reset` backgrounds, `DIM` (not `DarkGray`) for secondary text, because brblack is the background in Solarized.
8. **Selection never depends on span colours.** No `REVERSED`, no background fill: a 1-column `▌` gutter (Cyan when the list has focus, DIM otherwise) plus a BOLD name.
9. **Whitespace and single rules over boxes.** Boxes only for overlays.
10. **Colour encodes state and is never alone**: every state has a distinct glyph, so it reads with `NO_COLOR` or a monochrome theme. No Nerd Font dependency.

## Layout (owner: lazygit-style panels, modern status bar)

```
╭ heucristic ───────────────╮╭ heucristic / triage-flow ───────────────────────╮
│▌◆ triage-flow           1 ││ <embedded terminal>                             │
│ ● payroll-fix             ││                                                 │
│ ✓ main                    ││                                                 │
╰─────────────────── 1 of 3─╯╰─────────────────────── ~/worktrees/heucristic/… ╯
  NAV    ⏎ attach  Space switch  d kill  D remove  m pin  Tab overview  ? keys      killed foo
```

- Rounded panels, like lazygit: the sidebar's three (see Sidebar) and the selected agent's terminal. Each has its title in the top border and its metadata in the bottom-right corner (`N of M` for the list, the path or a Yellow `⇄ shared · ⏎ take over` for the terminal).
- The panel keys go to has a Cyan border (the focused sidebar panel in NAV, the terminal in AGENT). The others are DIM, and all are DIM while an overlay has the keys. The owner rejected free-floating rules and a titles-in-rules frame. Borders belong to panels.
- Sidebar: 24% of the width, clamped to 28-48 inner columns, plus its border. It's hidden below 60 columns so the agent keeps every column (Space still switches). Rows keep one column of inner padding on both edges. The terminal panel has one column of horizontal padding so the agent's own full-width rules don't fuse with our border.
- List title: `[1]` and the scope BOLD (`heucristic` in Focus, `Agents` in Overview). Position lives only in the `N of M` corner, in the border's colour and tucked against it (`1 of 3─╯`). The focused panel's title takes the Cyan accent, as lazygit's active title does. Terminal title: the identifier BOLD (Cyan when focused): `repo / agent` with the repo and `/` DIM, or for a main checkout `repo` BOLD + ` branch` DIM.
- Status bar (one row, no border, the owner's "modern neovim-ish bottom nav"): the mode badge (`  NAV  ` / ` AGENT `, seven columns, Cyan reversed. Cyan always means "keys go here", and Yellow is reserved for attention and warnings), then up to 7 context hints (key default fg, description DIM), then the status message right-aligned (errors Red and sticky until the next key, info expiring after ~4s). Hints drop least important first (`x`, `D`, `d`, `m`, `n`, `Tab`, `Space`, `⏎`, while `? keys` always stays). The status is cut to fit rather than pushing hints out, so the row never jitters. In AGENT the hints are `⌃\ nav  ⌃\ ⌃\ previous`, naming the configured key (plus the attention count once status exists). Hints never offer actions that are invalid for the selection.
- Key names, everywhere (status bar, overlays, help): `⏎`, `Esc`, `Space`, `Tab`, and the configured escape key, as in `⌃\`. Help sections are named after the modes: `AGENT`, `NAV`.
- Render the pty cursor only in AGENT.

## Glyphs

| state | glyph | style |
|---|---|---|
| asking (a permission prompt or a question) | `◆` | Yellow BOLD, name BOLD, time Yellow |
| failed (rate limited, overloaded…) | `!` | Red BOLD, name BOLD |
| done, unseen | `✓` | Green BOLD, name BOLD |
| working, or its turn ended while background subagents run | braille spinner, `↳n` (DIM) after the name while subagents run and line 2 doesn't say so | default |
| working but silent for 5 minutes with no tool running | the frozen frame, time with a Yellow `?` | default |
| idle, seen, or a harness without hooks | `●` | default |
| exited (resurrectable) | `○` | DIM, name DIM |
| no session | `·` | DIM, name DIM |
| shared with another client | `⇄` in the right column | Yellow |

Never `✗`/`×` for exited (reads as "close"). Rows show the time in state right-aligned (`38s` `4m` `2h` `3d`) for working, asking, failed and done, and since the turn ended for delegating. Green means only `✓`.

## Attention

- Overview opens with a **Needs you** group listing every asking, failed or done-unseen agent, in the order `a` visits them: asking and failed first, then done, longest waiting first. A waiting agent shows here and nowhere else, not under Pinned or its repo. Selecting a row here keeps no repo open for it, so when repos don't all fit they close from the bottom. Collapsed repo counts leave waiting agents out.
- Focus lists agents from other repos that need you under the repo's cards, in a `▾ Needs you` group with their repo on the right. They're one line each and can't be selected, since selecting one would change the repo in focus: `a` goes there. The status bar carries the counts.
- `a` attaches the most urgent agent other than the one in the pane, into AGENT (`⌃\ a` from AGENT). With none: `Nothing needs you`.
- Done clears to idle on attach, or after 3s in the pane while the terminal has focus (terminals that never report focus count as focused). Failed clears on the next prompt, asking when its hook state changes.
- Status bar: at the far right, `a ◆ n  ! n  ✓ n` (`⌃\ a …` in AGENT), never cut, hidden at zero. It counts what `a` would visit, so the agent being typed into isn't counted. It shows and hides as one block at the edge, so the hints never shift. The terminal title (OSC 2) carries the same counts first: `◆2 ✓1 · agent`.
- One bell when an agent other than the one being typed into starts asking, fails, or finishes a turn of 30s or more. At most one per 10s.

## Sidebar (owner: lazygit-style panels)

```
╭ [1] Agents ────────────────╮
│ ▾ Needs you                │
│   ◆ heucristic0         4m │
│     Bash: pnpm test --fil… │
│   ! rate-limit-1        2m │
│     rate limited           │
│   ✓ auth-refresh-2     12m │
│     Added token refresh w… │
│ ▾ agent-tui0               │
│▌  ⠦ agent-tui0         10s │
│   ● docs-sweep-6           │
│   ○ leave-request-7        │
│ ▾ heucristic0              │
│   ⠦ obs-layout-4 ↳3     3m │
│   ⠦ payroll-fix-3      38s │
╰─────────────────── 5 of 10─╯
╭ [2] agent-tui0 ────────────╮
│ ⠦ thinking             10s │
│ overlap 5 files · docs-sw… │
│ 1 uncommitted · ↑2         │
│ › go                       │
╰────────────────────────────╯
╭ [3] Changes +86 −3 ────────╮
│ src/triage/flow.ts   +9 −3 │
│ triage/flow.test.ts +75    │
│ src/shared/types.ts  +2    │
│ assets/logo.png     bin    │
│ notes/plan.md       new    │
╰────────────────────────────╯
```

Three panels stacked: [1] the agents, [2] the selected agent, [3] the files it changed. `1` `2` `3` focus them (see Keymap). Selecting another agent, or a panel that isn't shown, puts the focus back on [1]. The focused panel takes the Cyan border and title while the sidebar has the keys, and the others are DIM. Below an inner width of 36 the short forms apply.

- [1] is sized to its rows, at most what leaves [2] 4 rows and [3] 3 inside their borders, and scrolls past that. [3]'s rows are kept even for an agent with no changes, so the selection never changes how much room the list has. Its corner shows `N of M`, counting the agents Focus lists from other repos.
- [2] is sized to what it says, and [3] takes the rest. Without changes [3] is hidden and [2] takes the rest. With no agent selected (a repo row), [1] takes the whole column.

### Cards

```
▌◆ triage-flow                   heucristic  4m
▌  Bash: pnpm test --filter triage
```

- Line 1: gutter, glyph, name, then on the right the repo (DIM, only in Needs you and Pinned, and not in the short form), the time in state and `⇄`. The right side never truncates, and the name yields down to 8 columns.
- Cards under a `▾` header (a repo, Needs you, Pinned) are indented 2.
- Line 2, indented under the name, cut with `…`: the question (default fg), the failure (`rate limited`, `overloaded`, `billing limit`, or the reason's first line), the tool and its target (a path keeps its end, `Edit  …/payroll/calc.ts`, and in the short form only its basename), `thinking`, `↳3 subagents · quiet 22s`, `handing back`, or the first line of the last message with markdown stripped. All DIM except the question. Settled agents (idle, exited, no session) have no line 2.
- Selection never changes a card's height, and the gutter covers both lines.

### Tier

Which cards get a line 2, worked out from the list at its tallest (every repo open, Needs you's header reserved, pinned agents counted twice) and the agents with a session, so state changes never change it: line 2 for every agent that isn't settled when that fits [1], else for the first agents `a` would visit while rows remain, else for none.

### [2] The agent

- Title: `[2]` and the agent BOLD. On the right of the top border, `in pane` when the pane shows it, else its repo when the name doesn't already say it.
- Items, in display and priority order:
  - The state line: glyph, what it's doing, the time in state on the right. Left out when the selected card's line 2 already says it whole.
  - The lead: all of the question or last message, up to 3 lines, when the state line shows and cut it and the agent isn't in the pane.
  - The subagent tree: `├ ⠹ description  kind  4m`, the last on `└`.
  - `overlap flow.ts · auth-refresh-2 +2`: a file another agent in the same repo changed too, the file name Yellow and the rest DIM. Agents are counted rather than listed, so the file name is never what gets cut. Up to 2 lines, else one for all of them: `overlap 5 files · docs-sweep-6 +2`.
  - Readiness, counts only: `2 uncommitted · ↑2`, `behind 2 · ↑3`, `ready ↑1` or `nothing ahead` (arrows DIM), or why git couldn't be read. A main checkout shows it only with uncommitted files, and an agent worktree shows `nothing ahead` only once it's done.
  - `› ` and the last prompt (DIM), up to 2 lines.
- Each item gets its rows in priority order, shrinking to the rows left before it's skipped, and the first that can't show at all ends the panel, so a lower item never takes rows from a higher one. Rows [3] doesn't need for its files let the lead and prompt grow past their caps. Focused, it shows everything and `j`/`k` scroll it.

### [3] Changes

- Title: `[3] Changes` then the totals (`+` Green, `−` Red).
- A row per file in numstat order, counts right-aligned in columns. Paths keep their last 3 components (`core/src/activity.rs`), fewer while they don't fit, so the prefix every file shares goes first.
- Unfocused, the last row says `+k more` when they don't all fit. Focused, a Cyan `▌` marks the file under the cursor, the rows scroll to keep it in view, and the corner says where it is.

Context % and cost from the harness are planned and not shown yet.

## Views

- **Focus** (default): one repo, no per-repo header rows. The main checkout is labelled by its branch, never `(root)`.
- **Overview**: an accordion. Repo rows are selectable items with `▸`/`▾` (DIM) and collapsed repo rows show right-aligned nonzero counts of settled agents in `● ○` order (waiting agents are counted under Needs you, and open repos show none), each glyph a space before its number and pairs two spaces apart (`● 1  ○ 2`) (idle `·` worktrees are not counted). Every repo is open when the whole list fits with one-line cards. Otherwise the repos furthest from the selection close first, and the repo holding the selection always stays open. Like the tier, this counts every place an agent could be listed, so agents changing state never open or close a repo. A repo whose agents are all under Needs you has no row. Cards are indented under the repo, and line 2 sits under the name. An agent under Needs you isn't listed again under its repo, and the selection follows an agent as it moves between them, keeping its screen row when the list scrolls. A repo whose only entry is its main checkout collapses to one row.
- **Pinned (owner)**: pinned agents sort first. In Overview they form a `▾ Pinned` group (DIM) above the repos. In Focus they sort to the top of their repo, and in the switcher they come first.
- **Hiding is removed (owner).**

## Keymap

AGENT: `nav` → NAV (cursor on current agent). `nav nav` → previous agent, back in AGENT. Everything else goes to the pty.

NAV:

| key | action |
|---|---|
| `Esc` / `i` | back to [1] from [2] or [3], else to AGENT on the current agent |
| `⏎` / `l` | AGENT on the selected agent (starts or resurrects as needed) |
| `1` / `2` / `3` | focus [1] Agents, [2] the agent, [3] Changes |
| `j`/`k`, `g`/`G` | move the selection in [1], scroll [2], move the file cursor in [3] |
| `[` / `]` | previous / next repo |
| `p` | repo picker |
| `Space` / `/` | switcher |
| `Tab` | Focus ↔ Overview |
| `m` | pin / unpin |
| `a` | next agent that needs you (see Attention) |
| `n` | start an agent in the selected idle worktree |
| `e` | open the checkout in `$EDITOR`, or in [3] the file under the cursor (`⏎` too) |
| `r` | reply to the agent without attaching: a one-line box sends it as a prompt. Only offered once the agent's turn is over with no question open and no subagents running, since while it works a permission prompt can open before its report arrives and typed text would pick a menu option. Checked again on `⏎` and just before typing |
| `d` | kill session, **with confirmation (owner)**: `y` or `⏎`, since it can be resurrected |
| `D` | remove session, worktree and branch (confirmation, or `Y` if commits would be lost) |
| `x` | actions overlay ("Run on <agent>"): the configured actions, one key each. `Esc` closes |
| `?` | help overlay |
| `q` | quit |

## Overlays

- **Switcher**: input focused on open. Index 0 is the previous agent, then pins, then MRU, then the rest. It never lists the TUI's own session. Rows use the Overview grammar: a main checkout reads `repo branch` (branch DIM) with nothing on the right. Other agents show their repo on the right (DIM). Age arrives with status. `Ctrl+n`/`Ctrl+p`/`Ctrl+\` move, `⏎` switches into AGENT, and `Esc` returns unchanged. Rows: glyph, `agent`, repo DIM, age DIM. Matched characters BOLD Cyan. Rounded DIM border, BOLD title, hints in the bottom border.
- **Confirm**: sized to its content, with two short lines and hints in the bottom border padded by one space each side, the confirming key first and always BOLD Red (it must differ from the border), alternatives after it (`── Y discard and remove  k kill only  n/Esc cancel ─╯`). Border Yellow, Red when data would be lost, and the destructive key BOLD Red. Unmerged work lists the commit count per branch.
- **Help**: the full keymap. Section headers are the same reversed-Cyan badges as the footer (`AGENT`, `NAV`). Keys and descriptions in default fg (DIM is unreadable on some themes at this density). Sized to fit 80x24, in two columns when the terminal is short.

## Empty and transient states

One centred line in the terminal area, verb first, never a second hint block (the footer already has the keys): `⏎ resurrect tanda_cli`, `⏎ start claude here` (key BOLD), `attaching to fix…`, `this TUI runs here` (DIM), `No agents yet` (DIM).

## Truncation

Names keep their start (`payroll-fix-for-ti…`), paths keep their end (`…/heucristic/triage-flow`), measured in display width. Right-aligned metadata never truncates, so names yield first.
