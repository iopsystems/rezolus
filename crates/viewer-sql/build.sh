#!/bin/sh
# Build the SQL-backed WASM viewer alongside the legacy `viewer/` artifact.
# Output goes to site/viewer-sql/pkg/ (parallel to site/viewer/pkg/).
#
# This crate compiles for wasm32-unknown-unknown using the workspace-level
# Rust toolchain. Profile inherits from the workspace `wasm-release` profile.
set -e
cd "$(dirname "$0")/../.."
wasm-pack build crates/viewer-sql \
    --target web \
    --out-dir ../../site/viewer-sql/pkg \
    --out-name wasm_viewer_sql \
    --profile wasm-release "$@"
