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

- Two rounded panels, like lazygit: the agent list and the selected agent's terminal. Each has its title in the top border and its metadata in the bottom-right corner (`N of M` for the list, the path or a Yellow `⇄ shared · ⏎ take over` for the terminal).
- The panel keys go to has a Cyan border (the list in NAV, the terminal in AGENT). The other is DIM, and both are DIM while an overlay has the keys. The owner rejected free-floating rules and a titles-in-rules frame. Borders belong to panels.
- List panel: min(30, 25%) inner columns plus its border, hidden below 60 columns so the agent keeps every column (Space still switches). Rows keep one column of inner padding on both edges. The terminal panel has one column of horizontal padding so the agent's own full-width rules don't fuse with our border.
- List title: the scope BOLD (`heucristic` or `All repos`). Position lives only in the `N of M` corner, in the border's colour and tucked against it (`1 of 3─╯`). The focused panel's title takes the Cyan accent, as lazygit's active title does. Terminal title: the identifier BOLD (Cyan when focused): `repo / agent` with the repo and `/` DIM, or for a main checkout `repo` BOLD + ` branch` DIM.
- Status bar (one row, no border, the owner's "modern neovim-ish bottom nav"): the mode badge (`  NAV  ` / ` AGENT `, seven columns, Cyan reversed. Cyan always means "keys go here", and Yellow is reserved for attention and warnings), then up to 7 context hints (key default fg, description DIM), then the status message right-aligned (errors Red and sticky until the next key, info expiring after ~4s). Hints drop least important first (`x`, `D`, `d`, `m`, `n`, `Tab`, `Space`, `⏎`, while `? keys` always stays). The status is cut to fit rather than pushing hints out, so the row never jitters. In AGENT the hints are `⌃\ nav  ⌃\ ⌃\ previous`, naming the configured key (plus the attention count once status exists). Hints never offer actions that are invalid for the selection.
- Key names, everywhere (status bar, overlays, help): `⏎`, `Esc`, `Space`, `Tab`, and the configured escape key, as in `⌃\`. Help sections are named after the modes: `AGENT`, `NAV`.
- Render the pty cursor only in AGENT.

## Glyphs

| state | glyph | style |
|---|---|---|
| asking (a permission prompt or a question) | `◆` | Yellow BOLD, name BOLD, time Yellow |
| failed (rate limited, overloaded…) | `!` | Red BOLD, name BOLD |
| done, unseen | `✓` | Green BOLD, name BOLD |
| working | braille spinner | default |
| working but silent for 5 minutes with no tool running | the frozen frame, time with `?` | default |
| idle, seen, or a harness without hooks | `●` | default |
| exited (resurrectable) | `○` | DIM, name DIM |
| no session | `·` | DIM, name DIM |
| pinned | slot digit `1`-`9` in the right column, `*` beyond nine | DIM |
| shared with another client | `⇄` in the right column | Yellow |

Never `✗`/`×` for exited (reads as "close"). Rows show the time in state right-aligned (`38s` `4m` `2h` `3d`) for working, asking, failed and done. Green means only `✓`.

## Attention

- Overview opens with a **Needs you** group listing every asking, failed or done-unseen agent, in the order `a` visits them: asking and failed first, then done, longest waiting first. Selecting a row there doesn't expand its repo (like Pinned). A waiting pinned agent shows here, not under Pinned. Collapsed repo counts leave waiting agents out.
- In Focus, the list title shows a Yellow `◆ n` when other repos have agents waiting.
- `a` attaches the most urgent agent other than the one in the pane, into AGENT (`⌃\ a` from AGENT). With none: `Nothing needs you`.
- Done clears to idle on attach, or after 3s in the pane while the terminal has focus (terminals that never report focus count as focused). Failed clears on the next prompt, asking when its hook state changes.
- Status bar: at the far right, `a ◆ n  ! n  ✓ n` (`⌃\ a …` in AGENT), never cut, hidden at zero. It counts what `a` would visit, so the agent being typed into isn't counted. It shows and hides as one block at the edge, so the hints never shift. The terminal title (OSC 2) carries the same counts first: `◆2 ✓1 · agent`.
- One bell when an agent other than the one being typed into starts asking, fails, or finishes a turn of 30s or more. At most one per 10s.

## Peek

The sidebar's second panel, below the list: what the pane can't show about the agent in it (or about the selection when the pane is empty). Title: the agent's label, or, when the pane shows the agent, its state with the current tool's time (`⠹ Edit 38s`), since the pane's own title already names it. Lines in display order, each cut by priority when short of rows: state and time (only without a pane), the question, summary or tool target (only without a pane), `› ` and the last prompt (DIM), `+84 −12 · 3 files` since the fork point, `↑2 · 1 uncommitted`. Height: a third of the body, 3 to 8 lines, never more than it has, leaving the list at least 5 rows. Hidden below 15 rows. Context % and cost from the harness are planned and not shown yet.

## Views

- **Focus** (default): one repo, no per-repo header rows. The main checkout is labelled by its branch, never `(root)`.
- **Overview**: an accordion. Repo rows are selectable items with `▸`/`▾` (DIM) and collapsed repo rows show right-aligned nonzero counts of settled agents in `● ○` order (waiting agents are counted under Needs you, and open repos show none), each glyph a space before its number and pairs two spaces apart (`● 1  ○ 2`) (idle `·` worktrees are not counted). Only the repo holding the selection expands, with children indented 2 so their glyph sits under the repo name. The title's right side is `N repos` (DIM), matching Focus's `‹i/n›`. A repo whose only entry is its main checkout collapses to one row.
- **Pinned (owner)**: pinned agents sort first. In Overview they form a `▾ Pinned` group (DIM) above the repos, children indented 2. In Focus they sort to the top of their repo, and in the switcher they come first. Pins get slots `1`-`9`.
- **Hiding is removed (owner).**

## Keymap

AGENT: `nav` → NAV (cursor on current agent). `nav nav` → previous agent, back in AGENT. Everything else goes to the pty.

NAV:

| key | action |
|---|---|
| `Esc` / `i` | back to AGENT on the current agent |
| `⏎` / `l` | AGENT on the selected agent (starts or resurrects as needed) |
| `j`/`k`, `g`/`G` | move |
| `[` / `]` | previous / next repo |
| `p` | repo picker |
| `Space` / `/` | switcher |
| `Tab` | Focus ↔ Overview |
| `1`-`9` | pinned slots |
| `m` | pin / unpin |
| `a` | next agent that needs you (see Attention) |
| `n` | start an agent in the selected idle worktree |
| `o` | open the checkout in `$EDITOR` |
| `d` | kill session, **with confirmation (owner)**: `y` or `⏎`, since it can be resurrected |
| `D` | remove session, worktree and branch (confirmation, or `Y` if commits would be lost) |
| `x` | actions overlay ("Run on <agent>"): the configured actions, one key each. `Esc` closes |
| `?` | help overlay |
| `q` | quit |

## Overlays

- **Switcher**: input focused on open. Index 0 is the previous agent, then pins, then MRU, then the rest. It never lists the TUI's own session. Rows use the Overview grammar: a main checkout reads `repo branch` (branch DIM) with nothing on the right. Other agents show their repo on the right (DIM), then the pin slot. Age arrives with status. `Ctrl+n`/`Ctrl+p`/`Ctrl+\` move, `⏎` switches into AGENT, and `Esc` returns unchanged. Rows: glyph, `agent`, repo DIM, age DIM. Matched characters BOLD Cyan. Rounded DIM border, BOLD title, hints in the bottom border.
- **Confirm**: sized to its content, with two short lines and hints in the bottom border padded by one space each side, the confirming key first and always BOLD Red (it must differ from the border), alternatives after it (`── Y discard and remove  k kill only  n/Esc cancel ─╯`). Border Yellow, Red when data would be lost, and the destructive key BOLD Red. Unmerged work lists the commit count per branch.
- **Help**: the full keymap. Section headers are the same reversed-Cyan badges as the footer (`AGENT`, `NAV`). Keys and descriptions in default fg (DIM is unreadable on some themes at this density). Sized to fit 80x24, in two columns when the terminal is short.

## Empty and transient states

One centred line in the terminal area, verb first, never a second hint block (the footer already has the keys): `⏎ resurrect tanda_cli`, `⏎ start claude here` (key BOLD), `attaching to fix…`, `this TUI runs here` (DIM), `No agents yet` (DIM).

## Truncation

Names keep their start (`payroll-fix-for-ti…`), paths keep their end (`…/heucristic/triage-flow`), measured in display width. Right-aligned metadata never truncates, so names yield first.
