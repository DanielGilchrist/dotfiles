#!/usr/bin/env fish
# Build all zellij plugins in this workspace and copy artifacts to dist/.
# Usage: ./build.fish [plugin-name]   (defaults to building everything)

set -l here (status dirname)
cd $here

set -l target wasm32-wasip1
set -l mode release

set -l args
if set -q argv[1]
    set args -p $argv[1]
end

cargo build --$mode --target $target $args
or exit 1

mkdir -p dist
for wasm in target/$target/$mode/*.wasm
    cp $wasm dist/
    echo "→ dist/"(basename $wasm)
end
