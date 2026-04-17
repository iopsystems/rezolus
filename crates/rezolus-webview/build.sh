#!/bin/bash
# Build the WASM viewer and place the output in site/viewer/pkg/ so it can be
# loaded by the static site frontend (imports `../pkg/wasm_viewer.js`).
#
# Requires `wasm-pack` and (on macOS) Homebrew LLVM for compiling zstd to
# wasm32.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="$SCRIPT_DIR/../../site/viewer/pkg"

# LLVM from Homebrew is needed for compiling zstd C code to wasm32 on macOS
if [ -d /opt/homebrew/opt/llvm ]; then
    export CC=/opt/homebrew/opt/llvm/bin/clang
    export AR=/opt/homebrew/opt/llvm/bin/llvm-ar
fi

cd "$SCRIPT_DIR"
wasm-pack build \
    --target web \
    --out-dir "$OUT_DIR" \
    --out-name wasm_viewer \
    --release \
    "$@"
