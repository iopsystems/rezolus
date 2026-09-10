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
// Eight arguments, and they are the right eight: this is the display-mode
// query entry point, and every one of them is a distinct axis the caller
// chooses per request (what to query, over what window, at what resolution,
// with what band and rate alignment). They are bundled into `DisplayOptions`
// immediately below; hoisting that struct into the signature would only move
// the argument list to the call sites, which is where it is least readable.
#[allow(clippy::too_many_arguments)]
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
/// self-describing. A series with interpolated points then sets `interp` and
/// appends one more (`1.0`/`0.0` per point). Order is fixed: the six, then the
/// two `unc` columns if flagged, then the one `interp` column if flagged, so a
/// decoder that reads the flags in that order stays aligned whichever
/// combination a series has. Padding keeps the first f64 8-byte aligned so the
/// client can view columns as `Float64Array`s with zero copies.
pub fn encode_display_binary(series: &[DisplaySeries], budget: u32) -> Vec<u8> {
    // A series carries a band iff any of its points does (a decimated bucket with
    // no intervals yields None). The flag lets the decoder know to read the two
    // extra columns for this series.
    let has_unc = |s: &DisplaySeries| s.points.iter().any(|p| p.unc_lo.is_some());
    // Likewise for the interpolated flag: a series that never crosses an
    // unobserved stretch pays nothing for the feature. Sent as f64 rather than a
    // packed bitmap so it decodes as one more zero-copy `Float64Array` like
    // every other column — one byte per point saved is not worth a second
    // decode path.
    let has_interp = |s: &DisplaySeries| s.points.iter().any(|p| p.interpolated);
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
                "interp": has_interp(s),
                "n": s.points.len(),
            }))
            .collect::<Vec<_>>(),
    });
    let header_bytes = serde_json::to_vec(&header).unwrap_or_default();
    let total_floats: usize = series
        .iter()
        .map(|s| s.points.len() * (6 + if has_unc(s) { 2 } else { 0 } + usize::from(has_interp(s))))
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
        // Interpolated column last, so it does not shift the `unc` columns for a
        // series that has both.
        if has_interp(s) {
            for p in &s.points {
                buf.extend_from_slice(&f64::from(u8::from(p.interpolated)).to_le_bytes());
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use metriken_query::display::{DisplaySeries, EnvPoint};

    use super::*;

    /// Decode a body back into (header, per-series f64 columns) the way the
    /// browser does, so a test asserts against the bytes actually shipped
    /// rather than against the encoder's own internals.
    fn decode(buf: &[u8]) -> (serde_json::Value, Vec<Vec<f64>>) {
        let header_len = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
        let header: serde_json::Value = serde_json::from_slice(&buf[4..4 + header_len]).unwrap();
        let mut off = (4 + header_len).div_ceil(8) * 8;
        let mut cols = Vec::new();
        for s in header["series"].as_array().unwrap() {
            let n = s["n"].as_u64().unwrap() as usize;
            let ncols = 6
                + if s["unc"].as_bool().unwrap() { 2 } else { 0 }
                + if s["interp"].as_bool().unwrap() { 1 } else { 0 };
            for _ in 0..ncols {
                let mut col = Vec::with_capacity(n);
                for _ in 0..n {
                    col.push(f64::from_le_bytes(buf[off..off + 8].try_into().unwrap()));
                    off += 8;
                }
                cols.push(col);
            }
        }
        // Every byte after the header must be accounted for by the flags; a
        // leftover means the encoder wrote a column the header does not
        // describe, which is exactly how a decoder silently misaligns.
        assert_eq!(off, buf.len(), "trailing bytes the header did not describe");
        (header, cols)
    }

    fn series(name: &str, points: Vec<EnvPoint>) -> DisplaySeries {
        DisplaySeries {
            metric: HashMap::from([("__name__".into(), name.into())]),
            points,
            native_interval: 1.0,
            raw_points: 3,
            reducer: Reducer::Boxplot,
            band: [0.25, 0.75],
            decimated: true,
        }
    }

    fn pt(t: f64, v: f64) -> EnvPoint {
        EnvPoint::new(t, v, v, v, v, v)
    }

    #[test]
    fn a_series_with_no_interpolated_points_pays_nothing() {
        let s = series("a", vec![pt(1.0, 5.0), pt(2.0, 6.0)]);
        let (header, cols) = decode(&encode_display_binary(&[s], 500));
        assert_eq!(header["series"][0]["interp"], serde_json::json!(false));
        assert_eq!(cols.len(), 6, "six columns, no optional ones");
    }

    #[test]
    fn the_interp_column_is_written_after_the_unc_pair() {
        // Both optional column sets present. Distinct values throughout so a
        // wrong ORDER shows up as wrong data rather than as plausible numbers —
        // this is the property the hand-written JS mirror in
        // tests/display_binary_decode.test.mjs depends on.
        let s = series(
            "a",
            vec![
                pt(1.0, 5.0)
                    .with_band(Some((10.0, 11.0)))
                    .with_interpolated(true),
                pt(2.0, 6.0)
                    .with_band(Some((20.0, 21.0)))
                    .with_interpolated(false),
            ],
        );
        let (header, cols) = decode(&encode_display_binary(&[s], 500));
        assert_eq!(header["series"][0]["unc"], serde_json::json!(true));
        assert_eq!(header["series"][0]["interp"], serde_json::json!(true));
        assert_eq!(cols.len(), 9);
        assert_eq!(cols[6], vec![10.0, 20.0], "uncLo");
        assert_eq!(cols[7], vec![11.0, 21.0], "uncHi");
        assert_eq!(cols[8], vec![1.0, 0.0], "interp, last");
    }

    #[test]
    fn interp_without_a_band_still_lands_in_the_seventh_column() {
        // The combination the renderer actually hits: rate() across a hole
        // yields a value with NO band and the interpolated flag set. With `unc`
        // false the interp column moves from index 8 to index 6, which is the
        // whole reason the decoder must branch on both flags in order.
        let s = series("a", vec![pt(1.0, 5.0).with_interpolated(true)]);
        let (header, cols) = decode(&encode_display_binary(&[s], 500));
        assert_eq!(header["series"][0]["unc"], serde_json::json!(false));
        assert_eq!(cols.len(), 7);
        assert_eq!(cols[6], vec![1.0]);
    }

    #[test]
    fn mixed_series_each_declare_their_own_columns() {
        // Three series with different flag combinations in one body. The decode
        // helper's trailing-byte assertion is what makes this bite: mis-size any
        // series and the later ones read into the wrong bytes.
        let a = series("a", vec![pt(1.0, 1.0).with_interpolated(true)]);
        let b = series("b", vec![pt(1.0, 2.0)]);
        let c = series("c", vec![pt(1.0, 3.0).with_band(Some((7.0, 8.0)))]);
        let (header, cols) = decode(&encode_display_binary(&[a, b, c], 500));
        assert_eq!(header["series"][0]["interp"], serde_json::json!(true));
        assert_eq!(header["series"][1]["interp"], serde_json::json!(false));
        assert_eq!(header["series"][2]["interp"], serde_json::json!(false));
        // 7 + 6 + 8
        assert_eq!(cols.len(), 21);
        assert_eq!(cols[6], vec![1.0], "a's interp column");
        // a occupies 0..=6 (7 cols), b 7..=12 (6), so c starts at 13: its
        // columns are t,min,lo,median,hi,max,uncLo,uncHi = 13..=20.
        assert_eq!(cols[13], vec![1.0], "c's t");
        assert_eq!(cols[16], vec![3.0], "c's median, still aligned");
        assert_eq!(
            cols[20],
            vec![8.0],
            "c's uncHi, the last column in the body"
        );
    }
}
