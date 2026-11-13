# agent

A Rust workspace that runs coding agents in git worktrees, one zellij session per agent, behind a ratatui TUI and a CLI. User-facing behaviour is documented in the user docs (currently `~/.config/docs/parallel-claude.md`). The UI and interaction design is [docs/design.md](docs/design.md). This file covers the code.

Type-driven design comes first. Every invariant the code relies on lives in a type, so the wrong call doesn't compile. Then responsiveness: the UI thread never waits on git, a process or the filesystem. Then harness independence: Claude Code is one harness, zellij is one session host, and the core knows neither.

## Definition of done

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must pass. After a change the user will run, also run `./install.sh`, so the installed `agent` is the one they get.

Every crate sets `[lints] workspace = true`: `unsafe_code` is forbidden and clippy pedantic warns. Lint levels live only in the root `Cargo.toml`. Never add `#[allow]` or `#[expect]` in source, with or without a reason (see Lints are met, not silenced).

A bug fix ships with a regression test verified to fail without the fix. Run it against the unfixed code and see it fail before calling it done.

A change to what the user sees updates, in the same change, `docs/design.md`, the user docs and the `BINDINGS` table in `crates/tui/src/keymap.rs`. The help and the footer read that table, so a key missing from it is missing from both.

## Tools

```sh
./install.sh                                                  # cargo install into ~/.local/bin/agent
cargo run -p agent --example harness -- <bin> [steps...]      # run a binary in a pty, steps: wait:<ms> keys:<text> dump:<label> raw:<file>
agent dump <name>                                             # print an agent's screen with ANSI styling
```

Session layouts record `~/.local/bin/agent` (`agent exec`), so install with `install.sh` and never point a session at a `target/` build.

Never run the TUI or the harness example against the user's zellij server. The TUI previews the selected live session, which attaches a client and shrinks the user's view. Every run sets all of these and runs from inside a scratch repo:

```sh
export ZELLIJ_SOCKET_DIR=$(mktemp -d /tmp/zj.XXXX)    # short: zellij refuses socket paths over 103 bytes
export XDG_STATE_HOME=<scratch>/state
export AGENT_WORKTREES=<scratch>/worktrees
unset ZELLIJ ZELLIJ_SESSION_NAME ZELLIJ_PANE_ID
```

Zellij's resurrection cache is still shared, so the user's exited sessions show up. Never press `⏎` on one. Tear down by killing and deleting the throwaway sessions by name, with `ZELLIJ_SOCKET_DIR` set in the same command. Never `zellij kill-all-sessions`, which is one unset variable away from the user's server. Then remove the socket and scratch directories.

Never `git stash`, `git checkout`, `git reset` or `git clean` in a tree other agents may be working in.

## Architecture

The pipeline is: discover repos, worktrees and sessions into a `Snapshot`, read each agent's `Activity` from the reports its harness sends through `agent event`, then let the TUI show the fleet and turn keys into effects that the ports carry out. Everything is under `crates/`.

- `core` (`agent-core`): the domain and the ports, with no I/O of its own beyond `process` helpers. `ports` holds the traits adapters implement: `Vcs`, `Forge`, `SessionHost`, `ClientControl`, `Harness`, `Harnesses`, `Store`. `ops` holds the use cases over `Services` (borrowed from a `Backend` of boxed ports): `snapshot`, `plan`, `create`, `attach`, `checkout`, `plan_removal`, `remove`, `plan_restart`, `plan_restarts`, `restart`. `model` holds `Snapshot`, `Entry` and the proof types `Running` and `Startable`. `activity` is the state machine `Activity::next(previous, reported, now)` over `State` (`Idle | Working | Delegating | Done | Failed | Interrupted | Exited`). A launch is not work: `Session::Launched` makes the agent `Idle`, and `Activity::boot(now)` (`Boot::Launching` or `Boot::Starting`) says when the session is still coming up, which `hold` and the card's `AgentStatus::Starting` both read. A subagent's `Liveness` is its recorded status as of a moment, `Lost` when it has gone quiet and has no question open, so `Activity::liveness(id, now)` and `Activity::running(now)` take the time and its status and `heard` are private. Background commands (`BackgroundCommands`, from `CommandListing::Listed` in a `Finished`) outlive prompts, failures and interrupts, are replaced by each listing (`CommandListing::Unknown` leaves them), are cleared by a launch, a start or an end, and never change `State`. The TUI reads them for a live session only, through `Model::commands`, which cards, the status row and [2] all call. `core::visible` is the one rule for what shows (`has_visible`), applied by `CommandLine` and `TaskId`, which can only be made through it (including when read from a file, where a bad one drops only the commands). Core can't see terminal escapes, so `agent-term` applies the same rule to the cleaned text (the TUI draws it, the CLI prints it): a `ShownName` is only built from non-empty printable text (the command's first line that shows, else its id), and a command with no such name is left out of `Model::commands`, so it is never drawn or counted on a card or in [2]. A restart is the exception: it counts every command (`CommandsShown`'s `Remainder`), because the user must know how many it may stop even when a name can't be shown. [2] is one value, `Model::activity_list` (`ActivityList`: the subagents, `Option<Commands>` non-empty, and the cursor as a `Spot` found once while it is built), which drawing, hover, moves, `n of m` and the hit rows (`ActivityHit`, addressing a row by `ActivityRow`) all read, with no index that has to line up with a line. The scroll [2] was drawn at is a `FirstLine` in `Areas`, not derived from the hits, since the heading is a line no hit stands for. `ActivityRow::Subagent` and `Command` carry their agent, so a stored row can't resolve against another agent's list. `hold_activity_row` is the one rule that keeps the subagent or command under the cursor after its listing drops it (a `HeldCommand` in the agent's `Tracked`, in its old slot) and forgets every other held row, and it puts a stored command row back on the activity once neither the listing nor the hold names it. `Gist::Commands` is the second line of an idle card. `ops` brackets the launches whose outcome it sees (`start` and `exec_spec`) with a `LaunchMark` from `mark_launch`, and `LaunchMark::settle` is the only way to finish one: it ends the record when the launch failed. A resurrection (`mark_resurrection`) only marks, last, after everything that can fail, since the attach runs after `ops` returns and `exec_spec` ends the record if the launch then fails. A failed mark write doesn't stop `exec_spec`. A harness with no interrupt event says so in each report (`InterruptSignal::Transcript`), and `Harness::interruption` reads its transcript. What the read concludes is applied under a proof (`Going` or `Stopped`, made only by `Activity::going` and `Activity::stopped`) that the activity hasn't had a report since, through `Activity::interrupted` and `Activity::resumed`. `Activity::hold` says when typed text could land in a menu or nowhere (asking, booting, ended), and `ops::inject` refuses with `Error::Held` under it, so the TUI's worker and the CLI type now or type nothing. Nothing waits to be sent later. What the user has written for an agent is its `Compose` in `Tracked`, `Editing` or `Sending` under the `Ticket` the worker answers for. `names` parses `SessionName`, `AgentName`, `BranchName` and `Prompt`. `NameLimit` is the host's session name limit, from `SessionHost::name_limit`. `NameLimit::admit` turns a `SessionName` into a `HostName`, which records the limit it was admitted under and is the proof `SessionHost::create` takes. `HostedWorktree` and `HostedRepo` admit a checkout's own name, and every `Startable` and new worktree holds one. A worktree over the limit is still listed and removable, as `Hosting::Unhosted`, and its entry is `EntryState::Unhostable` instead of startable. `paths` holds `RepoRoot`, `AgentWorktree` and `Layout`. `config`, `theme` and `actions` parse `config.toml`. `discover`, `naming`, `process`. `testing` holds the in-memory ports (`FakeVcs`, `FakeForge`, `FakeHost`, `MemStore`, `FakeHarness`), and `activity::testing` the builders for an `Activity` that need its private fields (`with_commands`), both behind the `testing` feature.
- `harness` (`agent-harness`): `Claude` implements `Harness`, `Registry` implements `Harnesses`. Everything Claude-specific lives here: the launch command and its hooks, resuming, transcript parsing (`transcript`, `activity`), `Conversation` (the session ids a conversation has run under and which holds the messages, followed through `continued-in` records, cached per chain by `Chains` until a transcript in it changes) and the background session registry (`background`: `Survivor`, found by session chain, directory and process start, attached or stopped by `JobId`). `Harness::survivor` is the port to it and answers a `Lookup`: a `Survivor` that carries its own `Stop`, a process that is alive but not stoppable yet (`NotYet`, Claude still shows it as interactive), or nothing. `Claude` finds the process through the registry (by the chain of the transcript the hooks named and the resumable one), stops it with `claude stop`, verifies by pid and start time that it is gone, and falls back to SIGTERM on the verified pid. `stopping` holds that logic over an injected `Processes`, so no test signals anything.
- `git` (`agent-git`): `Git` implements `Vcs`, `GitHub` implements `Forge` through `gh`.
- `zellij` (`agent-zellij`): `Zellij` implements `SessionHost` and `ClientControl`. Everything zellij-specific lives here: layouts, session listing, client discovery.
- `store` (`agent-store`): `FsStore` implements `Store` over the state directory.
- `term` (`agent-term`): a pty and a vt100 `Emulator` behind `Terminal`, `TermSize`, key and mouse encoding, `strip_escapes`, the one reader of terminal escape sequences, `printable` and `truncate_end`, and what a background command is called on a screen (`ShownName`, `CommandsShown`), so the TUI and the CLI name commands the same way. Depends on `agent-core`.
- `tui` (`agent-tui`): the TUI. Depends on `agent-core` and `agent-term` only.
- `agent`: the binary. Parses the CLI with clap, `restart` holds the `agent restart` command over a `Console` (terminal or not, output, questions, and the hand-over of its own session), handles `agent event` before clap, and is the only place that builds a `Backend` from concrete adapters. The environment is read once at startup (`Layout::from_env`, `FsStore::from_env`, `Colours::from_env`, `XDG_CONFIG_HOME`, `HOME`).

The TUI is an Elm loop (`crates/tui/src/lib.rs`, `event_loop`):

```
input thread ─┐                                   ┌─▶ Runtime::apply ──▶ Workers (threads) ──┐
workers ──────┴─▶ Msg ──▶ update(&mut Model) ──▶ Vec<Effect>                                │
                              │                                                             │
                           settle                                                         Msg
                              │
              view::draw(&Model) ──▶ Drawn ──▶ update::drawn
```

- `model`: `Model`, the whole state. `Pane`, `Connection`, `Peek`, `Watcher`, `Overlay`, `Load`, `Ticket`, `Moment`.
- `update`: `update(m, msg) -> Vec<Effect>`, `tick_at(m, now)`, `settle`, `drawn`. Every decision is made here. `Effect` describes work, `Background` is what a new connection starts.
- `worker`: `Msg`, `Job`, `Workers`, and `Queue<K, V>` serving keyed requests on thread pools.
- `view`: `draw(frame, &Model) -> Drawn`. `view/sidebar.rs` draws [1], [2] and [3], `view/page.rs` and `view/log.rs` draw [0] when it reads a document.
- `keymap`: `BINDINGS`, the one table of NAV and peek keys, and `Situation`, what the labels depend on.
- `palette`: the `[theme]` resolved against the terminal into a `Palette`.
- `document`, `log`, `review`, `tree`, `touched`, `track`: what [0] reads, a log's entries and cursor, diff lines and drafted comments, [3]'s rows, the files an activity touched, and `Tracked`, everything kept per agent.
- `fixtures` (tests only): models built the way discovery builds them.

### Invariants

- `core` names no harness, no session host and no terminal. A Claude hook name, a zellij flag or a wezterm variable in `core` or `tui` is a bug. Terminal-specific behaviour is user config (`terminal_event` actions).
- A host's limits are capabilities of its `SessionHost` adapter. Zellij declares its own session-name limit (its socket path budget), and `core` and `tui` speak of "the session host's limit", in code and in UI copy, never of zellij.
- `tui` reaches the world only through `Backend` on worker threads. It never imports `agent-git`, `agent-zellij`, `agent-harness` or `agent-store`.
- A session is killed only through a `Running`. `Running::new` is private to `agent-core`, so outside it a `Running` comes only from `Entry::running(own)` or a `RemovalPlan`, and the TUI can never kill its own session. A worktree is removed only through an `Approved`, which only `RemovalPlan::approve` or `Hazardous::force` builds. `plan_removal` returns `Planned::Own` for the caller's own session (`Services::own`, read once at startup), which has no `approve`, so `remove` is never handed a removal of the session it runs in.
- An agent is restarted only through a `Restart`, which only a `RestartApproval::Free` (nothing to stop) or `WouldStop::force` (the user saw what it would stop) builds. What it would stop is a `Stops`: the activity, the background commands the harness runs, or both, never neither, held once by the `WouldStop` and by the `Restart` the user agreed to, so an idle agent with commands is a `WouldStop` too and `Error::WouldStop` carries the same `Stops`. `ops::restart` plans and rechecks with one rule (`would_stop`, over a `Seen` that says where the harness process was looked for, so an exited agent with no process carries no commands) and refuses a restart that would stop more than the user was shown: a new kind of stop, or a command that wasn't listed. Core carries every command (`BackgroundCommands`, so the count is honest); `agent-term`'s `CommandsShown` names the first few that print something once cleaned and counts the rest (`Remainder`), for both the TUI and the CLI. `plan_restart` returns `Planned::Own` for the caller's own session, which has no approval, so a restart never kills the session it runs in. `ops::restart` fixes the order: recheck what it would stop before touching anything, kill the session and wait until the host stops listing it, stop the harness's process and wait until it is gone, then start with the launch marked until the session shows. The restart thread's answer follows a fresh snapshot on the same channel, so the model sees the session before it hears the restart finished.
- The pane's client is a `Connection { mode, id, .. }`. A read-only view is a `Watcher`, owned by the peek float (`Peek::Watch`, `WatchView::Watched`), so closing the float ends it. A `Peek::Pane` has no name of its own and shows the pane's session, so `Model::set_pane`, the one writer of the pane, closes it when the pane moves to another session or empties. Only a `Connection` takes keys and has a `ClientId`, so a watcher can't be mistaken for the TUI's own client.
- `Model.clock` is the one clock. Whatever delivers a message sets it. Nothing calls `Instant::now` or `SystemTime::now` outside `Moment::now` and tests.
- `settle` runs after every message and tick, so `view::draw` only reads. `Drawn` is the draw's one write-back, applied by `update::drawn`.
- The runtime creates resources `update` can't (a `Terminal` spawns a process) and hands them back through an update function: `Runtime::connect` installs the connection only while `Pane::awaiting(ticket)`, then calls `update::connected`.
- Update this section in the same change that changes the shape.

## Comments

A comment explains a non-obvious why. A `///` doc comment may also state a contract the signature can't express: units, ordering, when it's called, what a failure means. A doc comment that only restates what an item is isn't allowed, and existing ones are pruned when their code is touched. If code needs a comment saying what it does, rename or restructure it until it doesn't. Comments in the examples below are teaching notes.

UI copy (status messages, errors, help text) and docs say what happened or what a key does in the fewest words, with no clauses joined by `;` or a dash. Exact wording for surfaces is in `docs/design.md`.

```rust
// Bad: says what the name and the signature already say
/// Asks for git facts about an agent's worktree.
pub fn inspect(&self, name: SessionName, worktree: PathBuf, events: Option<u64>);

// Good: states what the signature can't, and who relies on it
/// A request still waiting for `key` is replaced and never answered.
/// `Documents` relies on it: its answer to the newer request drops the
/// older ones for the same slot from `requested`, which would
/// otherwise wait forever.
fn request(&self, key: K, value: V);
```

## Principles

Every bug a reviewer finds gets one question first: what representation makes this unrepresentable? A fix of the form "call the right accessor" or "add a guard" is rejected when a type change can make the wrong call impossible. A guard is acceptable only with a stated reason a type change isn't practical.

The TUI once counted its own client as another window, because a read-only watcher and a client were both a `Connection` and the pid was read from either. The accessor fix was "use `client()` instead of `connection()`". The type fix makes them different types, and only the client carries an id:

```rust
// Bad: every caller must remember a watcher's pid isn't a client id
pub struct Connection {
    pub term: Terminal<Box<dyn Emulator>>,
    pub observer: bool,
}

fn own_client(m: &Model) -> Option<u32> {
    m.pane.connection().map(|c| c.term.pid())
}

// Good: a watcher has no id to confuse, and it belongs to the float, not the pane
pub struct Connection {
    pub term: Terminal<Box<dyn Emulator>>,
    pub mode: Mode,
    pub id: ClientId,
    // ...
}

pub struct Watcher {
    pub term: Terminal<Box<dyn Emulator>>,
    // ...
}
```

### Make illegal states unrepresentable

If a field only means something in some states, or two fields can contradict, use an enum with the data inside its variant. No sentinels: no empty string, `0` or `-1` for "none", and no `unwrap_or_default()` that turns a missing value into a plausible one.

```rust
// Bad: Connecting with no ticket, or Connected with a stale name, both compile
pub struct Pane {
    pub name: Option<SessionName>,
    pub ticket: Option<Ticket>,
    pub connection: Option<Connection>,
}

// Good
pub enum Pane {
    Empty,
    Checking { name: SessionName, then: AfterCheck, until: Instant },
    Connecting { name: SessionName, ticket: Ticket },
    Connected(Connection),
    Elsewhere { name: SessionName },
}
```

A collection that must be non-empty is built non-empty. `Company::Elsewhere(Windows)` holds a `first: Place` and `rest: Vec<Place>`, so "shown elsewhere in no windows" can't exist.

### Parse, don't validate

Convert raw input into a typed value once, at the boundary: CLI arguments, config, hook payloads, `git` and `zellij` output, terminal sizes. Past the boundary nothing re-checks it. `SessionName`, `AgentName`, `BranchName`, `Prompt` and `HarnessId` implement `FromStr` and are the only way in.

Strengthen the input instead of weakening the output:

```rust
// Bad: every caller handles a zero it could have ruled out
fn resize(&mut self, rows: u16, cols: u16) -> Result<(), ResizeError> {
    if rows == 0 || cols == 0 {
        return Err(ResizeError::Empty);
    }
    // ...
}

// Good: a zero dimension can't be passed
pub struct TermSize {
    pub rows: NonZeroU16,
    pub cols: NonZeroU16,
}

impl TermSize {
    pub fn of(area: Rect) -> Option<Self> {
        Some(Self {
            rows: NonZeroU16::new(area.height)?,
            cols: NonZeroU16::new(area.width)?,
        })
    }
}
```

### Proofs, not booleans

A `bool` returned from a check throws away what was checked. The action then trusts that someone checked. Return a proof the action requires instead.

```rust
// Bad: nothing stops a caller killing the TUI's own session
if entry.is_live() && Some(&entry.name) != own {
    host.kill(&entry.name)?;
}

// Good: kill takes a Running, which callers outside core can't build
pub fn running(&self, own: Option<&SessionName>) -> Option<Running>;
fn kill(&self, session: &Running) -> Result<(), HostError>;
```

Treat every `bool` field, `bool`-returning method and `Option` checked with `is_some()` to make a decision as a suspect. Two bools that only make sense together are one enum.

### No primitives where a type belongs

A `bool`, integer or `String` that stands for a concept gets a type. A `bool` that picks behaviour becomes a two-variant enum, so the call site says which.

```rust
// Bad
pub armed: bool,
pub struct Look { pub urgent: bool, /* ... */ }

// Good
pub enum Chord { Idle, Armed }
pub enum Need { Refresh, Decision }
```

A request id is a `Ticket`, not a `u64`. A client is a `ClientId`, not a `u32`. An error the UI matches on is an enum (`SendError`), not a `String`.

### One fact, one place

If two fields can disagree about the same fact, one is a bug waiting. A pure projection of other state is a function, not a field.

```rust
// Bad: a copy of what geometry(m.size, m.screen) already says, stale until someone refreshes it
pub sidebar_shown: bool,

// Good
fn sidebar_shown(m: &Model) -> bool {
    crate::view::geometry(m.size, m.screen).sidebar_shown
}
```

Everything known about one agent lives in one `Tracked`, so pruning the agent drops all of it. Deliberate snapshots are not duplicates: `Follow::Submitted { name, ticket }` captures the launch it follows and is meant to outlive a refresh.

### Effects are described, not performed

No filesystem, process, git or store access in `update`, `model` or `view`. A handler returns the `Effect` describing the work, `Runtime::apply` sends it to a worker, and the answer comes back as a `Msg`. The model records that it asked, so it asks once and can tell its own answer.

```rust
// Bad: a stat on the UI thread, and a slow network mount freezes the TUI
let found = if std::fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
    Found::Dir
} else {
    Found::Nothing
};

// Good
form.dir = DirCheck::Asked(path.clone());
Some(Effect::Job(Job::CheckDir(path)))
```

Every request settles in every future: success, failure, a stale ticket and a quit each land somewhere. A worker turns an error into a `Msg`, never a panic or a silent drop.

### Correlate replies with the state that asked

A reply can arrive after the user moved on. Every request carries the key of the state that asked, and the handler checks it before writing.

```rust
Msg::RemovalPlanned { ticket, result } => {
    if !matches!(&m.overlay, Overlay::Planning { ticket: t, .. } if *t == ticket) {
        return Vec::new();
    }
    // ...
}
```

`Model::ticket` issues tickets. `Pane::awaiting(ticket)` gates connections, `Model::start_look` makes only the newest look for other windows count, `DirCheck::awaits(path)` gates the repo field. An overlay carries the agent it acts on (`Overlay::Talk(Talk { to, .. })`, `CommentDraft.to`), never read from the selection at submit time.

### Exhaustive dispatch, projecting fallbacks

A match that picks behaviour per variant lists every variant, so a new variant breaks the build there instead of silently doing nothing. A `_ =>` is legal only in a projection whose fallback is right for every future variant, and on foreign `#[non_exhaustive]` enums such as `KeyCode`.

```rust
// Bad: add a Pane variant and its company changes are ignored
match &m.pane {
    Pane::Checking { name, then, .. } => checked(m, name.clone(), *then),
    Pane::Elsewhere { name } if !m.company(name).is_elsewhere() => reattach_preview(m, name.clone()),
    _ => Vec::new(),
}

// Good: a projection, None is right for anything that isn't Connected
pub fn connection(&self) -> Option<&Connection> {
    match self {
        Self::Connected(c) => Some(c),
        _ => None,
    }
}
```

### Draw is pure

`view::draw(frame, &Model) -> Drawn` reads the model and nothing else: no environment, no clock, no filesystem. What a frame shows is a function of the model, so any frame can be reproduced in a test. Anything a draw would have to decide is decided in `settle` first. What the draw measured (areas, scroll, rows) comes back as `Drawn` and `update::drawn` applies it.

```rust
// Bad: the frame depends on when it was drawn, so no test can reproduce it
&log::Context {
    columns,
    now: SystemTime::now(),
    spinner: spinner(SystemTime::now()),
    // ...
}

// Good
&log::Context {
    columns,
    now: m.clock.wall,
    spinner: spinner(m.clock.wall),
    // ...
}
```

### One clock

`Model.clock` is a `Moment { wall, mono }` read once per message or tick, so everything one message decides agrees on the time. Tests drive time with `update::tick_at(m, moment)`.

```rust
// Bad: two reads disagree within one message, and tests can't control it
if Instant::now().saturating_duration_since(s.at) > INFO_TTL {

// Good
if m.clock.mono.saturating_duration_since(s.at) > INFO_TTL {
```

### Colour comes from the theme

Every colour is a role in `Palette`: `accent`, `dim`, `warning`, `error`, `success`. The defaults are the terminal's ANSI cyan, yellow, red and green, so the user's palette decides what they look like. `view` reads the palette through `accent()`, `dim()`, `warning()`, `error()` and `success()`, set from `m.palette` at the start of each draw.

```rust
// Bad: ignores [theme] and NO_COLOR
Span::styled(format!("the link is {}", pr.slug()), Style::new().fg(Color::Yellow))

// Good
Span::styled(format!("the link is {}", pr.slug()), warning())
```

- No raw `Color` outside `palette` and tests.
- Backgrounds are `Color::Reset`. Never `DarkGray`, which is the background in Solarized.
- `dim` is `Secondary::Colour` (the foreground blended into the background until 3:1 contrast) when `COLORTERM` promises 24-bit colour and the terminal answered `OSC 10`/`OSC 11` at startup. Otherwise it is `Secondary::Faint`. The colours are read once, before the input thread takes the tty.
- `NO_COLOR` resolves to `Palette::MONOCHROME`. Modifiers survive.
- Colour never carries a state alone. Every state has a glyph, so it reads in monochrome.
- `every_colour_comes_from_the_palette` draws every panel with a sentinel palette and fails on any other colour. A new surface is added to it.

Which glyph, modifier or role a surface uses is `docs/design.md`'s decision.

### Lints are met, not silenced

Clippy pedantic passes with no `#[allow]` and no `#[expect]`, ever, reason or not. Box a large variant (`Overlay::New(Box<NewAgent>)`) instead of allowing `large_enum_variant` or `result_large_err`. Split a function instead of allowing `too_many_lines`. Replace a struct's bools with enums instead of allowing `struct_excessive_bools`. No `Deref` impls to make a newtype pass as its inner type: give it the methods it needs. If a lint can't be met by rewriting, or conflicts with these principles, raise it with the user.

## Concurrency

- The UI thread runs `event_loop`: paint, tick, update, apply. It never runs git, zellij, a harness or the store. It does spawn a connection's pty and hands the terminal to `$EDITOR` or an interactive action, pausing the input thread first (`InputCtl::Pause`).
- The input thread forwards terminal events as `Msg`. It stops reading the tty while paused, so an editor gets every keystroke.
- The refresh thread builds a `Snapshot` on request, coalescing requests, and polls `Store::activities` every `ACTIVITY_POLL`.
- The action worker runs `Job`s in order. A mutating job triggers a refresh. `Workers` joins it on drop, so quitting can't interrupt a removal halfway.
- Restarts run on one thread of their own, from a FIFO that is not a `Queue`, so none is replaced. Stopping a harness can take seconds, which would hold up kills and removals on the action worker. `Workers` drops the restarts still waiting on drop and joins the one in flight. `Model.restarts` is the batch handed to it, and a card reads `restarting` from that batch alone.
- Reads that can be slow run on pools behind `Queue<K, V>`: git facts, documents, subagent logs, activities, interrupt probes, name suggestions. A key is served by one thread at a time, and a waiting request for the same key is replaced by the newer one, so the answer reflects the latest ask. Looks for other windows run on two `Lookouts`, so a decision is never queued behind a refresh.
- A slow repo holds up only its own agents. Never add a pool that serialises unrelated keys.
- Every worker answer is a `Msg` carrying its correlation key. Workers never touch the `Model`.

## Testing

- `update` is tested by building a `Model` with `fixtures` (`fleet`, `connect`, `sighted`), sending `Msg`s through `update` or `tick_at`, and asserting on the returned `Effect`s and the model. No threads, no terminal.
- `view` is tested by drawing to ratatui's `TestBackend` and reading the buffer. Layout tests run at every size in `SIZES`, down to 80x24.
- `ops` is tested over the in-memory ports in `core::testing`. Their `Journal` records kills, stops, reports, creates and detaches in order, for tests of what must come before what, and `World` wires every fake to one of them with agents in it. Adapters are tested on their parsers with captured output, and `git` and `store` against real repos and directories under `tempfile::tempdir()`.
- `nav_keys_do_something_exactly_where_the_keymap_table_says` presses every key in every context and fails when a handler and `BINDINGS` disagree. A new key goes in the table, not in `RETIRED_KEYS` (in the `view/sidebar.rs` tests).
- A test name is a sentence saying what holds: `uncommitted_work_outranks_how_far_ahead_or_behind_it_is`.
- Never `sleep` to wait on async work. Drive time with `tick_at` and feed the `Msg` the worker would send.
- No test touches the user's state, worktrees or zellij server. A test needing a path gets a temp dir. A test needing a session uses `FakeHost`.

## Review

Every step is reviewed before it is committed, however small. Three adversarial reviewers run in parallel, each with the diff and this file:

- Principles: this file's principles and invariants. It also receives the other reviewers' bug lists and applies the type-level rule at the top of Principles to each.
- Correctness: edge cases, races between replies and state, and test quality. A regression test that would pass without the fix is a finding.
- Redundancy: dead code and parameters, duplicated helpers, reinvented utilities, derivable parallel state.

UI changes are also reviewed against `docs/design.md`. Fixes are re-reviewed narrowly. Nothing is committed until the reviews come back with no findings.

A commit message is one concise title line, with no body and no attribution.
