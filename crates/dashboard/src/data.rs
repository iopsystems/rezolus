//! Trait abstraction over the data source the dashboard renders against.
//!
//! Section generators only need *metadata* about the underlying data
//! (sampling interval, time bounds, metric names, label fan-out per
//! metric) to construct `View`s — actual query execution happens
//! elsewhere (the viewer wires up the DuckDB-backed query engine
//! separately and runs the SQL strings emitted on plots).

/// Read-only metadata about a loaded recording. Implemented for
/// `SqlCapture` (file mode) and `EmptyDashboardData` (the schema-dump
/// placeholder used by `crates/dashboard/src/main.rs` and tests).
///
/// All methods are cheap — they describe the schema, not the data.
pub trait DashboardData {
    /// Sampling interval in seconds (e.g. 1.0 for the typical 1Hz capture).
    fn interval(&self) -> f64;
    /// Inclusive (min, max) timestamps in nanoseconds across all series, or
    /// `None` for an empty recording.
    fn time_range(&self) -> Option<(u64, u64)>;
    /// Recording source identifier (e.g. `"rezolus"`).
    fn source(&self) -> &str;
    /// Recording-source version string.
    fn version(&self) -> &str;
    /// Filename the recording was loaded from (display-only).
    fn filename(&self) -> &str;

    /// Names of all counter metrics in the recording.
    fn counter_names(&self) -> Vec<&str>;
    /// Names of all gauge metrics.
    fn gauge_names(&self) -> Vec<&str>;
    /// Names of all histogram metrics.
    fn histogram_names(&self) -> Vec<&str>;

    /// How many distinct label sets exist for this counter metric. Used for
    /// the per-metric fan-out shown in `View::num_series` — return `0` when
    /// the metric isn't present.
    fn counter_label_count(&self, name: &str) -> usize;
    fn gauge_label_count(&self, name: &str) -> usize;
    fn histogram_label_count(&self, name: &str) -> usize;

    /// Number of distinct values of `key` (e.g. `"id"`) across all series of
    /// `metric`, looking across counter/gauge/histogram collections. Returns
    /// 0 if the metric is unknown. Used by section generators to decide
    /// whether to render per-device variants of charts — when there's only
    /// one CPU/GPU, the per-device chart degenerates to the aggregate.
    fn unique_label_values(&self, metric: &str, key: &str) -> usize;

    /// H2 `grouping_power` of `metric`, when known. Returns `None`
    /// for non-histogram metrics or unknown ones. Used by the
    /// service-KPI emitter to substitute `{{p}}` into histogram
    /// SQL templates with the right per-metric value.
    fn histogram_grouping_power(&self, _metric: &str) -> Option<u8> {
        None
    }
}

/// Empty `DashboardData` for the schema-dump binary and test fixtures.
/// Reports zero metrics and an undefined time range — enough for
/// section generators to emit dashboard JSON without binding to any
/// concrete data source. Production code uses `SqlCapture` (via the
/// `metriken-query` engine).
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyDashboardData;

impl DashboardData for EmptyDashboardData {
    fn interval(&self) -> f64 {
        1.0
    }
    fn time_range(&self) -> Option<(u64, u64)> {
        None
    }
    fn source(&self) -> &str {
        ""
    }
    fn version(&self) -> &str {
        ""
    }
    fn filename(&self) -> &str {
        ""
    }
    fn counter_names(&self) -> Vec<&str> {
        Vec::new()
    }
    fn gauge_names(&self) -> Vec<&str> {
        Vec::new()
    }
    fn histogram_names(&self) -> Vec<&str> {
        Vec::new()
    }
    fn counter_label_count(&self, _name: &str) -> usize {
        0
    }
    fn gauge_label_count(&self, _name: &str) -> usize {
        0
    }
    fn histogram_label_count(&self, _name: &str) -> usize {
        0
    }
    fn unique_label_values(&self, _metric: &str, _key: &str) -> usize {
        0
    }
}
