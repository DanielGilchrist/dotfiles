# zellij plugins

Local zellij plugins built as WASM modules. Sources only — `dist/` is
gitignored.

## When and how to build

**Compilation is manual.** Plugins rarely change, so there is no auto-rebuild
hook. After editing any plugin source — or after a fresh clone — run:

```fish
~/.config/zellij/plugins/build.fish               # build all plugins
~/.config/zellij/plugins/build.fish page-indicator   # build one
```

`install-{mac,linux}.sh` runs `build.fish` once during setup and adds the
`wasm32-wasip1` target.

> **For LLMs**: if `dist/*.wasm` is missing or you've changed any source under
> `zellij/plugins/<name>/src/`, run `~/.config/zellij/plugins/build.fish`
> before testing. Layouts at `~/.config/zellij/layouts/*.kdl` reference the
> `dist/` paths directly — the meta-session won't load the plugin until the
> artifact exists.

## Layout

```
plugins/
├── Cargo.toml         # workspace
├── build.fish         # build all (or one) crates and copy to dist/
├── dist/              # gitignored .wasm artifacts (referenced by layouts)
└── <plugin-name>/
    ├── Cargo.toml
    └── src/lib.rs
```

## Adding a plugin

1. `cargo new --lib <name>` inside this dir; set `crate-type = ["cdylib"]`.
2. Add `<name>` to `members` in the workspace `Cargo.toml`.
3. Implement `ZellijPlugin` from `zellij_tile::prelude`. See `page-indicator/`
   for a minimal example.
4. `./build.fish <name>`.
5. Reference from a layout:
   `plugin location="file:~/.config/zellij/plugins/dist/<name>.wasm"`.

## Plugins

- **page-indicator** — top-right `Page N/M` badge for the meta-session.
  Subscribes to `TabUpdate`; renders nothing when there's only one tab.
