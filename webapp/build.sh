#!/bin/sh
# Build the WebAssembly engine into webapp/pkg (needed before serving the
# app locally; GitHub Actions runs this before publishing).
#   needs: rustup target add wasm32-unknown-unknown; cargo install wasm-pack
set -e
HERE=$(cd "$(dirname "$0")/.." && pwd)
wasm-pack build "$HERE/crates/canlog-wasm" --release --target web --no-typescript --no-pack \
	--out-dir "$HERE/webapp/pkg" --out-name canlog
rm -f "$HERE/webapp/pkg/.gitignore"
ls -l "$HERE/webapp/pkg"
