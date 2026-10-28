#!/usr/bin/env sh
# Builds `agent` and installs it to ~/.local/bin, replacing any previous
# build. Sessions record this path in their zellij layouts (`agent exec`),
# so it must stay stable: never point them at a target/ build.
set -eu

cd "$(dirname "$0")"
root="$HOME/.local"
cargo install --locked --path crates/agent --root "$root" --force

installed="$root/bin/agent"
echo "installed $("$installed" --version 2>/dev/null || echo agent) to $installed"
if command -v fish >/dev/null 2>&1 && fish -c 'functions -q agent'; then
  echo "note: a fish function named 'agent' still shadows the binary in fish"
fi
