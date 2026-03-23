# Replace ringlog with tracing (stderr-only)

## Goal

Swap the `ringlog` logging backend for the `tracing` ecosystem while preserving
identical runtime behavior: all output to stderr, same log levels, same
config/CLI interfaces.

## Motivation

Align with the tracing ecosystem (already done in pelikan). tracing gives us
non-blocking I/O out of the box, structured logging capabilities for future use,
and a path to file rotation via logroller in a follow-up milestone.

## Scope

Milestone 1 only: stderr output, no file rotation, no config schema changes.

## Dependency changes

Remove:
- `ringlog = "0.8.0"`

Add:
- `tracing = "0.1"`
- `tracing-subscriber = { version = "0.3", features = ["fmt"] }`
- `tracing-appender = "0.2"`
- `tracing-log = "0.2"`

## Tasks

### 1. Update Cargo.toml dependencies

Remove ringlog, add the four tracing crates listed above.

### 2. Replace `use ringlog::*` in src/main.rs

Replace with explicit imports from tracing:

```rust
use tracing::{debug, error, info, trace, warn};
use tracing::Level;
```

### 3. Create shared logging types in src/common/

Define a `LogDrain` struct that holds `tracing_appender::non_blocking::WorkerGuard`.
This replaces the `mut log` handle + flush loop pattern.

Define a `configure_logging(level: tracing::Level) -> LogDrain` function that:
- Creates a `tracing_appender::non_blocking(std::io::stderr())` writer
- Builds a `tracing_subscriber::fmt` subscriber with the writer and level filter
- Calls `tracing_log::LogTracer::init()` for any transitive `log` crate users
- Returns the `LogDrain` (must be held alive for process lifetime)

### 4. Rewire the 6 mode entry points

Each mode (agent, exporter, recorder, viewer, hindsight, mcp) currently has:

```rust
let debug_output: Box<dyn Output> = Box::new(Stderr::new());
let level = ...;  // from config or CLI flags
let debug_log = if level <= Level::Info {
    LogBuilder::new().format(ringlog::default_format)
} else {
    LogBuilder::new()
}
.output(debug_output)
.build()
.expect("failed to initialize debug log");

let mut log = MultiLogBuilder::new()
    .level_filter(level.to_level_filter())
    .default(debug_log)
    .build()
    .start();

// flush loop
rt.spawn(async move {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let _ = log.flush();
    }
});
```

Replace each with:

```rust
let level = ...;  // unchanged — same config/CLI logic
let _log_drain = configure_logging(level);
// no flush loop needed — tracing_appender::non_blocking handles it
```

The `_log_drain` must live for the duration of the mode's `run()` function.

Level mapping (for the 3 config-driven modes):
- `ringlog::Level::Error` -> `tracing::Level::ERROR`
- `ringlog::Level::Warn`  -> `tracing::Level::WARN`
- `ringlog::Level::Info`  -> `tracing::Level::INFO`
- `ringlog::Level::Debug` -> `tracing::Level::DEBUG`
- `ringlog::Level::Trace` -> `tracing::Level::TRACE`

### 5. Update config/log.rs modules

Three files define a serde remote derive on `ringlog::Level`:
- `src/agent/config/log.rs`
- `src/exporter/config/log.rs`
- `src/hindsight/config/log.rs`

Replace `ringlog::Level` with a local `Level` enum that deserializes the same
TOML values (error/warn/info/debug/trace) and provides a method to convert to
`tracing::Level`.

### 6. Update log macro call sites (if needed)

The ~223 call sites using `debug!()`, `info!()`, `warn!()`, `error!()`,
`trace!()` should work unchanged since tracing's macros have compatible
signatures. Verify with `cargo build` — fix any signature mismatches.

### 7. Verify

- `cargo build` succeeds on macOS
- `cargo test` passes
- `cargo clippy` clean
- `cargo xtask fmt` clean
- Manual smoke test: run agent mode, confirm log output on stderr

## Files touched

- `Cargo.toml` — dependency swap
- `src/main.rs` — replace `use ringlog::*`
- `src/common/mod.rs` (or new `src/common/logging.rs`) — LogDrain + configure_logging
- `src/agent/mod.rs` — rewire init
- `src/exporter/mod.rs` — rewire init
- `src/recorder/mod.rs` — rewire init
- `src/viewer/mod.rs` — rewire init
- `src/hindsight/mod.rs` — rewire init
- `src/mcp/mod.rs` — rewire init
- `src/agent/config/log.rs` — replace ringlog::Level
- `src/exporter/config/log.rs` — replace ringlog::Level
- `src/hindsight/config/log.rs` — replace ringlog::Level

## Future work (milestone 2)

- Add `logroller` for file-based log rotation with gzip compression
- Add config fields: `log_file`, `log_rotation_interval`, `log_max_keep_files`
