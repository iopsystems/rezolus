//! `/api/v1/heatmap_range` — bucket heatmap and quantile spectrum
//! source-of-truth endpoint.
//!
//! `query_range` projects through the Prometheus matrix shape, which
//! forces the frontend to reshape `(t, b_idx, count)` triples back into
//! a dense grid. The bucket-heatmap and quantile-spectrum charts both
//! consume dense shapes directly, so this handler skips the matrix
//! detour: it runs `bucket_heatmap_sql` / `quantile_spectrum_sql`,
//! projects the Arrow batches into a chart-ready JSON envelope, and
//! returns it.

use std::sync::Arc;

use arrow::array::{Array, AsArray, Float64Array, ListArray, UInt64Array};
use arrow::datatypes::UInt64Type;
use arrow::record_batch::RecordBatch;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use http::{header, StatusCode};
use serde::{Deserialize, Serialize};

use super::capture_registry::CaptureId;
use super::state::{ApiResponse, AppState};
use ::dashboard::sql;
use metriken_query_sql::udf::{bucket_count, h2_upper};

/// Allowed `kind` values on the `heatmap_range` request.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HeatmapKind {
    Buckets,
    QuantileSpectrum,
}

#[derive(Debug, Deserialize)]
pub struct HeatmapRangeParams {
    /// Canonical metric name (no `:buckets` suffix).
    pub metric: String,
    pub kind: HeatmapKind,
    /// Required for `quantile_spectrum`; comma-separated list of
    /// quantiles in `[0.0, 1.0]`. Ignored for `buckets`.
    #[serde(default)]
    pub quantiles: Option<String>,
    /// Capture slot — `baseline` (default) or `experiment`.
    #[serde(default)]
    pub capture: Option<String>,
    /// Optional node selector. When set, the SQL runs against the
    /// per-node view rather than the aggregate `_src`. Unknown values
    /// return HTTP 400.
    #[serde(default)]
    pub node: Option<String>,
}

/// Untagged response envelope. The two variants share only the
/// `time_data` field — the rest of the shape differs enough that a
/// tagged enum would just push the discriminant work onto the
/// frontend, which already keys per-chart-type rendering on the
/// requested `kind`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum HeatmapRangeResponse {
    Buckets {
        time_data: Vec<f64>,
        bucket_bounds: Vec<u64>,
        /// Sparse `(t_idx, b_idx, count)` triples. Zero-count cells
        /// are dropped server-side to keep the wire small.
        data: Vec<(u32, u32, u64)>,
        /// Min/max over the non-zero counts. The frontend uses these
        /// for the log10 color-scale bounds; zeros must be excluded
        /// so `log10(0)` never leaks in.
        min_value: f64,
        max_value: f64,
    },
    QuantileSpectrum {
        time_data: Vec<f64>,
        /// `data[i]` is the per-timestamp series for the i-th
        /// quantile (after the p0 peel). Cast UBIGINT counts/ns to
        /// DOUBLE before serialization to avoid JSON precision drift.
        data: Vec<Vec<f64>>,
        /// Display names per series (e.g. `"p50"`, `"p99.9"`).
        /// Same length as `data`.
        series_names: Vec<String>,
        /// The peeled-off `p0` series — used as the colorscale's
        /// lower anchor (consumers want a per-row floor reference,
        /// not the global minimum). `None` when `0.0` wasn't in
        /// the requested quantile list.
        color_min_anchor: Option<Vec<f64>>,
    },
}

pub async fn handler(
    Query(params): Query<HeatmapRangeParams>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let capture = CaptureId::parse_opt(params.capture.as_deref());
    let Some(data_source) = super::routes::data_source_for(&state, capture) else {
        return ApiResponse::<serde_json::Value>::err(
            format!("capture '{capture:?}' not attached"),
            "capture_not_found",
        )
        .into_response();
    };

    // p comes from the per-metric `grouping_power` catalog lookup. We
    // can't usefully guess: a high-p metric run with p=3 would skew
    // bucket bounds and quantile resolution.
    let catalog = match state.sql_backend.describe_parquet(&data_source) {
        Ok(c) => c,
        Err(e) => {
            return ApiResponse::<serde_json::Value>::err(
                format!("describe_parquet: {e}"),
                "sql_error",
            )
            .into_response();
        }
    };
    let Some(&p) = catalog.histogram_p_by_metric.get(&params.metric) else {
        return ApiResponse::<serde_json::Value>::err(
            format!(
                "metric '{}' is not a histogram in this capture",
                params.metric
            ),
            "unknown_metric",
        )
        .into_response();
    };

    // Validate the node selector (when present). Unknown nodes are a
    // 400, not a silent fallthrough — the frontend caller should be
    // working from a catalog-derived list.
    let node = match params.node.as_deref() {
        Some(name) => {
            let nodes: Vec<&str> = catalog.nodes();
            if !nodes.contains(&name) {
                return (
                    StatusCode::BAD_REQUEST,
                    ApiResponse::<serde_json::Value>::err(
                        format!("unknown node '{name}' — known nodes: {}", nodes.join(", "),),
                        "unknown_node",
                    ),
                )
                    .into_response();
            }
            Some(name.to_string())
        }
        None => None,
    };

    let backend = state.sql_backend.clone();
    let metric = params.metric.clone();
    let kind = params.kind;
    let quantiles = match kind {
        HeatmapKind::Buckets => Vec::new(),
        HeatmapKind::QuantileSpectrum => match parse_quantiles(params.quantiles.as_deref()) {
            Ok(qs) => qs,
            Err(e) => {
                return ApiResponse::<serde_json::Value>::err(e, "bad_request").into_response()
            }
        },
    };

    let result = tokio::task::spawn_blocking(move || -> Result<HeatmapRangeResponse, String> {
        let sql_text = match kind {
            HeatmapKind::Buckets => sql::bucket_heatmap_sql(&metric, None),
            HeatmapKind::QuantileSpectrum => {
                sql::quantile_spectrum_sql(&metric, &quantiles, p, None)
            }
        };
        // Rewrite `_src` → `_src_node_<X>` when a node was requested.
        // The emitters always target `_src` by default; the rewrite is
        // the same one `range_query` uses so the two paths stay in
        // lockstep.
        let sql_text = match node.as_deref() {
            Some(n) => super::routes::rewrite_src_to_node_view(&sql_text, n),
            None => sql_text,
        };
        let batches = backend
            .run_sql(&sql_text, &data_source)
            .map_err(|e| e.to_string())?;
        match kind {
            HeatmapKind::Buckets => project_buckets(&batches, p),
            HeatmapKind::QuantileSpectrum => project_quantile_spectrum(&batches, &quantiles),
        }
    })
    .await;

    let outcome = match result {
        Ok(o) => o,
        Err(join_err) => {
            return ApiResponse::<serde_json::Value>::err(
                format!("heatmap task panicked: {join_err}"),
                "sql_error",
            )
            .into_response();
        }
    };
    match outcome {
        Ok(body) => {
            let json = serde_json::to_string(&body).expect("HeatmapRangeResponse serializes");
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                json,
            )
                .into_response()
        }
        Err(msg) => ApiResponse::<serde_json::Value>::err(msg, "sql_error").into_response(),
    }
}

fn parse_quantiles(raw: Option<&str>) -> Result<Vec<f64>, String> {
    let Some(raw) = raw else {
        return Err("kind=quantile_spectrum requires `quantiles=...`".into());
    };
    let mut out = Vec::new();
    for piece in raw.split(',') {
        let v: f64 = piece
            .trim()
            .parse()
            .map_err(|_| format!("invalid quantile: '{piece}'"))?;
        if !(0.0..=1.0).contains(&v) {
            return Err(format!("quantile out of range: {v}"));
        }
        out.push(v);
    }
    if out.is_empty() {
        return Err("quantiles list is empty".into());
    }
    Ok(out)
}

/// Walk the `(timestamp, buckets)` rows and emit:
///  - `time_data`: per-row timestamps as DOUBLE seconds.
///  - `bucket_bounds`: inclusive upper bound of each bucket index
///    (computed via `h2_upper(b_idx, p)`).
///  - `data`: sparse `(t_idx, b_idx, count)` triples for non-zero
///    cells only.
///  - `min_value` / `max_value`: log-safe bounds over the non-zero
///    counts (defaults to 0/0 on an empty result).
fn project_buckets(batches: &[RecordBatch], p: u8) -> Result<HeatmapRangeResponse, String> {
    let nb = bucket_count(p as u32);
    let bucket_bounds: Vec<u64> = (0..nb).map(|i| h2_upper(i, p as u32)).collect();

    let mut time_data: Vec<f64> = Vec::new();
    let mut data: Vec<(u32, u32, u64)> = Vec::new();
    let mut min_value: u64 = u64::MAX;
    let mut max_value: u64 = 0;

    for batch in batches {
        // Schema: timestamp (UBIGINT or BIGINT), buckets (LIST<UBIGINT>).
        // First non-NULL `buckets` row appears at index 1 of the source
        // (LAG over the first row is NULL); drop NULL rows here.
        let ts_col = batch.column(0);
        let buckets_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| "buckets column is not a List".to_string())?;
        for row in 0..batch.num_rows() {
            if ts_col.is_null(row) || buckets_col.is_null(row) {
                continue;
            }
            let ts_ns = cell_to_u64(ts_col.as_ref(), row)
                .ok_or_else(|| "timestamp cell unreadable".to_string())?;
            let t_idx = time_data.len() as u32;
            time_data.push(ts_ns as f64 / 1e9);
            let list_values = buckets_col.value(row);
            let counts = list_values
                .as_any()
                .downcast_ref::<UInt64Array>()
                .ok_or_else(|| "buckets list element is not UBIGINT".to_string())?;
            for b_idx in 0..counts.len() {
                if counts.is_null(b_idx) {
                    continue;
                }
                let c = counts.value(b_idx);
                if c == 0 {
                    continue;
                }
                data.push((t_idx, b_idx as u32, c));
                if c < min_value {
                    min_value = c;
                }
                if c > max_value {
                    max_value = c;
                }
            }
        }
    }
    if data.is_empty() {
        min_value = 0;
        max_value = 0;
    }
    Ok(HeatmapRangeResponse::Buckets {
        time_data,
        bucket_bounds,
        data,
        min_value: min_value as f64,
        max_value: max_value as f64,
    })
}

/// Walk the `(t, qs)` rows and unpack the `qs` list column-wise into
/// per-quantile parallel arrays. `quantiles[i] == 0.0` is peeled off
/// as `color_min_anchor`.
fn project_quantile_spectrum(
    batches: &[RecordBatch],
    quantiles: &[f64],
) -> Result<HeatmapRangeResponse, String> {
    // Identify the p0 column (if any). The peel-off rule says: keep the
    // p0 series separate, drop it from the main `data` array, expose it
    // via `color_min_anchor`. We accept at most one p0; multiple are an
    // input bug.
    let p0_idx = quantiles.iter().position(|q| *q == 0.0);
    let mut color_min_anchor: Option<Vec<f64>> = p0_idx.map(|_| Vec::new());

    let kept_idxs: Vec<usize> = (0..quantiles.len())
        .filter(|&i| Some(i) != p0_idx)
        .collect();
    let series_names: Vec<String> = kept_idxs
        .iter()
        .map(|&i| quantile_label(quantiles[i]))
        .collect();
    let mut data: Vec<Vec<f64>> = vec![Vec::new(); kept_idxs.len()];
    let mut time_data: Vec<f64> = Vec::new();

    for batch in batches {
        let t_col = batch
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or_else(|| "t column is not DOUBLE".to_string())?;
        let qs_col = batch
            .column(1)
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| "qs column is not a List".to_string())?;
        for row in 0..batch.num_rows() {
            if t_col.is_null(row) || qs_col.is_null(row) {
                continue;
            }
            time_data.push(t_col.value(row));
            let list_values = qs_col.value(row);
            let values = list_values
                .as_any()
                .downcast_ref::<UInt64Array>()
                .ok_or_else(|| "qs list element is not UBIGINT".to_string())?;
            if values.len() != quantiles.len() {
                return Err(format!(
                    "h2_quantiles row width {} disagrees with requested quantile count {}",
                    values.len(),
                    quantiles.len()
                ));
            }
            for (out_i, &src_i) in kept_idxs.iter().enumerate() {
                let v = if values.is_null(src_i) {
                    f64::NAN
                } else {
                    values.value(src_i) as f64
                };
                data[out_i].push(v);
            }
            if let Some(anchor) = color_min_anchor.as_mut() {
                let pi = p0_idx.unwrap();
                let v = if values.is_null(pi) {
                    f64::NAN
                } else {
                    values.value(pi) as f64
                };
                anchor.push(v);
            }
        }
    }

    Ok(HeatmapRangeResponse::QuantileSpectrum {
        time_data,
        data,
        series_names,
        color_min_anchor,
    })
}

fn quantile_label(q: f64) -> String {
    // `0.999` → `p99.9`, `0.9` → `p90`. Strip trailing zeros so common
    // values stay tidy, but keep meaningful tails for 0.999 / 0.9999.
    let pct = q * 100.0;
    let s = format!("{pct}");
    // Trim trailing zeros after a decimal point.
    let trimmed = if s.contains('.') {
        let t = s.trim_end_matches('0').trim_end_matches('.');
        if t.is_empty() { "0" } else { t }.to_string()
    } else {
        s
    };
    format!("p{trimmed}")
}

fn cell_to_u64(arr: &dyn Array, row: usize) -> Option<u64> {
    use arrow::datatypes::{DataType, Int64Type};
    if arr.is_null(row) {
        return None;
    }
    match arr.data_type() {
        DataType::UInt64 => Some(arr.as_primitive::<UInt64Type>().value(row)),
        DataType::Int64 => Some(arr.as_primitive::<Int64Type>().value(row) as u64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantile_label_strips_trailing_zeros() {
        assert_eq!(quantile_label(0.5), "p50");
        assert_eq!(quantile_label(0.9), "p90");
        assert_eq!(quantile_label(0.99), "p99");
        assert_eq!(quantile_label(0.999), "p99.9");
        assert_eq!(quantile_label(0.9999), "p99.99");
        assert_eq!(quantile_label(1.0), "p100");
    }

    #[test]
    fn parse_quantiles_validates_range_and_emptiness() {
        assert_eq!(
            parse_quantiles(Some("0.5,0.9,0.99")).unwrap(),
            vec![0.5, 0.9, 0.99]
        );
        assert!(parse_quantiles(None).is_err());
        assert!(parse_quantiles(Some("")).is_err());
        assert!(parse_quantiles(Some("1.5")).is_err());
        assert!(parse_quantiles(Some("-0.1")).is_err());
        assert!(parse_quantiles(Some("abc")).is_err());
    }
}
