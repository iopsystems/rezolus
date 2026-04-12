#![allow(dead_code)]

/// Top-level parquet footer keys. These are read by `Tsdb::load` or shared
/// infrastructure and must remain flat strings in both single-source and
/// combined files.
/// Identifies the recording source(s).
/// Single file: `"llm-perf"`.  Combined: `["rezolus", "llm-perf"]`.
pub const KEY_SOURCE: &str = "source";

/// Agent/tool version string (single-source files only; for combined files
/// the per-source version lives under `metadata.<source>.version`).
pub const KEY_VERSION: &str = "version";

/// Sampling interval in milliseconds, e.g. `"1000"`. Must be identical
/// across files before they can be combined.
pub const KEY_SAMPLING_INTERVAL_MS: &str = "sampling_interval_ms";

/// JSON-serialised hardware summary (from the Rezolus agent `/systeminfo`
/// endpoint). Display-only — used by the viewer and MCP.
pub const KEY_SYSTEMINFO: &str = "systeminfo";

/// JSON map of metric name → help text. Used by MCP `describe-metrics`.
pub const KEY_DESCRIPTIONS: &str = "descriptions";

/// JSON-serialised user selection/filter state saved by the viewer.
pub const KEY_SELECTION: &str = "selection";

/// Per-source metadata map. Value is a JSON object keyed by source name:
///
/// ```json
/// {
///   "llm-perf": { "version": "0.1.0", "role": "loadgen", "service_queries": { ... } },
///   "rezolus":  { "version": "5.8.3", "role": "service" }
/// }
/// ```
pub const KEY_METADATA: &str = "metadata";

// ── Keys nested under `metadata.<source>` ────────────────────────────

/// Per-source version string.
pub const NESTED_VERSION: &str = "version";

/// Service KPI query definitions (ServiceExtension JSON).
pub const NESTED_SERVICE_QUERIES: &str = "service_queries";

/// The role this source plays in the recording. Known values:
/// - `"service"` — the system under test (e.g. an LLM inference server)
/// - `"loadgen"` — the load generator / benchmark tool (e.g. llm-perf)
pub const NESTED_ROLE: &str = "role";
