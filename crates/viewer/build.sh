#!/bin/bash
# Build the WASM viewer and place the output in site/viewer/pkg/ so it can be
# loaded by the static site frontend (imports `../pkg/wasm_viewer.js`).
#
# Requires `wasm-pack` (>= 0.13 for --profile support) and (on macOS) Homebrew
# LLVM for compiling zstd to wasm32.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="$SCRIPT_DIR/../../site/viewer/pkg"
TEMPLATES_DIR="$SCRIPT_DIR/../../config/templates"
SITE_TEMPLATES_DIR="$SCRIPT_DIR/../../site/viewer/templates"
MANIFEST_PATH="$SITE_TEMPLATES_DIR/manifest.json"

# Ensure each config/templates/*.json has a matching symlink under
# site/viewer/templates/ so the pages-deploy `cp -rL` step picks it up.
# Without this, adding a template to config/templates/ silently 404s in
# the deployed viewer (the manifest below lists it but the file is gone).
for src in "$TEMPLATES_DIR"/*.json; do
    name=$(basename "$src")
    link="$SITE_TEMPLATES_DIR/$name"
    if [ ! -L "$link" ] && [ ! -e "$link" ]; then
        ln -s "../../../config/templates/$name" "$link"
    fi
done

# Regenerate the static-site templates manifest from config/templates/*.json
# so the WASM viewer picks up new templates without manual JS edits.
(
    cd "$TEMPLATES_DIR"
    printf '[\n'
    ls -1 *.json | sort | sed 's/\.json$//' \
        | awk 'BEGIN{first=1} {if(!first)printf ",\n"; printf "  \"%s\"", $0; first=0} END{printf "\n"}'
    printf ']\n'
) > "$MANIFEST_PATH"

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
