//! Trait abstraction over the data source the dashboard renders against.
//!
//! Section generators only need *metadata* about the underlying data (sampling
//! interval, time bounds, metric names, label fan-out per metric) to construct
//! `View`s — actual query execution happens elsewhere (the viewer wires up the
//! query engine separately and runs the PromQL/SQL strings emitted on plots).
//!
//! Decoupling this from a concrete `Tsdb` lets the same dashboard generators
//! drive both the legacy in-memory PromQL backend and the upcoming DuckDB/SQL
//! backend (which will read parquet column metadata to satisfy the same
//! questions).
use metriken_query::Tsdb;

/// Read-only metadata about a loaded recording. Implemented for `Tsdb`
/// (legacy) and, when the SQL viewer lands, the DuckDB-backed adapter.
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
}

impl DashboardData for Tsdb {
    fn interval(&self) -> f64 {
        Tsdb::interval(self)
    }
    fn time_range(&self) -> Option<(u64, u64)> {
        Tsdb::time_range(self)
    }
    fn source(&self) -> &str {
        Tsdb::source(self)
    }
    fn version(&self) -> &str {
        Tsdb::version(self)
    }
    fn filename(&self) -> &str {
        Tsdb::filename(self)
    }
    fn counter_names(&self) -> Vec<&str> {
        Tsdb::counter_names(self)
    }
    fn gauge_names(&self) -> Vec<&str> {
        Tsdb::gauge_names(self)
    }
    fn histogram_names(&self) -> Vec<&str> {
        Tsdb::histogram_names(self)
    }
    fn counter_label_count(&self, name: &str) -> usize {
        Tsdb::counter_labels(self, name).map_or(0, |l| l.len())
    }
    fn gauge_label_count(&self, name: &str) -> usize {
        Tsdb::gauge_labels(self, name).map_or(0, |l| l.len())
    }
    fn histogram_label_count(&self, name: &str) -> usize {
        Tsdb::histogram_labels(self, name).map_or(0, |l| l.len())
    }
}
