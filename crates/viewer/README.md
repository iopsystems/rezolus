# viewer

The Rust/WebAssembly companion to the browser-only Rezolus viewer at
[site/viewer/](../../site/viewer/). It exposes a `Viewer` class to JavaScript
that loads a Rezolus parquet recording from a `Uint8Array`, unpacks its
metadata, and runs PromQL range/instant queries against it — entirely in the
browser, with no server round-trip.

This crate is a member of the main rezolus Cargo workspace but targets
`wasm32-unknown-unknown`. Its dependencies (including `metriken-query`, which
provides the TSDB and PromQL engine) come from the workspace so the browser
and the native `rezolus view` binary stay in sync.

## Prerequisites

- [Rust](https://rustup.rs/)
- [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/) 0.13 or newer
  (required for the `--profile` flag)
- LLVM/Clang — needed to compile zstd to wasm32; on macOS use
  `brew install llvm`

## Building

```bash
./build.sh
```

The script invokes `wasm-pack build --profile wasm-release` and writes
`wasm_viewer.js`, `wasm_viewer_bg.wasm`, and the accompanying `.d.ts` files
into `site/viewer/pkg/`, where `site/viewer/lib/script.js` imports them as
`../pkg/wasm_viewer.js`.

The `wasm-release` profile is defined in the root `Cargo.toml`. It inherits
from `release` but strips debuginfo and sets `opt-level = "s"` so the shipped
`.wasm` stays compact.
