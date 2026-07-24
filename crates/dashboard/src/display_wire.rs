//! Display-mode wire encoding, shared by the server (axum) and WASM viewers so
//! both backends produce byte-identical decimated responses. The query itself is
//! a metriken-query `query_range_display`; this module owns the reducer options,
//! the compact binary column layout, and the result → bytes dispatch. Each shell
//! wraps the bytes its own way (axum `Response` vs a wasm-bindgen return).

use metriken_query::{
    DisplayOptions, DisplayResult, DisplaySeries, HistogramHeatmapResult, MetricsSource,
    QueryError, QueryOptions, RateMode, Reducer,
};

/// A display query's result, ready for a backend to ship: either the compact
/// binary body, or a non-series result (scalar/vector) to serialize as JSON.
pub enum DisplayWire {
    Binary(Vec<u8>),
    Json(DisplayResult),
}

/// Parse an `"lo,hi"` band argument, falling back to the interquartile range.
pub fn parse_band(s: Option<&str>) -> [f64; 2] {
    s.and_then(|s| {
        let (a, b) = s.split_once(',')?;
        Some([a.trim().parse().ok()?, b.trim().parse().ok()?])
    })
    .unwrap_or([0.25, 0.75])
}

/// Parse a rate time-alignment mode argument. `"raw"` selects [`RateMode::Raw`];
/// anything else (including absent) is the default [`RateMode::Grid`]. Shared by
/// both backends so the query param string maps the same way on each.
pub fn parse_rate_mode(s: Option<&str>) -> RateMode {
    match s {
        Some("raw") => RateMode::Raw,
        _ => RateMode::Grid,
    }
}

/// Run a display-mode range query and encode the result. `points` is the point
/// budget; `band` the inner-band quantiles; `rate_mode` the rate alignment.
pub fn display_query(
    source: &dyn MetricsSource,
    query: &str,
    start: f64,
    end: f64,
    step: f64,
    points: usize,
    band: [f64; 2],
    rate_mode: RateMode,
) -> Result<DisplayWire, QueryError> {
    let opts = DisplayOptions {
        budget: points,
        reducer: Reducer::Boxplot,
        band,
    };
    let qopts = QueryOptions::with_rate_mode(rate_mode);
    Ok(
        match source.query_range_display_opts(query, start, end, step, &opts, &qopts)? {
            DisplayResult::Series { result, budget } => {
                DisplayWire::Binary(encode_display_binary(&result, budget))
            }
            DisplayResult::HistogramHeatmap { result } => {
                DisplayWire::Binary(encode_heatmap_binary(&result))
            }
            other => DisplayWire::Json(other),
        },
    )
}

/// Encode display `Series` as a compact binary body:
///
/// ```text
/// [u32 LE headerLen][JSON header][pad to 8B][f64 LE column blobs]
/// ```
///
/// The JSON header carries per-series labels + provenance + point count `n`;
/// the blob is, per series in order, the six columns `t,min,lo,median,hi,max`
/// each `n` little-endian f64. A series carrying a measurement-uncertainty band
/// sets the header `unc` flag and appends two more columns (`uncLo,uncHi`,
/// `NaN` for points without a band) after the six — so a mixed response stays
/// self-describing. Padding keeps the first f64 8-byte aligned so the client can
/// view columns as `Float64Array`s with zero copies.
pub fn encode_display_binary(series: &[DisplaySeries], budget: u32) -> Vec<u8> {
    // A series carries a band iff any of its points does (a decimated bucket with
    // no intervals yields None). The flag lets the decoder know to read the two
    // extra columns for this series.
    let has_unc = |s: &DisplaySeries| s.points.iter().any(|p| p.unc_lo.is_some());
    let header = serde_json::json!({
        "resultType": "series",
        "budget": budget,
        "series": series
            .iter()
            .map(|s| serde_json::json!({
                "metric": s.metric,
                "nativeInterval": s.native_interval,
                "rawPoints": s.raw_points,
                "reducer": s.reducer,
                "band": s.band,
                "decimated": s.decimated,
                "unc": has_unc(s),
                "n": s.points.len(),
            }))
            .collect::<Vec<_>>(),
    });
    let header_bytes = serde_json::to_vec(&header).unwrap_or_default();
    let total_floats: usize = series
        .iter()
        .map(|s| s.points.len() * if has_unc(s) { 8 } else { 6 })
        .sum();

    let mut buf = Vec::with_capacity(4 + header_bytes.len() + 8 + total_floats * 8);
    buf.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&header_bytes);
    while buf.len() % 8 != 0 {
        buf.push(0);
    }
    for s in series {
        for p in &s.points {
            buf.extend_from_slice(&p.t.to_le_bytes());
        }
        for p in &s.points {
            buf.extend_from_slice(&p.min.to_le_bytes());
        }
        for p in &s.points {
            buf.extend_from_slice(&p.lo.to_le_bytes());
        }
        for p in &s.points {
            buf.extend_from_slice(&p.median.to_le_bytes());
        }
        for p in &s.points {
            buf.extend_from_slice(&p.hi.to_le_bytes());
        }
        for p in &s.points {
            buf.extend_from_slice(&p.max.to_le_bytes());
        }
        // Uncertainty band columns, only for a series that carries one. `NaN`
        // marks a point with no band; the client treats a non-finite edge as a
        // gap, matching the matrix path's per-point `null`.
        if has_unc(s) {
            for p in &s.points {
                buf.extend_from_slice(&p.unc_lo.unwrap_or(f64::NAN).to_le_bytes());
            }
            for p in &s.points {
                buf.extend_from_slice(&p.unc_hi.unwrap_or(f64::NAN).to_le_bytes());
            }
        }
    }
    buf
}

/// Encode a histogram bucket heatmap as a compact binary body:
///
/// ```text
/// [u32 LE headerLen][JSON header][pad to 8B]
/// [f64 timestamps][f64 counts][u32 timeIdx][u32 bucketIdx]
/// ```
///
/// The JSON header carries `bucketBounds`, `minValue`/`maxValue`, and the two
/// counts (`nTimestamps`, `nTriples`). Ordering the f64 columns first keeps
/// every column naturally aligned so the client views them as typed arrays with
/// zero copies — no JSON parse of the (potentially large) triples array.
pub fn encode_heatmap_binary(hm: &HistogramHeatmapResult) -> Vec<u8> {
    let n_ts = hm.timestamps.len();
    let n_tr = hm.data.len();
    let header = serde_json::json!({
        "resultType": "histogram_heatmap",
        "bucketBounds": hm.bucket_bounds,
        "minValue": hm.min_value,
        "maxValue": hm.max_value,
        "nTimestamps": n_ts,
        "nTriples": n_tr,
    });
    let header_bytes = serde_json::to_vec(&header).unwrap_or_default();

    let mut buf = Vec::with_capacity(4 + header_bytes.len() + 8 + n_ts * 8 + n_tr * 16);
    buf.extend_from_slice(&(header_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&header_bytes);
    while buf.len() % 8 != 0 {
        buf.push(0);
    }
    for t in &hm.timestamps {
        buf.extend_from_slice(&t.to_le_bytes());
    }
    for (_, _, count) in &hm.data {
        buf.extend_from_slice(&count.to_le_bytes());
    }
    for (time_idx, _, _) in &hm.data {
        buf.extend_from_slice(&(*time_idx as u32).to_le_bytes());
    }
    for (_, bucket_idx, _) in &hm.data {
        buf.extend_from_slice(&(*bucket_idx as u32).to_le_bytes());
    }
    buf
}
