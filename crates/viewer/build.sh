#!/bin/bash
# Build the WASM viewer and place the output in site/viewer/pkg/ so it can be
# loaded by the static site frontend (imports `../pkg/wasm_viewer.js`).
#
# Requires `wasm-pack` (>= 0.13 for --profile support) and (on macOS) Homebrew
# LLVM for compiling zstd to wasm32.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="$SCRIPT_DIR/../../site/viewer/pkg"

# On macOS, Apple's system clang can't target wasm32 — use Homebrew's LLVM
# for compiling zstd to wasm32. Resolve the prefix via brew so this works on
# both Apple Silicon (/opt/homebrew) and Intel (/usr/local).
if command -v brew >/dev/null 2>&1 && LLVM_PREFIX=$(brew --prefix llvm 2>/dev/null); then
    export CC="$LLVM_PREFIX/bin/clang"
    export AR="$LLVM_PREFIX/bin/llvm-ar"
fi

cd "$SCRIPT_DIR"
wasm-pack build \
    --target web \
    --out-dir "$OUT_DIR" \
    --out-name wasm_viewer \
    --profile wasm-release \
    "$@"
