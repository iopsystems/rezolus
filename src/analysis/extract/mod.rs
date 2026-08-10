//! Extraction: turn a recording into a deterministic [`OverviewRecord`].
//!
//! v1 emits one aggregated entry per metric *name* (counters as
//! `sum(rate(m[1m]))`, gauges as `sum(m)`, histograms as three quantile
//! entries `m:p50`/`m:p90`/`m:p99`), so emitted names are unique and
//! `EvidenceRef.metric` is unambiguous. Per-metric cost is two queries
//! (the anomaly engine re-queries internally); acceptable for v1.
//!
//! Known v1 limitations: `.rez` recordings carry no `version` metadata
//! (`agent_version: None`); a multi-recording `.rez` opened via the MCP
//! pool path exposes only its first recording's metadata.

pub mod context;
pub mod features;
