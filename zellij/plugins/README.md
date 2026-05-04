# zellij plugins

Local zellij plugins built as WASM modules. Sources only — `dist/` is
gitignored.

## When and how to build

**Compilation is manual.** Plugins rarely change, so there is no auto-rebuild
hook. After editing any plugin source — or after a fresh clone — run:

```fish
~/.config/zellij/plugins/build.fish              # build all plugins
~/.config/zellij/plugins/build.fish agents-bar   # build one
```

`install-{mac,linux}.sh` runs `build.fish` once during setup and adds the
`wasm32-wasip1` target.

To hot-reload a plugin in a running agents meta-session (preserves Claude
state in per-agent panes): `agent-reload-plugin [<name>]` — defaults to
`agents-bar`. Internally calls `zellij action start-or-reload-plugin`.

> **For LLMs**: if `dist/*.wasm` is missing or you've changed any source under
> `zellij/plugins/<name>/src/`, run `~/.config/zellij/plugins/build.fish`
> before testing. The meta-session layout references the `dist/` paths
> directly — it won't load the plugin until the artifact exists.

## Layout

```
plugins/
├── Cargo.toml         # workspace
├── build.fish         # build all (or one) crates and copy to dist/
├── dist/              # gitignored .wasm artifacts (referenced by layouts)
└── <plugin-name>/
    ├── Cargo.toml
    └── src/
        ├── main.rs    # ZellijPlugin impl + entry
        └── ...        # additional modules as needed
```

Plugins are **binary** crates (zellij invokes the wasi `_start` entry); no
`crate-type = ["cdylib"]`, no `lib.rs`.

## Adding a plugin

1. `cargo new --bin <name>` inside this dir.
2. Add `<name>` to `members` in the workspace `Cargo.toml`.
3. Add `zellij-tile.workspace = true` to the new crate's `Cargo.toml`.
4. In `src/main.rs`, implement `ZellijPlugin` from `zellij_tile::prelude` and
   wrap the type with `register_plugin!(MyType);`. See `agents-bar/` for an
   example with section modules.
5. `./build.fish <name>`.
6. Reference from a layout:
   `plugin location="file:$HOME/.config/zellij/plugins/dist/<name>.wasm"`
   (use `$HOME` not `~`; tilde expansion is unreliable in zellij plugin URLs).

## Plugins

- **agents-bar** — top-of-tab chrome for the agents meta-session. Currently
  hosts a `Page N/M` indicator (right-aligned, themed in the active-pane
  accent colour). Adding new chrome features = a new section module under
  `agents-bar/src/sections/` plus an entry in `LEFT` or `RIGHT`.
