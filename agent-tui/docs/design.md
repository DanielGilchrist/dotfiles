# agent TUI design

Distilled from two research briefs (interaction, visual) on lazygit, helix, k9s, yazi, zellij, tmux, gitui, television and atuin. Owner decisions override the briefs where they conflict, and are marked **(owner)**. Every visual or interaction change is reviewed against this doc by adversarial design reviewers.

## Principles

1. **One hint line, chosen by context. The full map lives behind `?`.** lazygit's `showBottomLine`, k9s's per-view menu, gitui's "context based help". Never a static hint block in a sidebar.
2. **The mode is stated in text, not only colour.** helix `NOR`/`INS`, zellij's status bar. We have two modes: `AGENT` (keys go to the pty) and `NAV` (keys go to the app).
3. **The escape key is a prefix, like tmux's `C-b`.** `nav` (default `Ctrl+\`, configurable under `[keys]` so a harness that claims it can be worked around) enters NAV, and any NAV action that targets an agent drops back into AGENT on that agent. Twice in a row means "previous agent" (tmux `last-window`). The owner tried a one-shot mode plus a second sustained key, like AeroSpace's, and dropped it: no second Ctrl chord was easier to reach than the first.
4. **Escaping is free to undo.** `Esc` in NAV returns to the agent you left.
5. **Attention is a per-agent flag plus a jump key** (tmux `#`/`!` flags, `next-window -a`). Source: hooks the harness is launched with (Claude: `--settings` carrying `UserPromptSubmit`, `PreToolUse`/`PostToolUse`, `PermissionRequest`, `Stop`, `StopFailure`, `SessionEnd`), reported through `agent event` into the state dir. `idle_prompt` is ignored because it fires while background work still runs.
6. **Confirmation strength matches what is lost.** One vocabulary: `y` confirms, `Y` confirms data loss, `Esc`/`n` cancels.
7. **Respect the user's palette.** Colours are roles, and each defaults to a named ANSI colour: accent Cyan, warning Yellow, error Red, success Green. This doc names them by their defaults. `Color::Reset` backgrounds. Secondary text (DIM in this doc) is the terminal's foreground blended into its background, read with `OSC 10`/`OSC 11` at startup before the input thread reads the tty. The blend starts halfway and takes more foreground until it reaches 3:1 contrast with the background, since halfway is barely readable on light themes and Solarized. It falls back to the `DIM` attribute when `COLORTERM` doesn't promise 24-bit colour or the terminal doesn't answer within a second (locally it answers in milliseconds, and the wait ends there). It is read once, so a theme change needs a restart. Never `DarkGray`, because brblack is the background in Solarized. `[theme]` (owner) overrides any role with an ANSI name, `#rrggbb` or `default`, and `dim` also takes `faint`. `NO_COLOR` drops every colour.
8. **Selection never depends on span colours.** No `REVERSED`, no background fill: a 1-column `▌` gutter (Cyan when the list has focus, DIM otherwise) plus a BOLD name.
9. **Whitespace and single rules over boxes.** Boxes only for overlays.
10. **Colour encodes state and is never alone**: every state has a distinct glyph, so it reads with `NO_COLOR` or a monochrome theme. No Nerd Font dependency.

## Layout (owner: lazygit-style panels, modern status bar)

```
╭ [1] heucristic ───────────╮╭ [0] heucristic / triage-flow ───────────────────╮
│▌◆ triage-flow           1 ││ <embedded terminal>                             │
│ ● payroll-fix             ││                                                 │
│ ✓ main                    ││                                                 │
╰─────────────────── 1 of 3─╯╰─────────────────────── ~/worktrees/heucristic/… ╯
  NAV    ⏎ attach  Space switch  d kill  D remove  m pin  ] agents  ? keys      killed foo
```

- Rounded panels, like lazygit: the sidebar's three (see Sidebar) and the main view [0], which shows the selected agent's terminal or what a panel's cursor is on (see Main view). Each has its title in the top border and its metadata in the bottom-right corner (`N of M` for the list, the path or a Yellow `⇄ shared · ⏎ take over` for the terminal).
- The panel keys go to has a Cyan border (the focused sidebar panel in NAV, a focused document in [0], the terminal in AGENT). The others are DIM, and all are DIM while an overlay has the keys. The owner rejected free-floating rules and a titles-in-rules frame. Borders belong to panels.
- Sidebar: 24% of the width, clamped to 28-48 inner columns, plus its border. It's hidden below 60 columns so the agent keeps every column (Space still switches). Rows keep one column of inner padding on both edges. The terminal panel has one column of horizontal padding so the agent's own full-width rules don't fuse with our border.
- List title: `[1]` then the three tabs, lazygit-style: `Agents`, the selected row's repo (`heucristic`) and `Pinned`, the active one BOLD and the others plain with a DIM ` · ` between. The form follows the width alone, never the cursor: below 32 columns inside the border (the title's two padding columns, 4 for `[1] `, 8 for the shortest repo name, 18 for the other tabs) only the active tab shows, and above it the repo name yields to fit as names do. With the sidebar clamped to 28-48 inner columns that means the strip shows from about 140 columns. With no row selected only the active tab shows, and the repo tab with no repo is bare `[1]`. Position lives only in the `N of M` corner, in the border's colour and tucked against it (`1 of 3─╯`). The focused panel's title takes the Cyan accent, as lazygit's active title does. Terminal title: `[0]` then the identifier BOLD (Cyan when focused): `repo / agent` with the repo and `/` DIM, or for a main checkout `repo` BOLD + ` branch` DIM.
- Status bar (one row, no border, the owner's "modern neovim-ish bottom nav"): the mode badge (`  NAV  ` / ` AGENT `, seven columns, Cyan reversed. Cyan always means "keys go here", and Yellow is reserved for attention and warnings), then the context's hints (key default fg, description DIM), then the status message right-aligned (errors Red and sticky until the next key, info expiring after ~4s). The hints are not capped. Each binding in the table in `crates/tui/src/keymap.rs` carries a rank, which may depend on the focused panel, the hovered item and the mode, and the bar shows as many as fit the width, dropping the lowest ranked first (at most widths that is `x`, `e`, `D`, `.`, `d`, `m`, then `Space` outside [1], `n`, `v`, `s`, `]`; `h l` are Hidden in the table, so they're never in the footer), while `? keys` stays. The same table feeds the help, so the two can't disagree, and a hint is never shown twice. The status is cut to fit rather than pushing hints out, so the row never jitters. In AGENT the hints are `⌃\ nav  ⌃\ ⌃\ previous`, naming the configured key (plus the attention count once status exists). The peek's badge reads ` PEEK `. Hints never offer actions that are invalid for the selection.
- Key names, everywhere (status bar, overlays, help): `⏎`, `Esc`, `Space`, `Tab`, and the configured escape key, as in `⌃\`. The vocabulary is one verb per action: **attach** (the keys go to the agent's terminal), **send** (compose a message, delivered now or queued), **open** (focus [0] on the item), **peek**, **nav** (the exit key and its mode) and **back** (one level out).
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
| a file another agent changed too | `∩` before its counts in [3] | Yellow |

Never `✗`/`×` for exited (reads as "close"). Rows show the time in state right-aligned (`38s` `4m` `2h` `3d`) for working, asking, failed and done, and since the turn ended for delegating. Green means only `✓` and added lines.

## Attention

- Overview opens with a **Needs you** group listing every asking, failed or done-unseen agent, in the order `a` visits them: asking and failed first, then done, longest waiting first. A waiting agent shows here and nowhere else, not under its repo. Pinned ends with the same group for the waiting agents that aren't pinned. Selecting a row here keeps no repo open for it, so when repos don't all fit they close from the bottom. Collapsed repo counts leave waiting agents out.
- Focus lists agents from other repos that need you under the repo's cards, in a `▾ Needs you` group with their repo on the right. They're one line each and can't be selected, since selecting one would change the repo in focus: `a` goes there. The status bar carries the counts.
- `a` attaches the most urgent agent other than the one whose terminal [0] shows, into AGENT (`⌃\ a` from AGENT). With none: `Nothing needs you`.
- Done clears to idle on attach, or after 3s with its terminal visible in [0] while the terminal has focus (terminals that never report focus count as focused). Reading its diff or its turn in [0] doesn't clear it. Failed clears on the next prompt, asking when its hook state changes.
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
│▌  ✓ auth-refresh-2     12m │
│▌    Added token refresh w… │
│ ▾ agent-tui0               │
│   ⠦ agent-tui0         10s │
│   ● docs-sweep-6           │
│   ○ leave-request-7        │
│ ▾ heucristic0              │
│   ⠦ obs-layout-4 ↳3     3m │
│   ⠦ payroll-fix-3      38s │
╰─────────────────── 3 of 10─╯
╭ [2] auth-refresh-2 ────────╮
│ ✓ done                 12m │
│ › go                       │
│ Added token refresh with   │
│ retry and a 401            │
│ interceptor. 14 tests …    │
│ +3 more                    │
╰────────────────────────────╯
╭ [3] Uncommitted · Branch +84 −3  ∩ 2 ───╮
│ 2 uncommitted · ↑2         │
│ triage/flow.ts    ∩  +9 −3 │
│ flow.test.ts      ∩ +75    │
╰────────────────────────────╯
```

Three panels stacked, each a plain list that picks what [0] shows: [1] the fleet, [2] the selected agent's turn and subagents, [3] its work in git. `1` `2` `3` focus them (see Keymap). The prose lives in [0], where there is room for it. The layout is a function of content and selection alone, and focus never moves a panel's edges. Selecting another agent, or a panel that isn't shown, puts the focus back on [1]. The focused panel takes the Cyan border and title while the sidebar has the keys, and the others are DIM. Below an inner width of 36 the short forms apply.

- [1] is sized to its rows, at most what leaves [2] and [3] their minimums (4 rows for [2], and 3 file rows plus the readiness row for [3], inside their borders), and scrolls past that. That reserve binds only [1]'s room, so the selection never changes it. Its corner shows `N of M`, counting the agents Focus lists from other repos.
- [2] is sized to its rows, up to 6, and [3] takes the rest when it has rows to say something and room for the readiness row and three files. Without that room [2] takes the rest. Without [3] content [2] takes the rest too. With no agent selected (a repo row), [1] takes the whole column. A list that has the keys scrolls to keep its cursor in view. Unfocused, [3] ends with `+k more` when its rows don't all fit.

### Cards

```
▌◆ triage-flow                   heucristic  4m
▌  Bash: pnpm test --filter triage
```

- Line 1: gutter, glyph, name, then on the right the repo (DIM, only in Needs you and the Pinned tab, and not in the short form), the time in state and `⇄`. The right side never truncates, and the name yields down to 8 columns.
- Cards under a `▾` header (a repo, Needs you) are indented 2. The Pinned tab's pins have no header and aren't.
- Line 2, indented under the name, cut with `…`: the question (default fg), the failure (`rate limited`, `overloaded`, `billing limit`, or the reason's first line), the tool and its target (a path keeps its end, `Edit  …/payroll/calc.ts`, and in the short form only its basename), `thinking`, `↳3 subagents · quiet 22s`, `handing back`, or the first line of the last message with markdown stripped. All DIM except the question. Settled agents (idle, exited, no session) have no line 2.
- Selection never changes a card's height, and the gutter covers both lines.

### Tier

Which cards get a line 2, worked out from the list at its tallest (every repo open, Needs you's header reserved, and in the Pinned tab every agent that isn't pinned counted as waiting) and the agents with a session, so state changes never change it: line 2 for every agent that isn't settled when that fits [1], else for the first agents `a` would visit while rows remain, else for none.

### [2] The turn

A list: the turn row `› <first line of the prompt>`, then one row per subagent as `├ ⠹ description  kind  4m` with the last on `└`. A subagent with no description shows its kind DIM in the description column. A subagent that has finished stays until the next prompt, drawn `✓` with how long it ran, so its row and transcript are still there to read and the cursor doesn't jump when it stops. The row under [2]'s cursor also stays after a prompt clears finished ones, until the cursor leaves it. It doesn't count in `↳n` or hold the agent from being at rest. The row under the cursor has a Cyan `▌` in the first column while [2] has the keys, and a BOLD description. The title is `[2] Turn`. An agent with no hook reports has no [2], and `2` says `no turn recorded · d then ⏎ restarts it through agent`. Its cursor drives [0]: the turn row shows the turn document and a subagent row shows that subagent's transcript. The harness reads the transcript of the subagent under the cursor off the UI thread (see Main view).

### [3] Changes

Everything git about the agent's worktree, as rows. "Can I merge this" and "is anyone else in this file" are questions about the work, so they're answered here and in [0]'s diff title.

- Title: `[3]` then two tabs as [1]'s title has them, `Uncommitted` and a second tab named for where the checkout is, `Branch` or `Unpushed`, the active one BOLD and the others plain with a DIM ` · ` between. Both show while they fit the width and only the active one below that, so the form never follows the cursor. Then the active tab's totals (`+` Green, `−` Red), then `∩ n` (Yellow) when n files are shared with other agents in the repo. `[` and `]` cycle the tabs while [3] has the keys (the footer's `]` hint and the help name the actual next tab), and the choice is kept for the session. The default is `Uncommitted`. Tabs are a per-panel concept in the keymap: `[` `]` is bound once for [1] and once for [3], each with its own label.
- **Uncommitted** lists only files with uncommitted state (staged, unstaged, untracked or deleted), counted against the last commit (`git diff --numstat HEAD` joined with `git status`, both with `--no-optional-locks`), untracked files as `new`. The numbers mean what isn't committed. **Branch**: what this branch changes compared with where it left `origin/main`, like a PR plus your uncommitted work. **Unpushed**: on the trunk (the local branch the default branch is a remote copy of, `main` on `origin/main`), commits not yet on origin plus uncommitted work; a push empties it. Both are the diff from `merge-base(HEAD, origin/HEAD)` to the working tree: committed and uncommitted work together, where uncommitted files keep their markers and files that are only committed have none. Hovering a file reads its diff against the same base as the tab it's in (`DiffBase::Head` or `Fork`).
- First row, on the second tab only: the branch summary, short enough for the narrow sidebar, `↑27 · 4 uncommitted`, each part left out when it's zero, `↓n` when the default branch has moved on, and `nothing to push` (trunk) or `no changes` when all are zero. Uncommitted has no such row: it lists only its files, and hovering one of them never shows the branch. What is compared with what is left to [0]. A detached HEAD is named by its short sha. With no default branch to compare with the tab is `Branch` and only uncommitted files are listed. Which of these applies is `Compared`, read with the worktree's facts. A main checkout shows the row only with uncommitted files, and an agent worktree shows the nothing form only once it's done. It is a row the cursor can be on, and then [0] shows the branch: titled `Unpushed · main vs origin/main` or `Branch · fix-thing vs origin/main` (`<sha> vs …` detached, `· no base` without one), then `Commits (n)` and `Uncommitted (n)`, and `behind origin/main by n` when behind, so committed work stays reachable. If git couldn't be read, `git: <why>` (its first line) is the only row. The row is about the worktree rather than a file, so it is DIM behind the files' gutter column. The panel's bottom-right corner counts the active tab's files (`4 files`).
- Two views, switched with `` ` `` (lazygit's key) and remembered for the session. The default is the **tree**: folders before files (each group alphabetical), a chain of single-child folders on one row (`crates/tui/src`), `▼` open and `▶` closed before a folder's name, two columns of indent per level, and a file under its folder with its name only. A folder row carries its files' summed counts in DIM (`+16 −3`, or `new` / `bin` when nothing has line counts). `⏎` on a folder opens or closes it, `-` closes every folder and `=` opens them all. What's closed is kept per agent by every real folder path, so a chain row (`a/b/c`) is closed when any folder it stands for is: `⏎` closes it at its deepest folder and opening it opens all of them, `-` closes every folder including the intermediate ones, and a chain that splits or merges as files come and go keeps its folders' state. A folder that appears starts open, and closed folders that no longer exist are forgotten when the agent's facts are read again. The cursor is held by identity (the file's or folder's path, or the readiness row) and re-resolved after each read, falling back to the nearest row, so a refresh above it never moves it to another file. The built rows are cached per agent until its facts, the view or its closed folders change. The cursor stays on the folder it toggled, and on the folder holding it after `-`. `` ` `` keeps it on the same file, and switching tabs keeps it on the same path when the other tab lists it (or a folder holding it) and otherwise puts it on the first row (the branch row on the second tab, the first file on Uncommitted). The **flat** list is the files in numstat order as below. The branch row, when the tab has it, stays first. In [0] a folder under the cursor lists the files under it with their counts (read from the git facts already loaded, so nothing waits on git, rather than one combined diff per folder), titled `<folder>/ n files +a −r`. `e` on a folder opens `$EDITOR` in that folder.
- Flat: a row per file in numstat order, counts right-aligned in columns. A file another agent changed too carries a Yellow `∩` before its counts, and the column is kept so the counts stay aligned. Paths keep their last 3 components (`core/src/activity.rs`), fewer while they don't fit.
- Every row opens with a three-column marker, in both views, so committed and uncommitted files read apart. A file shows lazygit's `XY` code: the index letter (`M` modified, `A` added, `D` deleted) in Green, then the working-tree letter in Red, `??` in Red for an untracked file, and nothing for one that is committed and clean now (only the second tab lists those). A folder shows a Green `●` in the first column when a file under it has staged changes and a Red `●` in the second when one has unstaged or untracked changes, and nothing when everything under it is committed. On the second tab a file that is dirty but identical to the fork point (staged, then reverted) isn't listed, since that list is the diff from the fork. The marker keeps its width and the path is what truncates.
- A Cyan `▌` marks the row under the cursor while [3] has the keys. Who else changed the file is in [0]'s diff title, where every name fits.

## Main view [0]

The right pane is a function of the focused panel and the item under its cursor, as in lazygit. The terminal is what [0] shows for [1].

| focus | row | [0] shows | title | corner |
|---|---|---|---|---|
| [1] | an agent | its live terminal | `[0] repo / agent` | path, or `⇄ shared` |
| [2] | the turn | the turn: the whole prompt, the whole message or question, the subagent rows | `[0] Turn` | position when it scrolls |
| [2] | a subagent | its transcript | `[0] description · kind` | `following`, `paused 12-52 of 88`, or the position once finished |
| [3] | branch row (second tab) | the branch: commits since the fork, uncommitted files | `[0] Branch · fix-thing vs origin/main` | position when it scrolls |
| [3] | a file | its diff from the fork point to the working tree, with an untracked file shown whole | `[0] path +a −r  ∩ agents` | position when it scrolls |
| [3] | a folder | a sectioned diff: a compact list of its files with markers and counts, then each file's diff under a bold path and its counts after a DIM rule, in the tree's order | `[0] path/ n files +a −r` | position when it scrolls |
| [1] | a repo row | the repo's checkouts, one line each as the cards say them, with their changed files | `[0] repo branch` | none |

- **Status.** Whenever [0] isn't the terminal, the top-right of its border carries the selected card's state: glyph, name, what it's doing and for how long. The glyph, name and time are reserved first, so the title yields to them (a path keeps its end, who else changed the file shrinks to `∩ auth-refresh-2 +1`), and what it's doing shows only with at least 14 columns. A delegating gist already ends in the quiet time, so no second time follows it. An agent that asks opens a Yellow banner row at the top of [0] with the question and `i answer`. It never switches [0] to the terminal by itself, since taking a diff away from under the reader is worse than one keypress.
- **In the pane** means its terminal is visible in [0], not that it's selected. Done doesn't clear while its diff or turn is on screen, and `a`, the bell and the status counts treat it as not in the pane.
- **The pty keeps its size.** Its rect is the terminal area for the screen mode and window, whatever [0] shows. Only `+` and `_` resize it. The reader keeps feeding the parser while the terminal is hidden.
- **Folders.** The file list shows at once from the git facts. The diffs are one document per folder and facts generation, read off the UI thread through the same loader and cache, against what the active tab does (HEAD on Uncommitted, the fork point on Branch and Unpushed). Diffs of those files already in the cache are reused, each file keeps the per-file line and byte caps, and the folder stops after 20 files or 5000 lines in all, ending `+k more files, open one to see its diff`.
- **Diff cursor.** With [0] focused on a file's or a folder's diff, `j`/`k` move a Cyan `▌` over its lines (hunk headers are skipped), `g`/`G` go to the first and last, `[`/`]` jump to the previous and next hunk, and `e` opens the file in `$EDITOR` at the line (`+N` before the path, as tig does), a removed line at the line that now follows it. Every diff row has a one-column gutter for the cursor. The line positions come from each `@@ -a,b +c,d @@` header.
- **Loading.** Diffs and branch logs are read off the UI thread once the cursor has rested 60 ms on a row, keyed by what makes them change (a file's numstat line or a branch's counts, and how many times the agent's git facts were read, so an edit that kept the counts still shows). The last 32 are kept, within 8 MB, and a read that failed is shown for 5 s and then asked again. A file read again keeps its old text on screen meanwhile. Another file never shows under this title, only blank, with a DIM `loading` in the corner after 150 ms, so holding `j` never flickers. Git's output is read up to 2 MB and each line is kept to 2000 characters, so a huge diff is never held whole. Hunk headers are cut with `…` and code wraps. A diff over 5000 lines shows the first 5000 and ends with `12,340 more lines · e open`. A binary file reads `binary · 48 KB`.
- **Transcripts.** The harness reads the tail of the subagent's transcript when the cursor lands, soon after its agent reports, and every 10 s while it runs, and stops once the transcript says it finished. [0] shows a pinned status row and a DIM rule, then the scroll region. The status row says whether it progresses: `⠹ working 4m12s · 37 calls · 1 error · sonnet`, `⠋ working 14m · quiet 6m ? · …` after 5 minutes with no record and no running tool (the spinner stands still), `! failing · cargo build failed 4 times · …` when one call failed three times since it last succeeded or three of the last five calls failed, `! stopped · API error …` when the transcript ends in an API error record (matched on `isApiErrorMessage`, not yet seen in the wild), `✓ finished in 6m02s · 52 calls`, or `● ended after 6m02s · no report`. Counts read `400+ calls` when the tail didn't reach the transcript's start. The prompt is one row and `+n lines`. Each said block starts a phase after a blank row, and its calls sit indented under it. A call is one row, `● Tool  target ··· summary  12s`: the target is the command without a leading `cd … &&`, a path relative to the checkout, a `"pattern" in path`, and so on, and the summary is read from the structured result (`730 lines`, `+9 −14`, `11 matches`, `new · 120 lines`, `exit 101`). Both sides are cut so nothing overflows the row. A failed call (`is_error` or `Exit code N`) is `!` Red with up to three `⎿` rows of stderr under it and `+n lines`. A succeeded Bash call whose output holds a failure line (`test result: FAILED`, ` FAILED`, `panicked at`, `error:`, `FAIL`, a nonzero `n failed`) is `●` Yellow with those lines under it. Three or more successful Read, Grep, Glob or LS calls in a row fold into `● Read  4 files  a · b · c +1 ··· 3.4k lines`, and the same call twice or more in a row into one row with `×n` whose outcome is the latest. A running call shows a spinner and `running 12s`, with a Yellow `?` after 5 minutes. Call durations show from 5 s. Below 100 columns the summary column shrinks to 10 and a finished call's duration replaces its summary. A finished subagent shows its `Report` (its handback message, else its last message) between the prompt and a DIM `Activity · n calls` rule, and opens at the top, with a subagent finishing while followed jumping there. A running one opens at the tail and follows, scrolling up pauses it, and `G` resumes. With [0] focused, `j`/`k` move a Cyan `▌` cursor over entries (the prompt, a said block, a call, a fold), `⏎` expands the entry (the whole prompt or text, a call's input and result up to 40 lines, a fold into its calls), `=` and `-` expand and collapse all, `g`/`G` go to the first and last entry, and `⌃D`/`⌃U`/`PgDn`/`PgUp` page with the cursor. Expansion is kept per subagent until its row leaves [2]. `e` writes the transcript as Markdown (prompt, report, each call's input and result, and the raw `.jsonl` path) to `agent-transcripts` in the temp dir and opens it in `$EDITOR` at the cursor's entry. A subagent with no transcript file reads `no transcript for this subagent`. The prompt and the report are kept whole, up to 8000 characters, and a call's input and result up to 1000 and 2000.
- **Peek.** `v` opens a floating window centred over everything, showing the selected live agent's terminal at exactly the pty's current size (a pty bigger than the screen is cropped from the top so Claude's prompt at the bottom stays visible, with `cropped, showing the bottom` on the bottom border, and it is never resized for the float). What's behind it is dimmed. The double Cyan border carries `<glyph> <agent>  <state> <time>` on the left and `⏎ attach · s send · Esc close` on the right, and the footer says the same, under a `PEEK` badge. No key reaches the pty while it is open: `s` opens the send box for that agent (the peek stays behind it), `i` or `⏎` enters AGENT with the normal border, and `Esc` or `v` closes it. `t` says `send is s now`. The terminal counts as visible while peeking (the done-clear and `a` see it), and the mouse is ignored.
- **Mouse.** A click on a row of [2] or [3], the readiness row included, focuses that panel with its cursor on the row, so [0] shows it. A click inside a document focuses it. The wheel scrolls the document under it, and over [2] or [3] it moves that cursor. Hit rows are recorded as drawn, so they stay right after a panel scrolls.
- **Focus.** `⏎` or `0` from [2] or [3] focuses the document, which takes the Cyan border while the badge stays `NAV`. `j`/`k` scroll a line there, `g` goes to the top and `G` to the bottom, `Esc` goes back to the panel.
- **Screen modes.** `+` hides the sidebar so [0] takes the width, which suits long diffs, and `_` brings it back. Keys keep going to the focused panel, so `j` flips to the next file in full screen. The title gains the position (`2 of 5 · path`) whenever the sidebar is out of sight, including below 60 columns, where `1` `2` `3` still pick what [0] shows.

## Repos

The sidebar shows a repo while a session or an agent worktree belongs to it, and while it's **listed**: kept in `~/.local/state/agent/repos` by `agent add`, the picker, `agent repo`, or creating an agent in it. A listed repo with nothing running is one row, its main checkout, so `⏎` starts the repo session there. A repo whose directory name can't be a session name can't be listed. `agent forget` drops the listing. A listed path that isn't a repo any more is a discovery warning, not an error. The **catalogue** is what the picker and the `n` form offer beyond that: `[repos] dirs` in the config, each a repo or a directory of repos, read without running git (a `.git` directory is enough, and linked worktrees are left out). Paths are canonical so a found repo matches its listing. It's rescanned with every refresh, never puts a repo in the sidebar by itself, and what couldn't be read there is reported like a discovery warning.

## Views

Three tabs in [1], in the order Agents, repo, Pinned, with Agents the one the TUI opens on (the saved selection is restored within it, and the tab itself isn't saved), cycled with `[` and `]`, named in its title where the width allows (see Layout) and otherwise only by the footer's `]` hint, which names the next tab (`agents`, `repo`, `pinned`). Moving between tabs keeps the selected agent when the new tab lists it. Otherwise the tab selects its first row (in Pinned the first pin, else the first Needs you row) and remembers what was selected, so the next tab that lists it, or a full cycle, comes back to it unless you moved in between.

- **Repo (Focus, default)**: one repo, named by the selected row's repo in the title, no per-repo header rows. The main checkout is labelled by its branch, never `(root)`.
- **Agents (Overview)**: an accordion. Repo rows are selectable items with `▸`/`▾` (DIM) and collapsed repo rows show right-aligned nonzero counts of settled agents in `● ○` order (waiting agents are counted under Needs you, and open repos show none), each glyph a space before its number and pairs two spaces apart (`● 1  ○ 2`) (idle `·` worktrees are not counted). Every repo is open when the whole list fits with one-line cards. Otherwise the repos furthest from the selection close first, and the repo holding the selection always stays open. Like the tier, this counts every place an agent could be listed, so agents changing state never open or close a repo. A repo whose agents are all under Needs you has no row. Cards are indented under the repo, and line 2 sits under the name. An agent under Needs you isn't listed again under its repo, and the selection follows an agent as it moves between them, keeping its screen row when the list scrolls. A repo whose only entry is its main checkout collapses to one row.
- **Pinned (owner)**: its own tab, with the Focus grammar. The pins come first, flat and in pin order, with their repo on the right as Needs you rows have. A waiting pin stays in its place, its glyph and time saying so. Then `▾ Needs you` for the waiting agents that aren't pinned, selectable as in Overview. With nothing to list the tab is empty and the terminal area says `Nothing pinned`. Selecting an agent the tab doesn't list (the switcher, `⌃\` back from it) goes to Agents with it. `p`, `{` and `}` go to the repo tab. Pinned agents also sort to the top of their repo in Focus and come first in the switcher.
- **Active only (owner)**: the repo and agents tabs list only checkouts with a live session, whatever its state, plus the selected one so the cursor never vanishes (it goes once something else is selected). A repo with nothing listed has no row, and `{`/`}` skip it. `.` shows everything, like hidden files, and again to filter. While entries are hidden the list's bottom-right corner says so in DIM: `+7 hidden · 3 of 10`, counting what `.` would add to the tab (the repo tab's repo, or every repo in Agents). `.` on the Pinned tab only says so in the status bar, left out below 26 columns and when nothing is hidden. `N of M` and the mouse rows count what's listed. The Pinned tab and Needs you are unaffected, since pins are chosen and waiting agents are live. The switcher and the repo picker still find everything, and picking a hidden checkout selects it, which lists it. The tier and the folds are worked out from the listed set without the selection's exception, so toggling can change them and moving the selection can't. The footer offers `. show all` (or `. active only`) while it would do something.
- **Hiding is removed (owner).**

## Keymap

AGENT: `nav` → NAV (cursor on current agent). `nav nav` → previous agent, back in AGENT. Everything else goes to the pty.

NAV. The table in `crates/tui/src/keymap.rs` is the source of truth for the footer and the help, and a test checks that every key the handlers match is in it. Keys that do nothing in the focused context aren't offered.

| key | action |
|---|---|
| `⏎` | in [1], attach the selected agent (starts or resurrects as needed) or, on a repo row, focus that repo. In [2] and [3], open the item under the cursor in [0]; on a folder in [3] it opens or closes the folder instead. In the peek, attach |
| `0` | the same as `⏎` in [1], [2] and [3] |
| `i` | attach the selected agent from anywhere, so `⌃\` returns to where you were |
| `s` | send: a one-line box to message the selected live agent without attaching, from any panel and from the peek. On `⏎` an agent at rest gets it now and the box says `send`. Otherwise it is queued and typed the first time the agent is at rest (turn over, no question open, no subagent running), because while it works a permission prompt can open before its report arrives and typed text would pick a menu option. The box says `queue` then, and its title `Send to <agent> · queues until it's at rest`. The worker checks the store again just before typing, and a message that finds the agent busy goes back to the queue. There is one queue per agent, in order, kept across selection changes, and only one message goes in per rest (the next waits for the agent to report again). It shows as `queued: <first words> +n` DIM on the agent's card line 2 (unless the agent is asking or failed), under the turn in [0] while [2] is focused, and as `n queued` in the status bar. `s` on an agent with a queue shows it above the input, and `⌃X` clears it. An agent that reports no hook activity can't be known to be at rest, so `s` refuses it and says so. Nothing is typed unless the agent has reported after its session last became live (seen live in a snapshot, or started or resurrected from here), so a record left by an earlier run never counts. `agent exec` records a launch before it starts the harness (busy, with no subagents or questions), so an agent restarted unseen by the TUI isn't at rest until it reports. A session whose last report was a launch or SessionStart waits 5 seconds more for a startup dialog. Nothing is typed while the user is in AGENT mode on that agent. A send the worker finds busy, or whose store read failed, stays queued until the agent's record is next read. There is no timeout. A record that has itself recorded a launch since the session went live (within 15 seconds) is trusted: its own at-rest and boot-quiet state decides. An agent started by an older binary has no launch to trust, so its queue says `queued · waiting for <agent> to report` and nothing is sent until one hook fires, which takes one interaction with it or a restart through agent. A message that fires no hook (a slash or `!` command) leaves the queue waiting the same way. A failed write drops that message with the reason in the status bar. `q` with messages queued confirms first, since they live only in memory. `t` was the key before and now only says `send is s now` |
| `v` | peek: float the selected agent's live terminal over everything, read-only (see Peek). `v` or `Esc` closes it |
| `h` / `l`, `Tab` / `⇧Tab` | focus the previous / next left panel: [1], [2], [3], wrapping, skipping one that isn't drawn or has nothing to focus. From [0] it goes on from the panel [0] came from. In AGENT the key still reaches the agent |
| `1` / `2` / `3` | focus [1] the fleet (and the terminal in [0] without typing into it), [2] the turn, [3] Changes |
| `Esc` | back one level: a focused document in [0] to its panel, [2] or [3] to [1], then in [1] to AGENT on the agent you left (offered in the footer while that agent isn't the selection) |
| `j`/`k`, `↓`/`↑`, `g`/`G`, `Home`/`End` | move the selection in [1], the cursor over [2]'s rows or [3]'s rows. With [0] focused, scroll it and jump to its top and bottom (`G` reads `follow` while a transcript is paused). With [0] on a subagent's log, `j`/`k` move the entry cursor |
| `J`/`K` | from [2] or [3], scroll [0] two rows without focusing it |
| `⌃D`/`⌃U` | half a page of the list, or of [0] from [2] or [3] |
| `PgDn`/`PgUp` | a page of the focused panel as last drawn |
| `[` / `]` | previous / next tab of [1] (agents, repo, pinned). Elsewhere the status bar says the panel has no tabs. The footer's `]` hint names the next tab |
| `{` / `}` | previous / next repo |
| `+` / `_` | [0] full width / back |
| `.` | in [1], show every checkout, or only those with a session (the default). The pinned tab always lists every pin |
| `Space` / `/` | switcher |
| `p` | repo picker: focus a listed repo, or add one from the catalogue or by path |
| `m` | pin / unpin |
| `a` | attach the next agent that needs you (see Attention). `⌃\ a` from AGENT |
| `n` | new agent form, or start an agent in the selected idle worktree |
| `c` | check out a branch or a GitHub PR link for review: the new-agent form with the branch in place of the prompt |
| `` ` `` | in [3], switch between the directory tree and the flat list |
| `-` / `=` | in [3]'s tree, close / open every folder |
| `e` | edit: in [1] and [0] the checkout in `$EDITOR`, in [3] the file under the cursor (or on a folder, `$EDITOR` in that folder), in [2] on a subagent row, and in [0] on a subagent's log, that subagent's transcript as Markdown (at the cursor's entry from [0]), run in the agent's checkout. Not offered on [2]'s turn row |
| `d` | kill session, **with confirmation (owner)**: `y` or `⏎`, since it can be resurrected |
| `D` | remove session, worktree and branch (confirmation, or `Y` if commits would be lost) |
| `x` | actions overlay ("Run on <agent>"): the configured actions, one key each. `Esc` closes |
| `?` | keybindings (see Help) |
| `q` | quit |

## Help

`?` opens a lazygit-style keybindings list: a rounded box titled `Keybindings` with `(type to filter)` on the right and `n of m` in the bottom-right corner. Rows are `key  description`, in a `Local` section (the focused panel's own keys, named beside the title) and then `Global`. The row under the cursor is reversed, and below the list a `Description` box gives the longer text for it. `↓`/`↑`, `j`/`k`, `⌃N`/`⌃P`, `Tab`/`⇧Tab`, `PgDn`/`PgUp`, `Home`/`End` move. Typing filters case-insensitively across the keys and descriptions, `Backspace` edits the filter, and `⏎` keeps the filter and returns to moving (`j` and `k` move again, and `/` edits the filter). While typing, `Esc` clears the filter and stops typing. Otherwise `?`, `q` and `Esc` close. The Description box's border names these. `j`, `k`, `q` and `?` are letters from the first other character or `/` typed until `⏎`. The list scrolls when it doesn't fit (it works at 80x24). Bindings that don't apply in the focused context aren't listed rather than struck through, which keeps the list short enough to scan at 80x24.

## Overlays

- **Switcher**: input focused on open. Index 0 is the previous agent, then pins, then MRU, then the rest. It never lists the TUI's own session. Rows use the Overview grammar: a main checkout reads `repo branch` (branch DIM) with nothing on the right. Other agents show their repo on the right (DIM). Age arrives with status. `⌃N`/`⌃P`/`⌃\` move, `⏎` switches into AGENT, and `Esc` returns unchanged. Rows: glyph, `agent`, repo DIM, age DIM. Matched characters BOLD Cyan. Rounded DIM border, BOLD title, hints in the bottom border.
- **Repo picker** (`p`): the same list grammar as the switcher. Rows are the sidebar's repos first, then the catalogue's repos the sidebar doesn't show, each group with an exact name first, then prefixes, then the rest. Every row ends with where the repo is (DIM, as `~/repos`), since a name alone can't tell two clones apart, and a listed row puts its live count before that (`● 2  ~/code`, default fg like the sidebar's counts). Text that starts like a path (`/`, `~`, `~/`, `.`, `./`, `../`) is the one row: the path keeping its end, and `add its repo` DIM. Whether it is one is checked off the UI thread when picked, and a failure lands in the status bar. With no row, the one row says `no match` (DIM) or `~user paths aren't supported` (Red) and the border offers only `Esc close`. Otherwise the bottom hint says what `⏎` does with the row under the cursor: `focus` a listed repo, `add` anything else. Adding lists the repo and focuses it once the next refresh shows it, unless a launch is already being followed. While the switcher or the picker is open the footer carries only `? keys`, since the border has the keys.
- **Check out** (`c`): the new-agent form (`Check out for review`) with its first field `branch` taking a branch name or a GitHub pull request link, and no `⌃E` or `⌃J`. The name is suggested from the branch (its words, lowercased, cut at the last whole word that fits) or the link (`pr-482`). It's never asked of the model and never made unique, since the branch decides: a branch already checked out in an agent worktree attaches to that agent whatever the form says. A link's problem shows under the branch field as it's pasted, a branch's only on `⏎`, since a branch is invalid mid-word. A pull request link also names its repo: with nothing typed in the repo field, the one listed or catalogued repo of that name is used, and otherwise the right of the repo row says `the link is acme/api` in Yellow. A branch git doesn't know is fetched from origin first, and a local branch of that name wins without a fetch. A pull request is asked of `gh`: from the repo itself it's checked out as its branch, so the agent can push to it, and from a fork its head is fetched by the `refs/pull/N/head` ref onto a `pr/N` branch tracking nothing, since origin has no branch of its own for it. A name that is an agent in another repo, or on another branch, is refused under the name before anything runs, in Red. All of it runs off the UI thread. Worker errors wrap under the name rather than being cut. The agent starts with no prompt.
- **Confirm**: sized to its content, with two short lines and hints in the bottom border padded by one space each side, the confirming key first and always BOLD Red (it must differ from the border), alternatives after it (`── Y discard and remove  k kill only  n/Esc cancel ─╯`). Border Yellow, Red when data would be lost, and the destructive key BOLD Red. Unmerged work lists the commit count per branch.
- **Help**: the full keymap. Section headers are the same reversed-Cyan badges as the footer (`AGENT`, `NAV`). Keys and descriptions in default fg (DIM is unreadable on some themes at this density). Sized to fit 80x24, in two columns when the terminal is short.

## Empty and transient states

One centred line in the terminal area, verb first, never a second hint block (the footer already has the keys): `⏎ resurrect tanda_cli`, `⏎ start claude here` (key BOLD), `attaching to fix…`, `this TUI runs here` (DIM), `No agents yet` (DIM).

## Truncation

Names keep their start (`payroll-fix-for-ti…`), paths keep their end (`…/heucristic/triage-flow`), measured in display width. Right-aligned metadata never truncates, so names yield first.
