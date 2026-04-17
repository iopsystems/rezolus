# rezolus-webview

A Rust library compiled to WebAssembly that powers the static Rezolus viewer at
[site/viewer/](../../site/viewer/). Provides a PromQL query engine over Rezolus
parquet recordings that runs entirely in the browser.

## Prerequisites

- [Rust](https://rustup.rs/) (edition 2024)
- [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/)
- LLVM/Clang (for compiling zstd to wasm32; on macOS: `brew install llvm`)

## Building

```bash
./build.sh
```

This compiles the crate to WebAssembly and writes the package into
`site/viewer/pkg/` where the frontend loads it from.
