//! Metadata keys a reader of a recording looks up: in a parquet file's
//! footer, a `.rez` recording's manifest, or a dendro source's metadata.
//!
//! These are on-disk names, so they change rarely and only in lockstep with
//! every reader. The binary's `src/parquet_metadata.rs` re-exports them
//! beside the keys only rezolus writes (`producer_epoch`, `selection`, the
//! 5.x parquet writer's `node`/`instance`), and the `recording` tools, the
//! viewers and `report-save` name them from here.

// ── Top-level keys ──────────────────────────────────────────────────────

/// Identifies the recording source(s).
/// Single file: `"llm-perf"`.  Combined: `["rezolus", "llm-perf"]`.
pub const KEY_SOURCE: &str = "source";

/// Agent/tool version string (single-source files only; for combined files
/// the per-source version lives under `per_source_metadata.<source>.version`).
pub const KEY_VERSION: &str = "version";

/// Sampling interval in milliseconds, e.g. `"1000"`. Must be identical
/// across files before they can be combined.
pub const KEY_SAMPLING_INTERVAL_MS: &str = "sampling_interval_ms";

/// JSON-serialised hardware summary (from the Rezolus agent `/systeminfo`
/// endpoint). Display-only — used by the viewer and MCP, and stored by
/// `recording annotate --systeminfo`.
pub const KEY_SYSTEMINFO: &str = "systeminfo";

/// JSON map of metric name → help text. Used by MCP `describe-metrics`.
pub const KEY_DESCRIPTIONS: &str = "descriptions";

/// Service KPI query definitions (ServiceExtension JSON). Top-level key in a
/// single-source parquet (`recording annotate`, a producer's own parquet), a
/// `.rez` recording's or a dendro source's metadata. When files are combined,
/// each input's is moved under
/// `per_source_metadata.<source>.<entry>.service_queries`.
pub const KEY_SERVICE_QUERIES: &str = "service_queries";

/// The section template key; see [`crate::source_templates`].
pub const KEY_DASHBOARD: &str = "dashboard";

/// Per-source metadata map (used in combined files). Value is a JSON
/// object keyed by source name, then by entry: the node name for a rezolus
/// source, the instance id (`"0"` by default) for any other. A source's
/// `descriptions` sit beside its entries:
///
/// ```json
/// {
///   "llm-perf": {
///     "0": { "version": "0.1.0", "role": "loadgen", "service_queries": { ... } },
///     "descriptions": { ... }
///   },
///   "rezolus":  { "web01": { "version": "5.8.3", "node": "web01" } }
/// }
/// ```
pub const KEY_PER_SOURCE_METADATA: &str = "per_source_metadata";

/// JSON payload of one-off events attached to the recording (restarts,
/// config changes, anomalies, ...). Value is an object `{"events": [...]}`
/// where each entry conforms to [`crate::events::Event`]. Each event
/// carries its own optional `source` / `node` / `instance` scope rather
/// than inheriting from file-level metadata, so the payload stays
/// self-describing across combine.
pub const KEY_EVENTS: &str = "events";

// ── Keys nested under `per_source_metadata.<source>.<entry>` ─────────

/// Per-source version string.
pub const NESTED_VERSION: &str = "version";

/// Service KPI query definitions (ServiceExtension JSON).
pub const NESTED_SERVICE_QUERIES: &str = "service_queries";

/// Per-source metric descriptions (metric name → help text). Nested under
/// `per_source_metadata.<source>` in combined files; single-source files use
/// the top-level `descriptions` key instead.
pub const NESTED_DESCRIPTIONS: &str = "descriptions";
