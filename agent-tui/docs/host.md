# Native session host

Design for replacing zellij with a session host that lives in this repo. The wire protocol (P1, `crates/host`), the host with a raw client (P2a), the query answers and escape filter (P2b, both in `crates/session`), the `Native` adapter and router (P3) and the clients (P4) are built. The host and client are reachable through the hidden `agent session` commands, and `[host] kind` picks which host makes new sessions. Zellij stays the default until a phase says otherwise.

`agent attach` already exists: it creates an agent's worktree and session. It keeps that meaning. The native host and client are `agent session host <name>` and `agent session attach <name>`, hidden, so nothing user-facing changes. A later phase gives `agent attach` a native path. The planned names below (`agent host`, `agent attach --watch`) are written as `agent session ...` where P2a built them.

## Goals and non-goals

Goals:

- Every key reaches the TUI or the agent. No multiplexer keymap sits in between.
- A session survives terminal close, sleep and reboot (an exited session resumes through the harness), and attaches from another terminal, SSH, a phone and nvim.
- No dependence on macOS or wezterm. Linux and macOS both work.
- Live zellij sessions keep running through the migration.

Non-goals:

- Ad-hoc panes. A session is one harness in one pty.
- Windows. The adapter is `cfg(unix)`. The port boundary keeps a named-pipe host possible.
- Zellij's scroll mode and `Ctrl+s e`. They are replaced by `agent dump --scrollback` (see Scrollback).

## Decisions

| | Decision | Rejected |
|---|---|---|
| Backend | A native host: one `agent session host <name>` process per session, owning the pty and the harness, serving a unix socket. | tmux (a key-grabbing multiplexer again), dtach and abduco (no typed client list, no watch-without-resize, no screen replay together). |
| D1 Ownership | A `Window` (an interactive client of a human) always takes ownership from a `Preview` (the TUI's own preview connection) without asking. Between two windows the newcomer takes over by default and the old owner becomes a watcher that can reclaim with a keypress. `agent session attach --as window|polite|preview|watch|peek` picks the role. A displaced preview waits, and takes ownership again only when nobody owns the session, which resizes the harness to its size. Previews never claim a session a window owns, and a window that was taken over from claims it back with a keypress, never automatically. | Refusing a busy session (the TUI's preview would block the user), zellij-style smallest-wins sizing (the squish the TUI works around), tmux-style latest-active-wins with every client interactive (other windows garble at the wrong size). |
| D2 Harness exit | The host exits when the harness exits. The agent shows `○ exited` and `⏎` resumes it. | Keeping a dead pane open with "press ⏎ to re-run", which looks live and holds nothing. |
| D3 Name limit | 48, checked at startup against the actual runtime dir length. It stays 20 while zellij can still create sessions. | Hashed socket names (a lookup table for a limit nobody hits), and keeping 20 (the limit was zellij's). |
| D4 Paths | Sockets in `$XDG_RUNTIME_DIR/agent` on Linux, else `/tmp/agent-<uid>`. The liveness lock lives in the state dir, not in `/tmp`. The host re-creates its socket if the socket inode disappears. | Sockets in the state dir (stale after reboot, longer paths), and the lock beside the socket (see Paths). |
| D5 Emulator | Open: pending the bake-off (see Open decisions). | |
| D6 TUI embedding | Pty-in-pty through `agent session attach --as preview --no-detach-key` first. A direct socket transport later, and optional. | Direct socket first (a larger first step that blocks zellij's removal on TUI rework). |
| D7 Scrollback | Kept by the host, capped by bytes, served by `agent dump --scrollback`. | Capping by lines (a width-dependent cost), and no scrollback (a regression). |
| D8 Detach key | `[keys] detach`, default `ctrl-\`, `none` allowed. On for a tty only, off for embedded clients. Matches the raw byte `0x1c` and the kitty `CSI 92;5u` encoding. | No detach key (SSH users would kill the terminal), and a two-key prefix (more to explain, nothing gained). |
| D9 Cutover | Native becomes the default for new sessions only after the nvim plugin and the TUI work on it. Zellij sessions move on `r` or `R`. The zellij crate is deleted once none remain. | A big-bang switch (kills live agents), and zellij as a permanent option (doubles the host surface). |

## Architecture

### Processes

```
agent session attach / TUI / nvim ──unix socket──▶ agent session host <name> ──pty──▶ the record's pane command (`agent exec <name>`, which execs claude)
```

`agent session host <name>` is a hidden subcommand. It calls `setsid` first, so it has no controlling terminal and gets no SIGHUP when the terminal closes, and points its standard streams at `/dev/null`. It runs the record's command in a pty through `portable-pty` (`agent_term::spawn_hosted`), feeds each output chunk to its screen and to every client's queue, and exits when the child exits.

A long-lived spawner such as the TUI would keep every host it starts as a zombie. Hosts are therefore started through a short-lived `agent session host --spawn` intermediate. It starts the host as its child, waits until the socket is up and the lock is held (10 s at most), and exits, so init adopts the host. The spawner waits for the intermediate only. A test starts a host this way, lets its harness exit and checks the host's pid is gone, not a zombie. The host leads its own session (`getsid` is its own pid), which another test checks.

Liveness is a lock on the record file in the state dir. The host takes an exclusive `flock` on `<state>/hosts/<name>.json` for its lifetime. The kernel drops it when the process dies, so a crashed host never looks live and two hosts can't race for one name. The probe takes it shared without waiting and gives it back (`probe`: `Live`, `Dead` or `Unrecorded`). The file is opened close on exec so the harness doesn't inherit the lock, which a test checks by killing the host with `SIGKILL` while a harness that ignores `SIGHUP` lives on, then starting a new host from the record. The lock is not in `/tmp` because macOS's `/usr/libexec/tmp_cleaner` deletes regular files under `/tmp` untouched for three days, and a held `flock` doesn't update times. A lock file deleted under a live host would let a second host start.

### Paths

- Runtime dir: `$AGENT_RUNTIME_DIR` as given, else `$XDG_RUNTIME_DIR/agent` when set (tmpfs on Linux, cleared at reboot), else `/tmp/agent-<uid>`. Created 0700, and an existing one must be a real directory of ours and is tightened to 0700. A symlink, a file or another user's directory is a typed `RuntimeError`. `RuntimeDir::open` is the only way to a `Runtime`, and a `Runtime` is the only way to a `SocketPath`, so a socket path is never built under an unchecked directory. The socket is `<runtime>/<name>.sock`, mode 0600. A relative `AGENT_RUNTIME_DIR` is an error and not a fallback. A path that doesn't fit `sockaddr_un` (103 bytes on macOS, 107 on Linux) is a `SocketPathTooLong` with both lengths, checked against the operating system by a test.
- Records: `~/.local/state/agent/hosts/<name>.json`, mode 0600, so exited sessions survive reboot. It holds the pane command (`program`, `args`, `cwd`) and is rewritten in place under the lock, never replaced by rename, since a new inode would not be the locked one. Logs: `<state>/hosts/<name>.log`, mode 0600, truncated at each start, warnings and protocol errors only. A host that fails before it can open its log writes the error into it from the `agent` process, except when another host holds the session (exit status 3).
- The socket inode is the one thing `tmp_cleaner` could still remove. The host checks it on a timer and re-creates it if it has gone, so a live host stays reachable.

### Protocol

Length-prefixed frames: a `u32` big-endian length (over 1 MiB closes the connection), a tag byte, then fields, decoded into a closed type at the boundary by a hand-written codec in `agent-host`. Each direction has two stages, each its own frame type with its own `Decoder`, so a frame from the wrong stage is an unknown tag. A `Decoder` reads only the bytes the frame in progress is missing, so the next stage's decoder starts with nothing lost. The tags of `Hello` and `Greeting` (0x80 and up) don't overlap the tags of `Command` and `HostFrame`.

```rust
// client to host: Hello once, then Commands
struct Hello { version: Version, role: Role }
enum Role {
    Window { take: Take, size: TermSize, colours: Option<Colours> },
    Preview { size: TermSize, colours: Option<Colours> },
    Watch { purpose: Purpose },
    Control,
}
enum Take { Over, Politely }
enum Purpose { Window, Preview }
enum Command {
    Input(Payload), Resize(TermSize), Claim(TermSize), Detach(ClientRef),
    Inject(Payload), Snapshot { scrollback: Scrollback }, Clients, Kill, Bye,
}

// host to client: one Greeting, then HostFrames
enum Greeting { Welcome(Welcome), Refused(HelloRefusal) }
struct Welcome { version: Version, size: TermSize, standing: Standing, you: ClientRef }
enum HelloRefusal { Version { supported: VersionRange }, Malformed, HelloTimeout }
enum HostFrame {
    Output(Payload), Resync, Snapshot(Payload), SnapshotEnd,
    Clients(ClientList), Promoted, Demoted { by: ClientInfo },
    Detached { by: ClientInfo }, Exited(ExitStatus), Forbidden { command: CommandKind },
}
enum Standing { Owner, Watcher, Control }
enum ExitStatus { Code(u8), Signal(NonZeroU8), Unknown }
```

Fields that mean nothing for a role don't exist for it: only a window and a preview send a size and colours, only a watcher states a purpose. A `Payload` is capped so no frame exceeds the limit (`Payload::split` cuts longer output), and a `ClientList` holds at most 1024 entries, written back to back with no count. `ClientInfo` is a host-assigned `ClientRef`, the peer's pid, its `Purpose` and its `Standing`. Frames carry no strings.

Ownership (D1): a `Window` with `Take::Over`, the default, takes ownership even from another window. The old owner is sent `Demoted { by }` and keeps watching, and it takes ownership back with `Claim(size)`, answered by `Promoted`. This is also the TUI's "move here". `Take::Politely` owns only when nobody does or a preview does. A `Preview` owns only while no window does and never displaces one, so it watches otherwise. A watcher never sizes the session. `Welcome.you` is the client's own entry in `Clients`.

Stream order on one socket: `Welcome`, then `Output` frames that rebuild the screen (a replay larger than a frame is several), then live `Output`. `Resync` says the `Output` that follows is a fresh replay. A `Control` client gets `Welcome` and nothing else unasked: no replay and no `Output`. `Snapshot` is answered by `Snapshot` chunks then `SnapshotEnd`, which are distinct from `Output` so an attached client can tell them apart. `Exited` and `Detached` end the stream.

A terminal size is at most 1000 by 1000 (`MAX_DIMENSION`). The host allocates a grid of the stated size eagerly, and a stated 20000 by 20000 is tens of GB. A `Hello`, `Resize` or `Claim` over it is malformed: the hello is refused and the connection closed. A `ClientInfo` has no purpose for a control client, so a client that only injects or lists isn't counted as a window.

Ownership as built in P2a, where the protocol leaves a gap:

- A displaced preview is sent `Demoted` too, and stays a waiting watcher. When the owner leaves, a waiting watcher takes ownership with its own size and is sent `Promoted`: a polite window first, then a preview, oldest first. A window that was taken over from does not wait. It claims.
- A `Preview` can't `Claim` from a window (`Forbidden`). A watcher that joined with `Role::Watch` has no size until it claims with one.
- `Detach` of an unknown client is ignored. `Detach` of itself, and any command the standing doesn't have, is answered `Forbidden`. A command from a client the host hasn't seated yet is ignored.
- The host sends `Exited` to every client including control clients, then stays up for `drain`. The socket is unlinked and the lock released as soon as every client is told, so the session can be started again at once. Clients connected by then can still take a snapshot in the drain. A connection made in the moment before the accept thread notices (at most `socket_check`, 250 ms) is served too.
- A client's `Welcome.size` is the pty's size when it joined. Later resizes by the owner are not announced, so a watcher draws at the size it was welcomed at. A frame for that is protocol work for P2c. `Hello.colours` is `None` from the CLI today, so OSC 10 and 11 are never answered in practice.
- At most 1024 clients are seated. Connections served at once, hello stage included, are 256, or fewer when file descriptors are: four a connection from the soft limit less 64, so 48 at the common limit of 256. Four more are kept for control clients, so a full host can still be told to `Kill`. Beyond that a connection is closed with no frame.
- Input and injected bytes are counted against a budget of 1 MiB shared by every client. A client whose bytes don't fit has its socket left unread until they do, so a large paste arrives whole and nothing is dropped from its middle. Output queues are bounded in bytes too. At most two snapshots are in flight, and a third request is `Forbidden`. A command a client sends with its `Hello` while it is held for a synchronized update is handled after its `Welcome`. A control client isn't held. A client whose input waits 10 s for the program to take what came before it is closed, since its thread would otherwise outlive it, and so is one whose connection takes no write for 10 s, which also gives back the permit of a snapshot it never read. A snapshot counts against the client's queue while it waits, and a join's replay doesn't, since a large screen's replay is larger than the queue. The log is capped at 256 KiB and at 20 lines a second, and says how many it left out.

What a standing may send is `Standing::admit`: `Input` and `Resize` for the owner, `Claim` for a watcher, `Detach` for the owner and a control client, and `Inject`, `Snapshot`, `Clients`, `Kill` and `Bye` for all. It returns an `Admitted` holding only the calls that standing has, or the `Forbidden` the host answers with. A client detaching itself is the host's check (its own ref gets `Forbidden`, and `Bye` covers it). `Inject` works from any client without attaching (it is `ops::inject`). `Hello` must arrive within 2 s, else `Refused(HelloTimeout)`. A version outside the host's range gets `Refused(Version)`, and the client says to restart the session to upgrade its host. A harness exit becomes `ExitStatus::from_pty`: a unix exit code is a byte, a signal is a number, and anything else is `Unknown`.

The host reads each client's peer uid and pid from the socket (`getpeereid` and `LOCAL_PEERPID` on macOS, `SO_PEERCRED` on Linux). `Purpose` replaces the `is_tui` argv guess in `crates/zellij/src/lib.rs`.

### Backpressure

The pty reader never waits on a client. Each client has a bounded queue. On overflow the host drops that client's queue and marks it stale. When its socket drains it gets one `Resync` and a fresh replay as `Output` and then live output. A slow phone costs that client a redraw and never the agent's progress.

P2a, before `Resync`: a queue is 4 MiB of frames. A client whose queue is full is closed with a log line and no frame (one whose writer already ended is just removed), and the others are not slowed (a test floods the pty and stops reading one client). `Outbox::deliver` returns `Delivery::Full` and `Core::drop_failed` is the one place that handles it, which is where P2c puts the resync. The pty reader hands output to the core through a bounded channel, so a core that falls behind slows the program, never grows memory.

### Replay on attach

The replay that follows `Welcome` rebuilds the screen on a client that has never seen it: reset SGR, clear, `CSI ?1049h` when the app is on the alternate screen, the emulator's formatted contents, the cursor, and every mode the app set that the emulator doesn't serialise.

Requirements, from a prototype against `vt100` 0.16.2 (an 80x24 pty, a host parser and a client parser built from the replay):

- `vt100`'s `state_formatted()` doesn't enter the alternate screen. After vim quit, 322 cells on the client were wrong. The replay prefixes `CSI ?1049h`, and with it vim, less, top, bash and an ink-like redraw loop replayed with zero differences.
- `DECSTBM` and `CSI s`/`CSI u` are not serialised, and 106 cells diverged after more output. The emulator wrapper (or the emulator chosen in P1.5) must track the scroll region, saved cursor, kitty keyboard flags, focus reporting `?1004`, `modifyOtherKeys` and title stack from the byte stream.
- `vt100` can't serialise the inactive main grid, so a client that attaches during an alternate-screen app sees a blank main screen after the app leaves.
- The replay is built on the host's core thread, in the same step that registers the client's queue, so nothing between the replay and the first `Output` is lost or duplicated.
- The replay is deferred to the end of a synchronized-output frame (`?2026l`), or a short timeout, so a client never starts mid-frame.
- A resize nudge (pty to rows-1 and back, to make the app repaint what the emulator can't serialise) is opt-in per harness and off by default. Claude Code's renderer may reprint the transcript on SIGWINCH. P2a has no nudge. Whether one is needed is unverified and is measured against a real harness.
- All pty resizes are serialised on one host thread.

The prototype compares `vt100` to `vt100`, so it proves self-consistency only. The bake-off adds an independent oracle: a real terminal emulator fed the same recorded captures, compared cell by cell with the client's result.

As built in P2a (`crates/session/src/screen.rs`, `ledger.rs`):

- `Screen` is the host's whole need of an emulator: `feed`, `resize`, `size`, `synchronized`, `replay` and `dump`. `Vt100Screen` implements it with `vt100` and a `Ledger`, a `vte` parser over the same bytes that tracks what `vt100` doesn't serialise: private modes that differ from a terminal's defaults (so `?7l` counts), the kitty keyboard stack, `modifyOtherKeys`, application keypad, the title, and a scroll region for each screen.
- Bytes are fed one at a time through the ledger so an event lands on its own byte. The main screen is captured just before the final byte of the sequence that enters the alternate screen. `CSI s` and `ESC 7` record the cursor, and `CSI u`, which `vt100` ignores, is carried out by moving the cursor there.
- The replay is: reset attributes, the main screen and `CSI ?1049h` when on the alternate screen, the visible screen with its input modes, the region, saved cursors, cursor and attributes, the other tracked modes, the kitty stack, `modifyOtherKeys`, keypad and title. Origin mode (`?6`), `?1047`, a per-screen kitty stack and the title stack are not replayed. `?47` is replayed as `?1049`.
- A resize forgets the captured main screen, since it can't be redrawn at another size, so a client that attaches after a resize during an alternate-screen program sees a blank main screen after the program leaves it.
- A client that arrives while `?2026` is set is held until the update ends or `update_hold` (100 ms) passes.
- Scrollback is `vt100`'s, sized as rows from the byte cap (32 MiB by default) at the width the host started with. `Snapshot` answers with the visible rows as styled text, and with scrollback first for `Full`. It is built on the core thread, so a full dump of a full scrollback holds up reading the program's output for as long as formatting it takes. That time grows with the scrollback and was not measured. At most two dumps are built at once.

### Sizing and ownership

At most one interactive client, the owner, sets the pty size. Watchers never resize and receive output at the host's size. The peek float already crops (`OBSERVED_MAX` in `crates/tui/src/lib.rs`).

D1 decides who the owner is. A `Window` takes ownership from a `Preview` at once and silently, because the TUI's preview exists to be replaced. Between two windows the newcomer takes over, and the previous owner is told (`Demoted`) and kept as a watcher that can press a key to reclaim (`Claim`). `--as window` takes over (the default), `--as polite` owns only an empty session or one a preview owns, and `--as watch` never takes one. This matches "⏎ move here" in the TUI (`close_elsewhere`, `crates/tui/src/worker.rs`).

### Thin client: `agent session attach <name>`

It puts the tty in raw mode, sends `Hello`, writes the replay, then copies stdin to `Input`, `Output` to stdout and SIGWINCH to `Resize`. It replaces `zellij attach` in `SessionHost::attach`, `zellij watch` in `observe`, nvim's `build_attach_cmd` and `zj <agent>` over SSH.

- The detach key is `[keys] detach` (D8). It is on for a tty only. Embedded clients (`--detach-key none`) never grab it, so the TUI keeps `ctrl-\` free.
- On exit it restores the outer terminal: pop kitty flags (`CSI < u`), mouse, bracketed paste and focus modes off, cursor shown, leave the alternate screen, SGR reset. The client sees every byte it writes through a `Ledger`, so it undoes exactly what the program and the host's replay turned on and nothing else. It does so on a detach, `Exited`, `Detached`, the host going away, an error and a panic (a hook restores before the message prints). 
- Threads: one reads the host, one reads stdin, one forwards resizes (`SIGWINCH` through `signal-hook`), and the client loop handles events one at a time. Bytes that may start the detach key wait 50 ms for the rest, so a lone Esc is delayed by that.
- Only a window that was sent `Demoted` sends `Claim` on its next key, which isn't typed. A polite window that waits, a preview and a watcher ignore keys. A promoted client sends its terminal's current size. `--as window|polite|preview|watch|peek` picks the role (`--watch` is `--as watch`), and `--detach-key none` leaves the key to the program. A detach key inside a bracketed paste is text, for at most 8 MiB or 10 s of one that never ends, and a held start of its end marker is not typed as text. The client writes to the host on a thread of its own, so a host that stopped reading doesn't stop the detach key or a signal: typing waits in a queue of 16 MiB and is dropped past that, still scanned for the detach key. A `SIGTERM`, `SIGHUP`, `SIGINT` or `SIGQUIT` to the client restores the terminal and exits 128 plus the signal. A terminal that reports 0 by 0 uses `--size`. The client checks the socket is served by the same user before it sends a key. Without a terminal on stdin and stdout (pipes, a script) there is no raw mode and no resize, and the size is `--size COLSxROWS` (80x24). The exit status is the harness's code (`128 + n` for a signal), or 1 when the host goes away. Input closing doesn't end the client, which stays until the host does or the detach key is pressed.
- `--detach-key <key|none>` defaults to `[keys] detach` (`ctrl-\`). A detach key can be any ctrl or alt key or a kitty-only key, each matched in its legacy and CSI u forms. A function key can't be one: `Detachable` has no such variant, so config rejects it and the client never sees it.
- Kitty keyboard: the host answers the app's `CSI ? u` query itself, which declares support for whichever terminal attaches later. Passthrough can't re-encode keys for a terminal without kitty support, so on such a terminal modified keys degrade to their legacy encodings. This must be tested on a real phone terminal. It is unverified.

### The TUI gets every key

Phase one keeps the pty: the TUI pane runs `agent session attach --as preview --detach-key none <name>`, a transparent pipe. Every key the TUI doesn't claim reaches the agent with no TUI code change. A later, optional direct socket transport inside `Terminal` removes a process and an emulator hop and lets the TUI encode kitty keys from the host's flags. Today `encode_key` (`crates/term/src/input.rs`) only legacy-encodes.

### Queries, escape policy and clipboard

The host is the app's terminal, so it answers every query itself, attached or not. An app started detached would otherwise wait on its DA query. The queries are removed from the forwarded stream so the outer terminal never answers twice. The vim run showed what real apps send: `CSI 6 n`, `CSI >0 c`, `CSI >4;2 m`, `?1004`, `?2026`, `CSI >1 u`, title push and pop.

#### The parser

`egress::Parser` (`crates/session/src/egress/parser.rs`) is the one reader of the program's output. The queries the host answers, the allowlist and what the screen and the ledger are fed all come from what it read, so none of them can disagree about where a sequence ends.

- It survives a sequence split across any number of reads. Every item it hands on is whole: text of valid UTF-8, one allowed control, a complete CSI, escape or OSC. Whatever is forwarded leaves a terminal that reads it in its ground state.
- The OSC number decides whether a string is kept or skipped. A string being kept is bounded by `Collecting { limit }`: a CSI is 256 bytes, a title 2048, a hyperlink 4096, a colour query 32 and a clipboard write 256 KiB. One that goes over is read to its end and dropped as `Overlong`. A sequence or cancel that begins before another ends drops the first as `Aborted`. A string that is only skipped holds nothing, so it runs to its terminator however long it is.
- What is dropped is counted by reason and logged (`Refusal::name`: control character, 8-bit control, invalid UTF-8, unlisted escape, unlisted CSI, unlisted mode, malformed CSI, APC, DCS, PM, SOS, unlisted OSC, malformed OSC, hyperlink target, hyperlink parameters, clipboard read, clipboard write, overlong sequence, aborted sequence).
- Snapshots and replays go through the same filter, as a `Recipient::Watcher`, so a dump never holds what a client would not have been sent.

#### The allowlist

`Class::verdict` (`crates/session/src/egress/table.rs`) is an exhaustive match, so this table is the allowlist and nothing else.

| Class | Verdict |
|---|---|
| Text, Control, Sgr, CursorMove, Erase, EditLines, Scroll, ScrollRegion, TabStops, CursorSave, Repeat, Index, CharacterSet, KeypadMode, Reset | Forwarded to every client |
| Modes | Forwarded to every client. Only the 19 known private modes (`Mode::ALL`: `?1 ?5 ?6 ?7 ?9 ?12 ?25 ?47 ?1000 ?1002 ?1003 ?1004 ?1005 ?1006 ?1015 ?1047 ?1049 ?2004 ?2026`). An unknown number is stripped from the list, and the sequence is dropped when none is left. ANSI `h` and `l` are dropped |
| KittyKeyboard | Forwarded to every client |
| ModifyOtherKeys | Forwarded to every client: `CSI > 4 ; Pv m` and `CSI > 4 n` |
| CursorStyle | Forwarded to every client, styles 0 to 6 |
| Hyperlink | Forwarded to every client: schemes `http`, `https` and `mailto`, a URI of at most 2048 bytes, and no parameters beyond `id=` |
| Title, TitleStack | Owner only: OSC 0 to 2, and `CSI 22 t` and `CSI 23 t` |
| Clipboard | Owner only, and only with `[host] osc52 = "owner"`. Selections `c p q s 0` to `7`. A read (`?`) is always refused |
| Answered(query) | Answered by the host and not forwarded (below) |
| Everything else | Dropped: other `CSI t`, any unlisted OSC (OSC 1337 included, since wezterm's `SetUserVar` handlers run Lua keyed on the variable name), DCS, APC, PM, SOS, 8-bit controls and unlisted escapes |

No sequence in app output controls the host. The host has no private control sequences, only socket frames.

#### The answers

`crates/session/src/answer.rs`. The host claims a VT220 that draws in colour, as much as `vt100` does, and nothing it can't show.

| Query | Reply |
|---|---|
| DA1: `CSI c`, `CSI 0 c`, `ESC Z` | `CSI ? 62 ; 22 c` |
| DA2: `CSI > c`, `CSI > 0 c` | `CSI > 1 ; 10 ; 0 c` |
| XTVERSION: `CSI > q`, `CSI > 0 q` | `DCS > | agent-host(<version>) ST` |
| DSR: `CSI 5 n` | `CSI 0 n` |
| CPR: `CSI 6 n` | `CSI row ; col R` from the emulator cursor |
| DECXCPR: `CSI ? 6 n` | `CSI ? row ; col ; 1 R` |
| DECRQM: `CSI Pm $ p`, `CSI ? Pm $ p` | `CSI [?] Pm ; state $ y`. An ANSI mode is 0 (unknown). A private mode is 1 (set) or 2 (reset) from the 19 tracked modes and 0 for any other |
| `CSI 14 t` | `CSI 4 ; rows*16 ; cols*8 t`, from an assumed 8 by 16 cell |
| `CSI 18 t` | `CSI 8 ; rows ; cols t` |
| Kitty `CSI ? u` | `CSI ? {flags} u` from the tracked flags |
| OSC 10 and 11 `?` | `rgb:` replies from the owner's `Hello.colours`, ending with the terminator the query used. No reply without them, which is always today since the CLI sends none |

#### Throughput

Measured on the implementer's machine, release build, one core, 64 MB of coloured text in 4 KiB reads: the whole pipeline (parser, filter, ledger and `vt100`) runs at 61 to 67 MB/s, `vt100` alone at 145 to 202 MB/s and the parser alone at about 190 MB/s. Other machines will differ.

### Environment

The host builds the child's environment from a fixed policy. It does not inherit a snapshot from the spawner, since the spawner may be a TUI inside wezterm or an SSH session.

- Set: `TERM`, `COLORTERM`, `AGENT_SESSION=<name>` (replaces `ZELLIJ_SESSION_NAME` in `nesting_env`).
- Removed: `SSH_*`, `WEZTERM_*`, `TERM_PROGRAM` and `TERM_PROGRAM_VERSION`, `ZELLIJ*`, `KITTY_*`, `ITERM_*`, `VTE_VERSION`, `TMUX*`, `STY`.
- Kept: the rest of the user's environment as the host process saw it at start, `PATH` included. `agent exec` has no login shell logic today, so none is repeated here. Only `SSH_CONNECTION`, `SSH_CLIENT`, `SSH_TTY` and `SSH_ORIGINAL_COMMAND` are removed of the `SSH_` variables, so `SSH_ASKPASS` stays. `SSH_AUTH_SOCK` is decided once, from the spawner: outside an ssh login its own agent is kept, as the path it was given, when what it leads to is a socket we own. A link is followed to its end and only the end is judged, since wezterm's `agent.<pid>` is a link to launchd's socket. A link that dangles or leads to anything else, or to a socket of someone else's, is refused. When there is no agent or it is refused, and inside an ssh login whatever was forwarded, the first stable agent (`$XDG_RUNTIME_DIR/ssh-agent.socket`, or launchd's on macOS) that is such a socket is used, else the variable is unset. `COLUMNS`, `LINES`, `NVIM*`, `LC_TERMINAL*`, `TERM_SESSION_ID`, `WT_SESSION`, `GHOSTTY_*`, `ALACRITTY_*` and the other terminal variables are removed too. The variables Claude Code sets for its own children are the harness's list, which `agent exec` removes, so there are two lists with no overlap.

The exact lists are `ChildEnvironment` in `crates/session/src/environment.rs`, tested against an environment that has each removed variable set and against names that only look alike (`STYLE`, `TERMINFO`).

### Scrollback

The host keeps scrollback capped by bytes (default 32 MiB per host, configurable, about 1M cells), not lines. A `vt100` cell is 32 bytes, so 5000 lines at 200 columns is about 32 MB per host. `agent dump --scrollback <name>` prints it. A TUI key (`E`) and the nvim map (`<leader>ab`) open it in `$EDITOR`, replacing `Ctrl+s e` (`lua/agent/keymaps.lua`, wezterm `key_bindings.lua`). The TUI key is added to `BINDINGS` in the same change.

## Lifecycle

- **Start:** `create` writes the record, spawns the host and waits (bounded) for the socket. `LaunchMark` handling in `ops` is unchanged. The host tells its spawner `R` on its standard output once the socket is up and the program runs, or `E` and the reason when it can't, so readiness is the host's word. A record is made only by a start that brings a command, and a failed start removes the record it made and the socket it bound. In P2a `agent session host --spawn -- <program> <args>` records the command and starts the host, and without a command it starts the recorded one. A session whose lock is held is refused (`already runs`). The host binds its socket before it starts the program, so a program that can't start leaves an error in the log and a host that exits.
- **Harness exits:** the host waits up to 500 ms for the last output (a descendant that holds the pty open would otherwise keep it waiting), sends `Exited(status)`, then has the accept thread unlink the socket and release the lock, and stays up for 2 s for the clients it still has before it exits. The session lists as `Exited` and resurrects from its record through `agent exec`.
- **Kill:** `Kill` sends `SIGHUP` to the harness's process group and `SIGKILL` 2 s later if it is still there, then follows the exit path. The group id is only signalled while the harness hasn't been reported exited.
- **Host crash:** the pty master closes and the child gets SIGHUP, as when a zellij server dies today. The conversation is on disk. No supervisor is added, since a launchd or systemd unit would make the tool platform-specific.
- **Reboot:** the runtime dir is gone and records remain, so every agent lists `Exited` and `⏎` resumes it.
- **Logout on Linux:** `setsid` doesn't leave the systemd login scope. With `KillUserProcesses=yes` an ssh logout kills hosts, as it kills zellij servers today. `loginctl enable-linger` is the user's workaround and is documented, not automated.
- **Upgrade:** `install.sh` never touches running hosts, which keep their old binary image. This relies on cargo replacing the binary by rename. It is verified on Linux by reasoning only and must be checked on macOS, where overwriting a running signed binary in place gets it killed. Clients accept a range of host versions. An unsupported host is reported, never killed, and `agent restart` is the upgrade path.
- **Resources:** one process and about four threads per session, idle on blocking reads. Memory is the emulator grid plus the scrollback cap. Ten hosts are not yet measured.

## Migration

- **Router (built in P3):** `Backend.host` becomes a `Hosts` adapter in the `agent-native` crate (with `Native`), implementing `SessionHost` and `ClientControl` over `Zellij` and `Native`. `sessions` and `liveness` merge both listings. Per-session calls go to the owner of the name, decided by liveness first (a live zellij session, or a held lock on a native record), then by the record. This covers someone running `zellij attach name` by hand while a native record exists. A name live in both is a discovery warning. New sessions go to `[host] kind = "native" | "zellij"`. Core stays host-agnostic. `name_limit` is the limit of the host new sessions go to (decided in P3, see below).

P3 decisions:

- Ownership. Zellij live, else native live, else the only host with the name, else `[host] kind`. A host that can't answer in 2 s is an error for every call that needs to know the owner (`create`, `kill`, `delete`, `start_detached`, `write`, `screen`, `observe`), never read as not having the name. Only `attach`, which returns a command and no result, falls back to `[host] kind`. With no native record of a name and `kind = "zellij"` zellij is asked nothing, and with no native state at all the router is a pass-through: `listing`, `sessions` and `liveness` are zellij's own call with its own timeout and error. A name live in both lists once, as zellij's, with `Warning::Doubled`. Two dead twins resurrect under `[host] kind`.
- Name limit. It is the default host's: 20 for zellij, and for native the smaller of 48 and what the runtime directory's socket path leaves (`RuntimeDir::name_budget`, 103 bytes on macOS, 107 on Linux, less the directory, `/` and `.sock`). It is not computed from which sessions exist, since that needs a zellij listing on every call. Every host lists every session it has, whatever its length, so a native session over 20 is listed under `kind = "zellij"` and `core` calls it unhostable: attachable and removable, not startable. With `kind = "native"` and a runtime directory that leaves under 20, a zellij session longer than that limit is unhostable in the same way.
- Restart. `r` and `R` kill through every host that runs the name (both, for a name live in both, whose `Warning::Doubled` says to restart it) and `create` under `[host] kind`, which deletes the other host's dead twin first (and refuses when the other host's session is live). A resurrect on `⏎` of an exited session stays with its owner, because attaching carries no pane to create from.
- Native `create` starts the host and waits for it (at most 10 s, in the `host --spawn` intermediate), then returns the attach command, so a caller that is told to start a session detached finds it running. `start_detached` starts from the record, and a name with no record is an error.
- Native `attach` is `agent session attach <name> --start`, which starts a session from its record when no host runs it, so `⏎` on an exited native session resurrects it. `observe` is `--as peek --no-detach-key --size COLSxROWS` at the host's own size, a read-only preview that is not a window.
- Windows. Clients of `Window` purpose that are not the caller's, not under it and not under any TUI. A `--as watch` client has `Watch` purpose and is no window. A `Window` carries a `WindowHandle` the adapter that listed it built (zellij's client pid, native's client number in the host that listed it). Detaching goes to that host and client only, so a preview of the same process is not closed, a pid reused by another process is not signalled, and a window of a host that has gone is already closed. A look asks every live host at once, each bounded by 2 s, and the zellij and native adapters share one scan of the process table.
- `kill` sends `Kill` and waits up to 6 s for the lock to be free. `delete` kills, then removes the record and the log while holding the record's lock, so a host that starts meanwhile is refused instead of left without a record. `write` waits for the host's answer to what it typed (a `Bye` after it), so a refusal is reported. `write` and `screen` each open one control connection with a 5 s read bound. Taking the record's lock waits out a probe for 250 ms before reporting another host. All run on worker threads, none on the UI's.
- The caller's own session is named by each host (`SessionHost::own_session`: zellij reads `ZELLIJ_SESSION_NAME`, native `AGENT_SESSION`) and the router asks both, so the TUI, `agent restart` and `agent rm` know their own native session too.
- P4 decisions:
  - `SessionHost::attach` and `create` take an `Attachment` (`Window` or `Preview`), not a string, carried by `Backend`/`Services` so every attach the process builds is for the same use. The TUI's pane is `--as preview --detach-key none` (D6), every other `agent` command attaches a `Window`, the default `--as window`, which takes over (D1). The TUI is told from the others by the one `runs_tui` rule over the command line. `observe` stays `--as peek`, `write`, `screen` and `windows` are `Control` connections.
  - `screen` takes an `Extent` and a `Rendering`. `Scrollback` is `Snapshot { scrollback: Full }` (D7), and zellij's `dump-screen --full`. `Plain` is `Snapshot { style: Plain }`, rendered by the host from the screen's cells so the gaps a program drew with tabs and cursor moves stay as spaces, and zellij's dump without `--ansi`. `agent dump <name> [--scrollback] [--plain]`. The TUI's `E` is a worker job (`Job::ReadScrollback`) that replies with the text, and the editor then opens it as a `.txt` file at its last line. A newer `E` replaces an older one still reading.
  - `[keys] detach` is `Config::detach` (`Detach::{Off, On(Detachable)}`, default `ctrl-\`, `none`), kept out of `Keys` and `KeyField` because the TUI never uses it. `Native` passes it as `--detach-key <key|none>` on a window attach and the client uses it only when stdin and stdout are a tty (`detach_for`).
  - A `--as watch` client has `Purpose::Watch`, a protocol value of its own, so it is not a window: the TUI doesn't pause for it, `r` doesn't ask about it and moving here doesn't disconnect it. A demoted window keeps `Purpose::Window` and is one. `windows_of` still walks the process lineage of the `Window`-purpose clients, to leave out the caller's own and any under a TUI.
  - nvim: the plugin's `agent.host` calls the hidden `agent plugin live [--cwd]|argv|choice`, `agent prompt` for messages (so it is refused while the agent asks or boots, and says why) and `agent dump --scrollback --plain`, which route to the owner, so one code path serves both hosts. `live` without `--cwd` reads only liveness, so a send doesn't list every session's panes. `choice` types a digit from 1 to 9 and enter, and is the one way in while the agent asks, since a menu is answered by typing. `lua/agent/zellij.lua` is gone. `<leader>ab` opens the scrollback in a scratch tab. `<C-S-x>` in a terminal buffer is an alias that only a kitty-protocol terminal can send.
  - Wezterm: the Ctrl+Shift+X handler that sent zellij's `Ctrl+s e` is removed from `wezterm/key_bindings.lua` with the default flip. Scrollback in nvim is `<leader>ab`.
  - A program in a native session never gets `ZELLIJ_*` (denied in its environment). A TUI run in one with a private `ZELLIJ_SOCKET_DIR` therefore lists the default zellij server's sessions, so scratch runs pass it through `env ZELLIJ_SOCKET_DIR=...`. The TUI also removes `AGENT_SESSION` from the environment of its panes (`Native::nesting_env`, joined with zellij's by the router), so a zellij server it resurrects doesn't hand a stale session name to every pane.
  - Verified by hand, with a fake harness (`cat -v`) in a scratch git repo and `[host] kind = "native"`, no zellij involved: `⏎` enters and typed text echoes, the nav key leaves, a hop chord switches agents, `s` types into an agent without attaching, `v` peeks, a second `agent session attach --as window` shows `⇄` and `⏎ move here` and moving here closes it, `r` restarts, `D` removes, `E` opens 161 lines of scrollback in `$EDITOR`, the TUI inside a native session refuses to kill or remove its own session, and nvim's `agent.host` lists, types, attaches and reads scrollback.
- **Cutover:** live zellij agents keep running until restarted. `r` restarts one under the configured host. `R` migrates every idle agent. An exited zellij session resurrects on `⏎` under zellij, which owns it. A restart (`r`, `R`) is what moves it, under the default host, deleting its stale layout and cache entry.
- **Default flipped:** `[host] kind` defaults to `native`. New sessions are native, live zellij sessions run on until `r` or `R`, and the zellij crate goes once none remain.
- **Rollback:** `[host] kind = "zellij"`, then `r` on each agent that moved.
- **Removal:** when no zellij session or layout exists, delete `crates/zellij`, the router's zellij arm, `known.rs`'s zellij claims, the `ZELLIJ_*` handling in `main.rs`, `lua/agent/zellij.lua` and the zellij test-environment section of `AGENTS.md`. Update `docs/design.md` (Open elsewhere, Places, Observer) and the user docs.

## Security

- Runtime dir 0700, ownership and no-symlink checked, socket 0600. A host binds a socket path only if nothing answers there: a live socket is another host's whatever state directory it keeps its lock in (`HostError::Occupied`), a dead one is replaced, and the host re-binds only when its file is gone and never removes a file it didn't bind. A lock file beside the socket, `<name>.lock` in the runtime directory, is held for the host's life, so two hosts starting together on a dead socket can't both remove it and bind: the second finds the lock held. It is left in place when the host ends. Every connection's peer uid is checked before anything else is read.
- A hostile same-uid client can already ptrace the agent, so the threat is robustness. The frame cap, `Hello` timeout, closing on an unknown tag, per-client queue caps and parsed `SessionName`s (no client string reaches a shell or a path) cover it.
- App output can't command the host, and query replies are generated, never echoed from client input. The escape allowlist above is the boundary between app output and the user's terminal.

## Portability

Linux and macOS use `std::os::unix`, `portable-pty` and nix. The BSDs should work through `getpeereid` and are untested. The thin client is plain raw-tty passthrough, so no terminal brand is assumed, and `Place` names terminals for display only.

## Phases

Each phase ships alone, passes the definition of done and gets the three-reviewer pass. Tests use tempdirs and a private runtime dir, never live zellij. Before each default flip, a verification step runs a live scratch end-to-end on a real terminal (spawn, attach, detach, reattach, resize, kill, resume) in an isolated state dir.

- **P0 Extract.** Move the process scan and `Place` walk out of `agent-zellij` into a shared crate. No behaviour change. Its tests move with it.
- **P1 Protocol (done).** `agent-host`: frames, codec, limits. Tests: round trips, truncated, oversized and unknown frames.
- **P1.5 Emulator bake-off.** Run `vt100` plus wrapper, `alacritty_terminal` and `libghostty-vt` over the recorded captures with an independent oracle. Decides D5.
- **P2a Host and raw client (done).** `agent session host` and `agent session attach`, locks, spawn without zombies, replay with the fixes above, in `agent-session`. Tests run real hosts in temporary directories with a private runtime directory: a scripted program whose replay matches a live client's screen cell for cell, a client attaching mid-stream and during an alternate-screen program, take, polite, preview and claim, a watcher that can't resize or type, `Inject` and `Snapshot` without attaching, `Kill` with and without `SIGHUP` obeyed, harness exit and late snapshots, a stale lock and a new host after `SIGKILL`, socket re-creation, hello timeout, malformed hello and command, version refusal, environment policy, a stalled client, held and released updates, runtime directory and socket path validation, the detach key in both encodings, the outer terminal restored after detach, exit, host loss and panic, and a client under a real pty that follows a resize.
- **P2b Queries and escape filter (done).** Query answering, the allowlist, OSC 52 policy. Tests: a table of sequences with the expected forwarded bytes, sequences split across reads, OSC 1337 never forwarded.
- **P2c Backpressure and Resync.** Tests: a client that stops reading gets `Resync` and the pty reader never blocks.
- **P3 `Native` adapter, router, `[host] kind` (done).** `Native` and `Hosts` in the `agent-native` crate, `Control` in `agent-session`, `agent-places` shared with zellij. Tests: the router over two `FakeHost`s (ownership by liveness then record, a name live in both, merged listing and liveness, name limit, per-call routing, creating over the other host's twin, restart migrating, merged windows and detach routing), `Native` against real hosts in a tempdir (create, list, typing a frame and more, screen, windows and detach, kill, exited resurrect through the record and through `attach --start`, stale record, delete, peek spec, name limit), and `[host] kind` parsing. With `kind = "zellij"` and no native state nothing changes: the router makes the calls zellij got before, with the same timeouts and errors (a test counts them). Once a native record exists, a routed call also reads native's records (a directory listing) and, for a name native has a record of, zellij's liveness.
- **P4 Clients (done).** TUI company and peek through host client lists, nvim on `agent attach`, `agent prompt` and `agent ls`, the scrollback key and map, docs. This phase must finish before native becomes the default. The nvim plugin is checked by hand in an isolated `XDG_STATE_HOME`.
- **P5 Direct socket in the TUI (optional).** A socket transport in `Terminal` and kitty-aware key encoding. Off the critical path.
- **P6 Default native, later delete zellij.** After the verification step.

## Risks and what's unverified

Verified by the prototype only: replay on `vt100` (alternate screen prefix, scroll region gap). Raw captures (`vim.raw`, `inkish.raw`, `region.raw`) are kept in the scratch directory for the bake-off.

Unverified:

- Claude Code under the replay and the optional resize nudge, including whether it uses the alternate screen.
- The raw client in real terminals other than the pty the tests use (wezterm, ssh, a phone), and what a `SIGTERM` or `SIGHUP` does to its terminal.
- A host started at login or after sleep, and a log file nobody reads.
- A real socket broker end to end, flicker on replay, resize races, memory and CPU for ten hosts, Linux.
- Kitty keyboard degradation on a terminal without support (a phone over SSH).
- cargo's rename-on-install on macOS for a running signed binary.
- Claims marked "from memory, unchecked": macOS `/tmp` is cleaned at boot, `portable-pty` supports ConPTY, mosh-style state sync as the model for `Resync`.

Risks:

- Emulator fidelity: anything the wrapper misses shows as a corrupt redraw until the next full repaint.
- Backpressure bugs show as silent stalls, so `Resync` needs a test with a client that stops reading.
- Losing zellij scroll mode if the scrollback key isn't shipped with P4.

## Open decisions

- **D5 Emulator.** Evaluate `vt100` with a tracked-state wrapper, `alacritty_terminal` and `libghostty-vt` in P1.5. `libghostty-vt` needs `zig` at build time through its Rust binding (third-party, pre-1.0 API at 0.2.2, from the proposal's reading of `build.rs`), which isn't installed here and is the owner's to install. Criteria, in order:
  1. Replay equals the live screen on every capture under an independent oracle, including alternate screen, scroll region, saved cursor and the modes above.
  2. Answers queries (or exposes them) without hand-written parsing.
  3. A `cargo install` that needs no network clone or extra toolchain, or a documented prebuilt path.
  4. Contains `unsafe` inside a dependency, so our crates keep `forbid(unsafe_code)`.
  5. Memory per cell and speed on a flood of output.

  The TUI pane stays on `vt100` regardless, since its renderer is `tui-term`.
