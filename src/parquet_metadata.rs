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

/// Nanosecond timestamp of the first successful scrape for this source.
pub const NESTED_FIRST_SAMPLE_NS: &str = "first_sample_ns";

/// Nanosecond timestamp of the last successful scrape for this source.
pub const NESTED_LAST_SAMPLE_NS: &str = "last_sample_ns";

// ── Node / instance disambiguation ──────────────────────────────────

/// Node name for rezolus agent data. Identifies which host/VM the
/// metrics came from. Top-level in single-source files, nested under
/// `per_source_metadata.<source>.node` in combined files.
pub const KEY_NODE: &str = "node";

/// Instance identifier for service data. Identifies which process/container
/// the metrics came from. Top-level in single-source files, nested under
/// `per_source_metadata.<source>.instance` in combined files.
pub const KEY_INSTANCE: &str = "instance";

/// Per-source node name (nested key).
pub const NESTED_NODE: &str = "node";

/// Per-source instance identifier (nested key).
pub const NESTED_INSTANCE: &str = "instance";

/// Per-source nested key: JSON array of sampler status (from the agent's
/// `/samplers` endpoint). Present only for Rezolus-agent sources.
pub const NESTED_SAMPLER_STATUS: &str = "sampler_status";

/// Per-source metric descriptions (metric name → help text). Nested under
/// `per_source_metadata.<source>` in combined files; single-source files use
/// the top-level `descriptions` key instead.
pub const NESTED_DESCRIPTIONS: &str = "descriptions";

// ── Viewer hints ─────────────────────────────────────────────────────

/// The default rezolus node to display when the viewer opens a combined
/// file with multiple nodes. Set by `parquet combine --pinned <node>`.
pub const KEY_PINNED_NODE: &str = "pinned_node";

// ── One-off event annotations ───────────────────────────────────────

/// JSON payload of one-off events attached to the recording (restarts,
/// config changes, anomalies, ...). Value is an object `{"events": [...]}`
/// where each entry conforms to `dashboard::events::Event`. Each event
/// carries its own optional `source` / `node` / `instance` scope rather
/// than inheriting from file-level metadata, so the payload stays
/// self-describing across combine.
pub const KEY_EVENTS: &str = "events";

// ── Combined A/B tarball manifest ───────────────────────────────────

/// Wire shape of the `ab.json` manifest inside a `*.parquet.ab.tar`
/// archive produced by `parquet combine --ab`. The two captures live
/// next to the manifest as unmodified parquet entries
/// (`baseline.parquet`, `experiment.parquet`). Schema is versioned so
/// future additions (per-side descriptions, etc.) can land without
/// breaking older readers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AbContainers {
    pub version: u32,
    pub baseline: AbSide,
    pub experiment: AbSide,
    /// Optional category template name (e.g. `"inference-library"`) the
    /// viewer should auto-apply when loading this tarball. Set at
    /// combine time via `parquet combine --ab --category <name>`. CLI
    /// `--category` on `rezolus view` still wins. Optional so older
    /// manifests (and `--ab` runs without `--category`) deserialize
    /// without bumping `version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AbSide {
    /// Display alias for this side (e.g. "vllm", "sglang").
    pub alias: String,
    /// Source names contributed by this side. A multi-source input
    /// (service + loadgen) lists all its sources here. Informational —
    /// nothing in the load path branches on it.
    pub sources: Vec<String>,
}

impl AbContainers {
    pub const SCHEMA_VERSION: u32 = 1;
}

// ── Save-as-Report trimmed parquet ───────────────────────────────────

/// File-level marker: parquet was column-trimmed by "Save as Report"
/// (combined-A/B tarballs carry the marker in each per-side parquet).
/// Presence flips the viewer into report mode — empty section list,
/// frontend defaults to `/report`. String-typed so future variants
/// (`"full"`, etc.) don't need a schema bump.
pub const KEY_REPORT: &str = "report";

/// Canonical `KEY_REPORT` value written by the current writer.
pub const REPORT_VALUE_TRIMMED: &str = "trimmed";

/// Server-side mirror of the WASM `synthesize_manifest` in
/// `crates/viewer/src/lib.rs` — needed so compare-mode Save-as-Report
/// works when the manifest wasn't carried in from a pre-built A/B tar.
pub fn synthesize_ab_manifest(
    baseline_alias: Option<&str>,
    baseline_filename: &str,
    baseline_sources: &[String],
    experiment_alias: Option<&str>,
    experiment_filename: &str,
    experiment_sources: &[String],
    category: Option<&str>,
) -> AbContainers {
    fn resolve_alias(alias: Option<&str>, filename: &str, fallback: &str) -> String {
        if let Some(a) = alias {
            if !a.is_empty() {
                return a.to_string();
            }
        }
        let base = std::path::Path::new(filename)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if !base.is_empty() {
            base.to_string()
        } else {
            fallback.to_string()
        }
    }
    AbContainers {
        version: AbContainers::SCHEMA_VERSION,
        baseline: AbSide {
            alias: resolve_alias(baseline_alias, baseline_filename, "baseline"),
            sources: baseline_sources.to_vec(),
        },
        experiment: AbSide {
            alias: resolve_alias(experiment_alias, experiment_filename, "experiment"),
            sources: experiment_sources.to_vec(),
        },
        category: category.map(|s| s.to_string()),
    }
}

#[cfg(test)]
mod synthesize_tests {
    use super::*;

    fn srcs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn uses_explicit_aliases_when_set() {
        let m = synthesize_ab_manifest(
            Some("before"),
            "baseline.parquet",
            &srcs(&["rezolus"]),
            Some("after"),
            "experiment.parquet",
            &srcs(&["rezolus"]),
            None,
        );
        assert_eq!(m.version, AbContainers::SCHEMA_VERSION);
        assert_eq!(m.baseline.alias, "before");
        assert_eq!(m.experiment.alias, "after");
        assert_eq!(m.baseline.sources, vec!["rezolus".to_string()]);
        assert_eq!(m.experiment.sources, vec!["rezolus".to_string()]);
        assert_eq!(m.category, None);
    }

    #[test]
    fn falls_back_to_filename_basename_when_no_alias() {
        let m = synthesize_ab_manifest(
            None,
            "/tmp/foo/baseline.parquet",
            &srcs(&["rezolus"]),
            None,
            "experiment.parquet",
            &srcs(&["rezolus"]),
            None,
        );
        assert_eq!(m.baseline.alias, "baseline.parquet");
        assert_eq!(m.experiment.alias, "experiment.parquet");
    }

    #[test]
    fn empty_filename_falls_back_to_baseline_experiment_literal() {
        let m = synthesize_ab_manifest(
            None,
            "",
            &srcs(&["rezolus"]),
            None,
            "",
            &srcs(&["rezolus"]),
            None,
        );
        assert_eq!(m.baseline.alias, "baseline");
        assert_eq!(m.experiment.alias, "experiment");
    }

    #[test]
    fn passes_category_through() {
        let m = synthesize_ab_manifest(
            Some("a"),
            "a.parquet",
            &srcs(&["rezolus"]),
            Some("b"),
            "b.parquet",
            &srcs(&["rezolus"]),
            Some("inference-library"),
        );
        assert_eq!(m.category, Some("inference-library".to_string()));
    }

    #[test]
    fn multi_source_each_side_independent() {
        let m = synthesize_ab_manifest(
            Some("a"),
            "a.parquet",
            &srcs(&["rezolus", "vllm"]),
            Some("b"),
            "b.parquet",
            &srcs(&["rezolus", "sglang"]),
            None,
        );
        assert_eq!(
            m.baseline.sources,
            vec!["rezolus".to_string(), "vllm".to_string()]
        );
        assert_eq!(
            m.experiment.sources,
            vec!["rezolus".to_string(), "sglang".to_string()]
        );
    }
}
