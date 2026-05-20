//! DuckDB-backed loader + Arrow-extraction helpers for MCP subcommands.
//!
//! Each MCP CLI invocation creates its own `DuckDbBackend` (cheap; no
//! parquet read happens until the first query). `SqlCapture::open`
//! warms the pool by reading parquet metadata + a one-shot
//! `min/max(timestamp)` query — same path the file-mode viewer uses,
//! reused here so a single source of truth governs catalog/time-range
//! semantics.
//!
//! Returns the backend (the caller keeps it for running SQL) alongside
//! the `SqlCapture` (dashboard metadata + cached time range). Both are
//! reference-counted: the MCP stdio server caches them per parquet path.
//!
//! [`Series`] + [`batches_to_series`] project Arrow batches into the
//! `{labels, values}` shape that the statistical consumers in
//! `correlation.rs` and `anomaly_detection/` operate on. The projection
//! contract matches `crates/prom-matrix/src/native.rs::arrow_to_prom_matrix`:
//! one `t` field (DOUBLE seconds), one `v` field (numeric — NULL/NaN
//! drops the row), all other fields become per-series labels.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, AsArray};
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use metriken_query::DuckDbBackend;

use crate::viewer::sql_capture::SqlCapture;

/// One time series — label set plus `(timestamp_seconds, value)`
/// tuples. Local to MCP; algorithms in `correlation.rs` and
/// `anomaly_detection/` consume this directly.
#[derive(Debug, Clone, Default)]
pub struct Series {
    pub labels: HashMap<String, String>,
    pub values: Vec<(f64, f64)>,
}

/// Load a parquet capture for an MCP subcommand.
///
/// Returns `(backend, capture)`. Errors are stringified to
/// `Box<dyn Error>` because MCP subcommands surface failures through
/// the same boxed-error channel.
pub fn open_capture(
    path: &Path,
) -> Result<(Arc<DuckDbBackend>, SqlCapture), Box<dyn std::error::Error>> {
    let backend = Arc::new(DuckDbBackend::new());
    let capture =
        SqlCapture::open(path, &backend).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    Ok((backend, capture))
}

/// Project Arrow `RecordBatch`es into a `Vec<Series>` per the standard
/// `t DOUBLE, v <numeric>, labels...` contract.
///
/// Behaviour pinned to match
/// `crates/prom-matrix/src/native.rs::arrow_to_prom_matrix`:
/// - Rows with NULL or non-finite `v` are dropped (Prometheus gap).
/// - Rows with NULL or unparseable `t` are dropped.
/// - The first batch's schema is authoritative; later batches must
///   match (single-statement DuckDB queries guarantee this).
/// - Empty input or missing `t`/`v` columns returns an empty vec, not
///   an error — same fail-open posture as the matrix emitter.
pub fn batches_to_series(batches: &[RecordBatch]) -> Vec<Series> {
    let Some(first) = batches.iter().find(|b| b.num_rows() > 0) else {
        return Vec::new();
    };

    let schema = first.schema();
    let mut t_idx: Option<usize> = None;
    let mut v_idx: Option<usize> = None;
    let mut label_indices: Vec<(usize, String)> = Vec::new();
    for (i, field) in schema.fields().iter().enumerate() {
        match field.name().as_str() {
            "t" => t_idx = Some(i),
            "v" => v_idx = Some(i),
            name => label_indices.push((i, name.to_string())),
        }
    }
    let (Some(t_idx), Some(v_idx)) = (t_idx, v_idx) else {
        return Vec::new();
    };

    // Stable group-by-label-tuple identical to prom-matrix's keying:
    // a serialized JSON map of (sorted) labels. We use a String key
    // because HashMap doesn't impl Hash; the small per-row stringify
    // is fine for MCP workloads (offline analysis, not hot path).
    let mut groups: Vec<Series> = Vec::new();
    let mut group_index: HashMap<String, usize> = HashMap::new();

    for batch in batches {
        if batch.num_rows() == 0 {
            continue;
        }
        let t_col = batch.column(t_idx);
        let v_col = batch.column(v_idx);
        let label_cols: Vec<(&dyn Array, &str)> = label_indices
            .iter()
            .map(|(i, name)| (batch.column(*i).as_ref(), name.as_str()))
            .collect();

        for row in 0..batch.num_rows() {
            let Some(v) = cell_to_f64(v_col.as_ref(), row) else {
                continue;
            };
            if !v.is_finite() {
                continue;
            }
            let Some(t) = cell_to_f64(t_col.as_ref(), row) else {
                continue;
            };

            let mut labels = HashMap::with_capacity(label_cols.len());
            for (col, name) in &label_cols {
                let s = cell_to_string(*col, row).unwrap_or_else(|| "null".to_string());
                labels.insert((*name).to_string(), s);
            }

            // Sort keys for the group key so two rows with the same
            // logical label set hash to the same bucket regardless of
            // iteration order.
            let mut entries: Vec<(&String, &String)> = labels.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut key = String::new();
            for (k, v) in entries {
                key.push_str(k);
                key.push('=');
                key.push_str(v);
                key.push('\0');
            }
            let idx = match group_index.get(&key) {
                Some(&idx) => idx,
                None => {
                    let idx = groups.len();
                    group_index.insert(key, idx);
                    groups.push(Series {
                        labels,
                        values: Vec::new(),
                    });
                    idx
                }
            };
            groups[idx].values.push((t, v));
        }
    }

    groups
}

/// Build the canonical SQL for summing a counter metric's per-series
/// per-second rate, projected as `(t DOUBLE, v DOUBLE)`. Matches the
/// PromQL idiom `sum(rate(M[1m]))` that the legacy MCP exhaustive
/// anomaly-detection loop uses.
///
/// Returns `None` if the metric isn't in the catalog. The per-column
/// rate (`irate_1s`) is summed because `sum(rate(M))` in PromQL is
/// `sum_over_series(rate_per_series(M))` — taking the rate of the
/// sum would mishandle per-series resets.
pub fn counter_sum_rate_sql(
    catalog: &metriken_query::MetricCatalog,
    metric: &str,
) -> Option<String> {
    let series = catalog.series_by_metric.get(metric)?;
    if series.is_empty() {
        return None;
    }
    let v_expr = series
        .iter()
        .map(|s| {
            format!(
                "COALESCE(irate_1s({}, timestamp), 0)",
                quote_ident(&s.physical)
            )
        })
        .collect::<Vec<_>>()
        .join(" + ");
    Some(format!(
        "SELECT CAST(timestamp / 1e9 AS DOUBLE) AS t, {v_expr} AS v FROM _src ORDER BY t"
    ))
}

/// Build SQL for summing a gauge metric's series at each timestamp.
/// Matches PromQL `sum(M)`. Returns `None` if the metric isn't in
/// the catalog.
pub fn gauge_sum_sql(catalog: &metriken_query::MetricCatalog, metric: &str) -> Option<String> {
    let series = catalog.series_by_metric.get(metric)?;
    if series.is_empty() {
        return None;
    }
    let v_expr = series
        .iter()
        .map(|s| format!("COALESCE({}, 0)", quote_ident(&s.physical)))
        .collect::<Vec<_>>()
        .join(" + ");
    Some(format!(
        "SELECT CAST(timestamp / 1e9 AS DOUBLE) AS t, CAST({v_expr} AS DOUBLE) AS v FROM _src ORDER BY t"
    ))
}

/// Build SQL for the all-time `q` quantile of a combined histogram
/// metric — matches PromQL `histogram_quantile(q, M)` over cumulative
/// buckets. Series are folded with `h2_combine_lol` first so a
/// metric with multiple physical histograms produces a single
/// per-timestamp quantile.
///
/// `q` must be in (0, 1); typical anomaly-detection quantiles are
/// 0.50 / 0.90 / 0.99. Returns `None` if the metric isn't in the
/// catalog.
pub fn histogram_quantile_sql(
    catalog: &metriken_query::MetricCatalog,
    metric: &str,
    q: f64,
) -> Option<String> {
    let series = catalog.series_by_metric.get(metric)?;
    if series.is_empty() {
        return None;
    }
    let coalesced: Vec<String> = series
        .iter()
        .map(|s| format!("COALESCE({}, []::UBIGINT[])", quote_ident(&s.physical)))
        .collect();
    let combined = if coalesced.len() == 1 {
        coalesced.into_iter().next().unwrap()
    } else {
        format!("h2_combine_lol([{}])", coalesced.join(", "))
    };
    Some(format!(
        "SELECT CAST(timestamp / 1e9 AS DOUBLE) AS t, hist_p({combined}, {q}) AS v FROM _src ORDER BY t"
    ))
}

/// Quote a DuckDB identifier (column or table name). Doubles internal
/// `"`. Rezolus metric columns can contain `/` and `:` characters, so
/// every emitted reference must be quoted; this helper centralises
/// the rule so individual builders don't have to.
fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Read an Arrow cell as f64, returning `None` for NULL or
/// unsupported dtypes. Mirrors prom-matrix's `cell_to_f64`.
fn cell_to_f64(arr: &dyn Array, row: usize) -> Option<f64> {
    if arr.is_null(row) {
        return None;
    }
    Some(match arr.data_type() {
        DataType::Float64 => arr
            .as_primitive::<arrow::datatypes::Float64Type>()
            .value(row),
        DataType::Float32 => arr
            .as_primitive::<arrow::datatypes::Float32Type>()
            .value(row) as f64,
        DataType::Int64 => arr.as_primitive::<arrow::datatypes::Int64Type>().value(row) as f64,
        DataType::Int32 => arr.as_primitive::<arrow::datatypes::Int32Type>().value(row) as f64,
        DataType::UInt64 => arr
            .as_primitive::<arrow::datatypes::UInt64Type>()
            .value(row) as f64,
        DataType::UInt32 => arr
            .as_primitive::<arrow::datatypes::UInt32Type>()
            .value(row) as f64,
        _ => return None,
    })
}

/// Stringify an Arrow cell for label-column rendering. Mirrors
/// prom-matrix's `cell_to_string_opt` minus the NaN/Inf drop (those
/// only matter for `v`, handled separately).
fn cell_to_string(arr: &dyn Array, row: usize) -> Option<String> {
    if arr.is_null(row) {
        return None;
    }
    Some(match arr.data_type() {
        DataType::Utf8 => arr.as_string::<i32>().value(row).to_string(),
        DataType::LargeUtf8 => arr.as_string::<i64>().value(row).to_string(),
        DataType::Float64 => arr
            .as_primitive::<arrow::datatypes::Float64Type>()
            .value(row)
            .to_string(),
        DataType::Int64 => arr
            .as_primitive::<arrow::datatypes::Int64Type>()
            .value(row)
            .to_string(),
        DataType::Int32 => arr
            .as_primitive::<arrow::datatypes::Int32Type>()
            .value(row)
            .to_string(),
        DataType::UInt64 => arr
            .as_primitive::<arrow::datatypes::UInt64Type>()
            .value(row)
            .to_string(),
        DataType::UInt32 => arr
            .as_primitive::<arrow::datatypes::UInt32Type>()
            .value(row)
            .to_string(),
        DataType::Boolean => arr.as_boolean().value(row).to_string(),
        _ => format!("{arr:?}#{row}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("site")
            .join("viewer")
            .join("data")
            .join("demo.parquet")
    }

    #[test]
    fn open_capture_returns_metadata() {
        use dashboard::DashboardData;
        let path = fixture_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let (_backend, capture) = open_capture(&path).expect("open demo.parquet");
        // demo.parquet is a single-source rezolus recording.
        assert_eq!(capture.source(), "rezolus");
        // Time range must be populated for a non-empty recording.
        assert!(capture.time_range().is_some());
        // Counters and gauges exist.
        assert!(!capture.counter_names().is_empty());
    }

    #[test]
    fn open_capture_errors_on_missing_file() {
        let result = open_capture(Path::new("/nonexistent/file.parquet"));
        assert!(result.is_err());
    }

    /// End-to-end: load demo.parquet, run a simple per-CPU `cpu_usage`
    /// rate query through the backend, extract series. Confirms:
    /// - Multiple series (one per CPU `id` label) split correctly.
    /// - `t` is float seconds, `v` is a finite float.
    /// - Labels are populated from the schema label columns.
    #[test]
    fn batches_to_series_splits_by_label() {
        let path = fixture_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let (backend, capture) = open_capture(&path).expect("open demo.parquet");
        let path_str = capture.parquet_path().to_string_lossy().to_string();

        // demo.parquet has per-CPU `cpu_usage/user/<id>` columns. Project
        // a couple of them (CPU 0 and 1) with the standard `t/v/labels`
        // shape. Two SELECTs UNIONed so we exercise the multi-series
        // grouping path.
        let sql = "\
            SELECT \
              CAST(timestamp / 1e9 AS DOUBLE) AS t, \
              CAST(\"cpu_usage/user/0\" AS DOUBLE) AS v, \
              '0' AS id \
            FROM _src \
            UNION ALL \
            SELECT \
              CAST(timestamp / 1e9 AS DOUBLE) AS t, \
              CAST(\"cpu_usage/user/1\" AS DOUBLE) AS v, \
              '1' AS id \
            FROM _src \
        ";
        let batches = backend
            .run_sql(sql, &path_str)
            .expect("run_sql against demo.parquet");
        let series = batches_to_series(&batches);
        assert_eq!(series.len(), 2, "two distinct id labels → two series");
        for s in &series {
            assert_eq!(s.labels.len(), 1);
            assert!(s.labels.contains_key("id"));
            assert!(!s.values.is_empty(), "each series should have rows");
            // Spot-check shape of one tuple.
            let (t, v) = s.values[0];
            assert!(t > 0.0, "timestamp positive seconds");
            assert!(v.is_finite(), "value is a finite float");
        }
    }

    /// Empty-input fail-open: no batches, no `t`/`v` columns, or batches
    /// with zero rows all return an empty vec rather than erroring.
    #[test]
    fn batches_to_series_empty_input() {
        assert!(batches_to_series(&[]).is_empty());
    }

    /// `counter_sum_rate_sql` produces a valid SQL string for a real
    /// counter metric and DuckDB binds + executes it without error.
    /// `cpu_cycles` is per-CPU on demo.parquet (multiple series), so
    /// the COALESCE-sum-of-rates path is exercised.
    #[test]
    fn counter_sum_rate_sql_runs_against_demo_parquet() {
        let path = fixture_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let (backend, capture) = open_capture(&path).expect("open");
        let path_str = capture.parquet_path().to_string_lossy().to_string();
        let sql = counter_sum_rate_sql(&capture.catalog(), "cpu_cycles")
            .expect("cpu_cycles counter present on demo.parquet");
        let batches = backend.run_sql(&sql, &path_str).expect("run_sql");
        let series = batches_to_series(&batches);
        assert_eq!(series.len(), 1, "single aggregate series");
        let s = &series[0];
        // First row is NULL (no LAG) so it gets dropped by
        // batches_to_series; later rows should be finite floats.
        assert!(!s.values.is_empty(), "rate produced no rows");
        assert!(s.values.iter().all(|(_, v)| v.is_finite()));
    }

    /// `gauge_sum_sql` produces a valid SQL string for a gauge metric.
    /// `cpu_cores` is a single-value gauge on demo.parquet.
    #[test]
    fn gauge_sum_sql_runs_against_demo_parquet() {
        let path = fixture_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let (backend, capture) = open_capture(&path).expect("open");
        let path_str = capture.parquet_path().to_string_lossy().to_string();
        let sql = gauge_sum_sql(&capture.catalog(), "cpu_cores")
            .expect("cpu_cores gauge present on demo.parquet");
        let batches = backend.run_sql(&sql, &path_str).expect("run_sql");
        let series = batches_to_series(&batches);
        assert_eq!(series.len(), 1);
        let s = &series[0];
        assert!(!s.values.is_empty());
        // `cpu_cores` is positive on a real recording.
        assert!(s.values.iter().all(|(_, v)| *v > 0.0));
    }

    /// `histogram_quantile_sql` produces a valid SQL string for a
    /// histogram metric. `scheduler_runqueue_latency` is a histogram
    /// on demo.parquet; p50 should be a non-negative latency value.
    #[test]
    fn histogram_quantile_sql_runs_against_demo_parquet() {
        use dashboard::DashboardData;
        let path = fixture_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let (backend, capture) = open_capture(&path).expect("open");
        // Pick whichever histogram metric the parquet happens to
        // carry first — different demo recordings have different
        // histograms, but every real recording has at least one.
        let hist_name = {
            let names = capture.histogram_names();
            if names.is_empty() {
                eprintln!("skipping: no histograms on demo.parquet");
                return;
            }
            names[0].to_string()
        };
        let path_str = capture.parquet_path().to_string_lossy().to_string();
        let sql =
            histogram_quantile_sql(&capture.catalog(), &hist_name, 0.5).expect("histogram present");
        let batches = backend.run_sql(&sql, &path_str).expect("run_sql");
        // We don't assert on series count or row count — sparse
        // histograms can be all-NULL on some recordings. What matters
        // is that the SQL itself is well-formed and DuckDB executes
        // it without binder error.
        let _ = batches;
    }

    /// SQL builders return `None` for unknown metric names.
    #[test]
    fn sql_builders_return_none_for_unknown_metric() {
        let path = fixture_path();
        if !path.exists() {
            eprintln!("skipping: fixture {} missing", path.display());
            return;
        }
        let (_backend, capture) = open_capture(&path).expect("open");
        let cat = capture.catalog();
        assert!(counter_sum_rate_sql(&cat, "does_not_exist").is_none());
        assert!(gauge_sum_sql(&cat, "does_not_exist").is_none());
        assert!(histogram_quantile_sql(&cat, "does_not_exist", 0.5).is_none());
    }
}
