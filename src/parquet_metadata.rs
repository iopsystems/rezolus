#![allow(dead_code)]

// ── Parquet writer settings ─────────────────────────────────────────────

/// Maximum number of rows per row group. All rezolus tools that write parquet
/// files should apply this (via `ParquetOptions::max_batch_size`) so row group
/// sizing is consistent across recordings and combined files.
///
/// Deliberately smaller than `metriken-exposition`'s 50k default: the viewer
/// reads time-windowed slices when you zoom in, and a query only decodes the
/// row groups that overlap the window. Finer groups make drill-down far
/// cheaper — measured on a 24h @1s recording, a 5-minute-window histogram
/// query dropped from ~474 ms (2 giant groups) to ~20 ms at 1800 rows/group,
/// while full-range scans are unchanged (decode-bound). The cost is worse
/// compression: ~1.75× file size on that recording (small groups compress
/// less well). 1800 rows ≈ 30 min at the default 1 s sampling interval.
pub const MAX_ROW_GROUP_SIZE: usize = 1800;

// ── Top-level parquet footer keys ───────────────────────────────────────

/// The keys a reader looks up (`source`, `version`, `service_queries`,
/// `events`, `per_source_metadata`, ...) are `dashboard::keys`, shared with
/// the viewers, `report-save` and the `recording` tools; the ones the
/// binary names through this module are re-exported here. What remains
/// below is what only rezolus writes.
pub use ::dashboard::keys::{
    KEY_DESCRIPTIONS, KEY_EVENTS, KEY_PER_SOURCE_METADATA, KEY_SERVICE_QUERIES, KEY_SYSTEMINFO,
    KEY_VERSION,
};

/// The producing agent's **counter epoch**: an opaque id that changes when,
/// and only when, every cumulative counter in the recording restarted from
/// zero together — which for a process-scoped producer is once per process.
///
/// Two recordings with equal epochs over overlapping time are two
/// observations of one monotonic series. A change means every counter reset
/// at that point, which a consumer cannot otherwise distinguish from a
/// counter that merely wrapped.
///
/// Name and semantics follow dendro's `keys::PRODUCER_EPOCH`, so an archive
/// written here reads the same to any consumer of that format.
///
/// **Source-wide, and it does not cover a single counter.** A counter that
/// wrapped, or that a sampler zeroes on read, did not restart the process, so
/// this says nothing about it; that needs a per-counter generation.
pub const KEY_PRODUCER_EPOCH: &str = "producer_epoch";

/// Every producer epoch a recording observed, in order: a JSON array of
/// `{"epoch": <id>, "from_ts": <first row timestamp>, "version": <agent
/// version>}`. Written when the agent restarts mid-recording; absent when no
/// restart was seen. The last entry is the current epoch, which is also under
/// [`KEY_PRODUCER_EPOCH`]. Every cumulative counter reset at each entry's
/// `from_ts` after the first.
///
/// Name, shape and the first two fields follow dendro's
/// `keys::PRODUCER_EPOCHS`; `version` is rezolus's addition, so a reader can
/// tell which agent build wrote the rows from each point on.
pub const KEY_PRODUCER_EPOCHS: &str = "producer_epochs";

/// JSON-serialised user selection/filter state saved by the viewer.
pub const KEY_SELECTION: &str = "selection";

// ── Keys nested under `per_source_metadata.<source>.<entry>` ─────────

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

// ── Viewer hints ─────────────────────────────────────────────────────

/// The default rezolus node to display when the viewer opens a combined
/// file with multiple nodes. Set by `parquet combine --pinned <node>`.
pub const KEY_PINNED_NODE: &str = "pinned_node";

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
