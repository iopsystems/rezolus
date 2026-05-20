//! `LiveCapture` — DashboardData-shaped wrapper around a
//! `metriken_query::LiveSource`. The baseline slot of a live-mode
//! viewer carries one of these; the SQL query path goes through the
//! backend's `live_sources` map under `LIVE_BASELINE_DATA_SOURCE`, but
//! anything that needs *schema* metadata (interval, time range,
//! metric names, label fan-out) goes through this struct's
//! `DashboardData` impl.
//!
//! The schema cache (`counter_metrics`, `gauge_metrics`,
//! `histogram_metrics`, plus per-metric label fan-out) is owned by
//! `LiveCapture` so the `DashboardData` impl can return `&str` slices
//! against `self`. The cache is updated in-place by the live ingest
//! bridge (`live_ingest::ingest_snapshot`) whenever a snapshot lands.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use metriken_query::{LiveColumn, LiveColumnKind, LiveSource};

use ::dashboard::DashboardData;

/// Per-metric schema observed in the live stream. Tracks which
/// physical columns belong to each metric (so label fan-out counts
/// match the parquet path's `SqlCapture::label_count_for`) and the
/// distinct values of each label key (for `unique_label_values`).
#[derive(Default)]
struct MetricSchema {
    /// Set of `physical` column names that fan out from this metric.
    /// Cardinality = `*_label_count` return value.
    physicals: BTreeSet<String>,
    /// `label_key → set of distinct values` seen across all series of
    /// this metric. Powers `unique_label_values`.
    label_values: BTreeMap<String, BTreeSet<String>>,
}

pub struct LiveCapture {
    /// Underlying SQL data source. Query handlers reach this via the
    /// backend's `live_sources` map; `LiveCapture` holds an `Arc` so
    /// `time_range_ns()` can query it for the inclusive `_src`
    /// timestamp range on each call.
    live: Arc<LiveSource>,
    /// Snapshot polling interval in seconds (mirrors
    /// `SqlCapture::interval_seconds`). Set once at construction.
    interval_seconds: f64,
    /// Recording source identifier (e.g. `"rezolus"`). Set once at
    /// construction; matches the source label parquet captures stamp.
    source: String,
    /// Source version string (from `fetch_agent_info`).
    version: String,
    /// Display filename — for live mode this is the agent URL.
    filename: String,
    /// Per-kind metric schemas accumulated from snapshots. Updated by
    /// `observe_columns` as new metrics or labels appear.
    counter_metrics: BTreeMap<String, MetricSchema>,
    gauge_metrics: BTreeMap<String, MetricSchema>,
    histogram_metrics: BTreeMap<String, MetricSchema>,
}

impl LiveCapture {
    pub fn new(
        live: Arc<LiveSource>,
        sampling_interval_ms: u64,
        source: impl Into<String>,
        version: impl Into<String>,
        filename: impl Into<String>,
    ) -> Self {
        Self {
            live,
            interval_seconds: (sampling_interval_ms as f64) / 1000.0,
            source: source.into(),
            version: version.into(),
            filename: filename.into(),
            counter_metrics: BTreeMap::new(),
            gauge_metrics: BTreeMap::new(),
            histogram_metrics: BTreeMap::new(),
        }
    }

    /// Underlying live source. Used by the ingest bridge to call
    /// `LiveSource::append`.
    pub fn live(&self) -> &Arc<LiveSource> {
        &self.live
    }

    /// Display-filename update. Called when a re-`/api/v1/connect`
    /// swaps the agent URL.
    #[allow(dead_code)]
    pub fn set_filename(&mut self, name: impl Into<String>) {
        self.filename = name.into();
    }

    /// Update the in-memory schema cache from a snapshot's column
    /// descriptors. Idempotent — re-observing the same column is a
    /// no-op. Called from `live_ingest::ingest_snapshot` after the
    /// `LiveSource::append`.
    pub fn observe_columns(&mut self, columns: &[LiveColumn]) {
        for col in columns {
            let bucket = match col.kind {
                LiveColumnKind::Counter => &mut self.counter_metrics,
                LiveColumnKind::Gauge => &mut self.gauge_metrics,
                LiveColumnKind::Histogram { .. } => &mut self.histogram_metrics,
            };
            let entry = bucket.entry(col.metric.clone()).or_default();
            entry.physicals.insert(col.physical.clone());
            for (k, v) in &col.labels {
                entry
                    .label_values
                    .entry(k.clone())
                    .or_default()
                    .insert(v.clone());
            }
        }
    }
}

impl DashboardData for LiveCapture {
    fn interval(&self) -> f64 {
        self.interval_seconds
    }
    fn time_range(&self) -> Option<(u64, u64)> {
        // LiveSource queries take a Mutex; small cost, called rarely.
        self.live.time_range_ns().ok().flatten()
    }
    fn source(&self) -> &str {
        &self.source
    }
    fn version(&self) -> &str {
        &self.version
    }
    fn filename(&self) -> &str {
        &self.filename
    }

    fn counter_names(&self) -> Vec<&str> {
        self.counter_metrics.keys().map(|s| s.as_str()).collect()
    }
    fn gauge_names(&self) -> Vec<&str> {
        self.gauge_metrics.keys().map(|s| s.as_str()).collect()
    }
    fn histogram_names(&self) -> Vec<&str> {
        self.histogram_metrics.keys().map(|s| s.as_str()).collect()
    }

    fn counter_label_count(&self, name: &str) -> usize {
        self.counter_metrics
            .get(name)
            .map_or(0, |s| s.physicals.len())
    }
    fn gauge_label_count(&self, name: &str) -> usize {
        self.gauge_metrics
            .get(name)
            .map_or(0, |s| s.physicals.len())
    }
    fn histogram_label_count(&self, name: &str) -> usize {
        self.histogram_metrics
            .get(name)
            .map_or(0, |s| s.physicals.len())
    }

    fn unique_label_values(&self, metric: &str, key: &str) -> usize {
        let from = |bucket: &BTreeMap<String, MetricSchema>| {
            bucket
                .get(metric)
                .and_then(|s| s.label_values.get(key))
                .map_or(0, |v| v.len())
        };
        let n = from(&self.counter_metrics);
        if n > 0 {
            return n;
        }
        let n = from(&self.gauge_metrics);
        if n > 0 {
            return n;
        }
        from(&self.histogram_metrics)
    }
}
