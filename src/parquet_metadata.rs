#![allow(dead_code)]

// ── Parquet writer settings ─────────────────────────────────────────────

/// Maximum number of rows per row group. Matches the value used by
/// `metriken-exposition`'s `ParquetWriter` (`DEFAULT_MAX_BATCH_SIZE`).
/// All rezolus tools that write parquet files should use this constant
/// so that row group sizing is consistent across recordings and combined
/// files.
pub const MAX_ROW_GROUP_SIZE: usize = 50_000;

// ── Top-level parquet footer keys ───────────────────────────────────────

/// Top-level parquet footer keys. These are read by `Tsdb::load` or shared
/// infrastructure and must remain flat strings in both single-source and
/// combined files.
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
/// endpoint). Display-only — used by the viewer and MCP.
pub const KEY_SYSTEMINFO: &str = "systeminfo";

/// JSON map of metric name → help text. Used by MCP `describe-metrics`.
pub const KEY_DESCRIPTIONS: &str = "descriptions";

/// JSON-serialised user selection/filter state saved by the viewer.
pub const KEY_SELECTION: &str = "selection";

/// Service KPI query definitions (ServiceExtension JSON). Top-level key
/// used in single-source parquet files written by `parquet annotate`.
/// When files are combined, this is moved under
/// `per_source_metadata.<source>.service_queries`.
pub const KEY_SERVICE_QUERIES: &str = "service_queries";

/// Per-source metadata map (used in combined files). Value is a JSON
/// object keyed by source name:
///
/// ```json
/// {
///   "llm-perf": { "version": "0.1.0", "role": "loadgen", "service_queries": { ... } },
///   "rezolus":  { "version": "5.8.3", "role": "service" }
/// }
/// ```
pub const KEY_PER_SOURCE_METADATA: &str = "per_source_metadata";

// ── Keys nested under `per_source_metadata.<source>` ─────────────────

/// Per-source version string.
pub const NESTED_VERSION: &str = "version";

/// Service KPI query definitions (ServiceExtension JSON).
pub const NESTED_SERVICE_QUERIES: &str = "service_queries";

/// The role this source plays in the recording. Known values:
/// - `"service"` — the system under test (e.g. an LLM inference server)
/// - `"loadgen"` — the load generator / benchmark tool (e.g. llm-perf)
pub const NESTED_ROLE: &str = "role";
