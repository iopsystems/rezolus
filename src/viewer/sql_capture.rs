//! `SqlCapture` — the DuckDB-backed capture handle for the
//! file/upload/A-B viewer paths.
//!
//! Owns a parquet path, a cached `MetricCatalog` (from
//! `DuckDbBackend::describe_parquet`), and the few scalar pieces of
//! file-level metadata the dashboard needs (sampling interval, time
//! range, source, version, filename). Nothing in here executes queries
//! on the dashboard's behalf — handlers run SQL against the same
//! `DuckDbBackend` separately.
//!
//! `SqlCapture::open` is the single eager step: read parquet KV
//! metadata for `source`/`version`/`sampling_interval_ms`, walk the
//! Arrow schema to bucket counter/gauge/histogram metric names, and
//! run a one-shot `SELECT min/max(timestamp) FROM _src` through the
//! backend to cache the time range. The min/max query warms the
//! backend's per-source connection pool — desirable, since dashboard
//! queries will follow immediately.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::{DataType, Field};
use metriken_query_sql::{DuckDbBackend, MetricCatalog, SqlError};
use parquet::file::reader::{FileReader, SerializedFileReader};

use crate::parquet_metadata::{
    KEY_PER_SOURCE_METADATA, KEY_SAMPLING_INTERVAL_MS, KEY_SOURCE, KEY_VERSION,
    NESTED_VERSION,
};
use dashboard::DashboardData;

/// Counter / gauge / histogram tag for a metric. Derived from the
/// Arrow data type of the metric's first physical column (UInt64 =
/// counter, Int64 = gauge, `List<UInt64>` + a `grouping_power` field
/// metadata key = histogram). `MetricCatalog` deliberately drops this
/// tag — we recompute it once at load time so `counter_names` /
/// `gauge_names` / `histogram_names` are O(1) Vec returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricKind {
    Counter,
    Gauge,
    Histogram,
}

/// File-mode capture handle. Held by `CaptureRegistry` slots.
/// `Arc<MetricCatalog>` is cloned (cheap) into every handler that
/// needs catalog reads.
pub struct SqlCapture {
    /// Absolute path to the parquet on disk. Doubles as the
    /// `DuckDbBackend` pool key.
    parquet_path: PathBuf,
    /// Per-metric → physical-column-and-labels index. Built by
    /// `DuckDbBackend::describe_parquet`; shared with handlers via
    /// `Arc::clone`.
    catalog: Arc<MetricCatalog>,
    /// Counter / gauge / histogram tag per metric name. See
    /// [`MetricKind`].
    kind_by_metric: HashMap<String, MetricKind>,
    /// Sampling interval in seconds (typically 1.0). Derived from the
    /// `sampling_interval_ms` parquet KV; defaults to 1.0 when absent.
    interval_seconds: f64,
    /// Inclusive (min, max) timestamps in nanoseconds, cached at open
    /// time. `None` for an empty recording.
    time_range: Option<(u64, u64)>,
    /// Recording source (`"rezolus"`, `"llm-perf"`, …). Read from
    /// the `source` KV; combined files surface the first source name.
    source: String,
    /// Source version string. Combined files pull from
    /// `per_source_metadata.<source>.version`.
    version: String,
    /// Display filename. Defaults to the basename of `parquet_path`;
    /// overridable for tarballs / uploads via [`set_filename`].
    filename: String,
}

impl SqlCapture {
    /// Load a parquet at `path` through the shared `DuckDbBackend`.
    /// Cold-start (~ms scale): one parquet schema read + one
    /// `min/max(timestamp)` SQL round-trip that warms the per-source
    /// pool. Subsequent dashboard queries hit the warm pool.
    pub fn open(path: &Path, backend: &DuckDbBackend) -> Result<Self, SqlError> {
        let parquet_path = path.to_path_buf();
        let path_str = parquet_path.to_string_lossy().to_string();

        let catalog = backend.describe_parquet(&path_str)?;
        let kind_by_metric = classify_metrics(&parquet_path).unwrap_or_default();
        let (source, version, interval_seconds) =
            read_scalar_metadata(&parquet_path).unwrap_or_default();
        let time_range = query_time_range(backend, &path_str)?;
        let filename = parquet_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("capture.parquet")
            .to_string();

        Ok(Self {
            parquet_path,
            catalog,
            kind_by_metric,
            interval_seconds: if interval_seconds > 0.0 {
                interval_seconds
            } else {
                1.0
            },
            time_range,
            source,
            version,
            filename,
        })
    }

    /// Path to the parquet file on disk. Used as the `DuckDbBackend`
    /// data_source key when handlers run queries against this capture.
    pub fn parquet_path(&self) -> &Path {
        &self.parquet_path
    }

    /// Shared catalog handle. Cheap clone.
    pub fn catalog(&self) -> Arc<MetricCatalog> {
        Arc::clone(&self.catalog)
    }

    /// Override the display filename. Used by A/B tarball loaders and
    /// the upload handler to surface the user-friendly name rather
    /// than the temp-extracted basename.
    pub fn set_filename(&mut self, name: impl Into<String>) {
        self.filename = name.into();
    }
}

impl DashboardData for SqlCapture {
    fn interval(&self) -> f64 {
        self.interval_seconds
    }
    fn time_range(&self) -> Option<(u64, u64)> {
        self.time_range
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
        self.names_with_kind(MetricKind::Counter)
    }
    fn gauge_names(&self) -> Vec<&str> {
        self.names_with_kind(MetricKind::Gauge)
    }
    fn histogram_names(&self) -> Vec<&str> {
        self.names_with_kind(MetricKind::Histogram)
    }

    fn counter_label_count(&self, name: &str) -> usize {
        self.label_count_for(name, MetricKind::Counter)
    }
    fn gauge_label_count(&self, name: &str) -> usize {
        self.label_count_for(name, MetricKind::Gauge)
    }
    fn histogram_label_count(&self, name: &str) -> usize {
        self.label_count_for(name, MetricKind::Histogram)
    }

    fn histogram_grouping_power(&self, metric: &str) -> Option<u8> {
        self.catalog.histogram_p_by_metric.get(metric).copied()
    }

    fn unique_label_values(&self, metric: &str, key: &str) -> usize {
        let Some(series) = self.catalog.series_by_metric.get(metric) else {
            return 0;
        };
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for s in series {
            if let Some(v) = s.labels.get(key) {
                seen.insert(v.as_str());
            }
        }
        seen.len()
    }
}

impl SqlCapture {
    fn names_with_kind(&self, want: MetricKind) -> Vec<&str> {
        self.kind_by_metric
            .iter()
            .filter(|(_, kind)| **kind == want)
            .map(|(name, _)| name.as_str())
            .collect()
    }

    fn label_count_for(&self, name: &str, want: MetricKind) -> usize {
        if self.kind_by_metric.get(name).copied() != Some(want) {
            return 0;
        }
        self.catalog
            .series_by_metric
            .get(name)
            .map_or(0, |s| s.len())
    }
}

/// Walk the parquet schema once and tag each metric as
/// counter/gauge/histogram. Mirrors the classification rules in
/// `metriken-query-sql::views::classify`, but we keep our own copy
/// because `MetricCatalog` deliberately drops the kind tag (the
/// wide-form SQL generator doesn't need it) and we don't want to
/// thread it through the upstream public API just for the dashboard.
///
/// Returns an empty map on any parquet read error — the catalog will
/// still be usable, but section generators will see every metric as
/// missing and rendering will degrade gracefully.
fn classify_metrics(path: &Path) -> Option<HashMap<String, MetricKind>> {
    let file = std::fs::File::open(path).ok()?;
    let reader = SerializedFileReader::new(file).ok()?;
    let parquet_meta = reader.metadata();
    let schema = parquet::arrow::parquet_to_arrow_schema(
        parquet_meta.file_metadata().schema_descr(),
        parquet_meta.file_metadata().key_value_metadata(),
    )
    .ok()?;

    let mut out: HashMap<String, MetricKind> = HashMap::new();
    for field in schema.fields() {
        if field.name() == "timestamp" || field.name() == "duration" {
            continue;
        }
        let Some(kind) = classify_field(field) else {
            continue;
        };
        let name = canonical_metric_name(field);
        // First column for a given canonical name wins, matching the
        // dedupe rule in metriken-query-sql/src/views.rs:151.
        out.entry(name).or_insert(kind);
    }
    Some(out)
}

fn classify_field(field: &Field) -> Option<MetricKind> {
    match field.data_type() {
        DataType::UInt64 => Some(MetricKind::Counter),
        DataType::Int64 => Some(MetricKind::Gauge),
        DataType::List(inner) if inner.data_type() == &DataType::UInt64 => {
            // Histograms require a grouping_power metadata key — without
            // it the column is just a list and unusable for h2_* macros.
            field
                .metadata()
                .get("grouping_power")
                .and_then(|v| v.parse::<u8>().ok())
                .map(|_| MetricKind::Histogram)
        }
        _ => None,
    }
}

fn canonical_metric_name(field: &Field) -> String {
    if let Some(name) = field.metadata().get("metric") {
        return name.clone();
    }
    field
        .name()
        .strip_suffix(":buckets")
        .unwrap_or(field.name())
        .to_string()
}

/// Read `source`, `version`, and `sampling_interval_ms` (converted to
/// seconds as f64) out of the parquet file KV metadata. Combined
/// files surface the first listed source name and pull `version` from
/// `per_source_metadata.<source>.version`.
///
/// Returns `None` on any I/O or parse failure; caller substitutes
/// sensible defaults.
fn read_scalar_metadata(path: &Path) -> Option<(String, String, f64)> {
    let file = std::fs::File::open(path).ok()?;
    let reader = SerializedFileReader::new(file).ok()?;
    let kvs = reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .cloned()
        .unwrap_or_default();

    let lookup = |key: &str| -> Option<String> {
        kvs.iter()
            .find(|kv| kv.key == key)
            .and_then(|kv| kv.value.clone())
    };

    // `source` is either a bare string (single-source) or a JSON
    // array (combined). For dashboard display purposes we pick the
    // first source name.
    let source = lookup(KEY_SOURCE)
        .map(|raw| {
            serde_json::from_str::<Vec<String>>(&raw)
                .ok()
                .and_then(|arr| arr.into_iter().next())
                .unwrap_or(raw)
        })
        .unwrap_or_default();

    // Single-source files store `version` directly; combined files
    // hide it under per_source_metadata.<source>.version. Try the
    // direct path first; if absent and we have a per-source blob,
    // dig into the per-source object for our chosen source.
    let version = lookup(KEY_VERSION).unwrap_or_else(|| {
        lookup(KEY_PER_SOURCE_METADATA)
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|v| {
                v.get(&source)
                    .and_then(|src| src.get(NESTED_VERSION))
                    .and_then(|s| s.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default()
    });

    let interval_seconds = lookup(KEY_SAMPLING_INTERVAL_MS)
        .and_then(|s| s.parse::<u64>().ok())
        .map(|ms| ms as f64 / 1000.0)
        .unwrap_or(0.0);

    Some((source, version, interval_seconds))
}

/// Run `SELECT min(timestamp), max(timestamp) FROM _src` once at
/// load time to cache the time range. The query warms the backend's
/// per-source connection pool as a side effect — paying the cold
/// start at load is better than ambushing the first dashboard
/// request with it.
fn query_time_range(
    backend: &DuckDbBackend,
    data_source: &str,
) -> Result<Option<(u64, u64)>, SqlError> {
    use arrow::array::Array;

    // Force UBIGINT on both projections — `min(timestamp)` on a UBIGINT
    // column comes back as a DECIMAL(38,0) under some DuckDB versions,
    // and an Arrow `as_primitive::<UInt64Type>()` cast panics on that.
    let batches = backend.run_sql(
        "SELECT \
           CAST(min(timestamp) AS UBIGINT) AS lo, \
           CAST(max(timestamp) AS UBIGINT) AS hi \
         FROM _src",
        data_source,
    )?;
    for batch in &batches {
        if batch.num_rows() == 0 {
            continue;
        }
        let lo_col = batch.column(0);
        let hi_col = batch.column(1);
        if lo_col.is_null(0) || hi_col.is_null(0) {
            return Ok(None);
        }
        let lo = cell_to_u64(lo_col.as_ref(), 0);
        let hi = cell_to_u64(hi_col.as_ref(), 0);
        match (lo, hi) {
            (Some(lo), Some(hi)) => return Ok(Some((lo, hi))),
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// Read a row's value as `u64` regardless of whether the column came
/// back as UBIGINT / BIGINT / TIMESTAMP-as-ns. Falls back to `None`
/// on NULL or unsupported types.
fn cell_to_u64(arr: &dyn arrow::array::Array, row: usize) -> Option<u64> {
    use arrow::array::AsArray;
    use arrow::datatypes::{DataType, Int64Type, UInt64Type};
    if arr.is_null(row) {
        return None;
    }
    match arr.data_type() {
        DataType::UInt64 => Some(arr.as_primitive::<UInt64Type>().value(row)),
        DataType::Int64 => Some(arr.as_primitive::<Int64Type>().value(row) as u64),
        _ => None,
    }
}
