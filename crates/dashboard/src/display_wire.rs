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

/// The canonical mixed-flag body used to pin the display-binary layout across
/// languages.
///
/// The layout's whole risk is that the two optional column groups are read in
/// the wrong order, or read for a series that did not flag them: nothing fails
/// loudly, the *later* series in the buffer just decode into the wrong bytes.
/// A decoder that gets it wrong still returns plausible numbers.
///
/// Rust and JS each had tests for this, but each tested against its own
/// hand-written mirror of the other side — so the two could drift together and
/// stay green. This fixture is the shared artifact instead: one buffer produced
/// by the real encoder, checked in as hex alongside its expected decode, that
/// every implementation asserts against. A third consumer (systemslab, if it
/// adopts the binary path) can use the same file without reimplementing the
/// encoder to test its decoder.
///
/// Deliberate properties, each catching a distinct mistake:
/// - all four flag combinations appear (`unc` only, `interp` only, both, neither)
/// - the series have DIFFERENT point counts, so a decoder that assumes a uniform
///   stride misaligns rather than coincidentally working
/// - "both" is first, so an off-by-one propagates through everything after it
/// - every column holds distinct values, so a swap shows up as wrong data rather
///   than as plausible-looking numbers
/// - `unc` is partial within a series (some points `None`), which is the real
///   shape a hole produces
pub mod fixture {
    use std::collections::HashMap;

    use metriken_query::Reducer;
    use metriken_query::display::{DisplaySeries, EnvPoint};

    /// Budget recorded in the fixture's header.
    pub const BUDGET: u32 = 500;

    fn series(name: &str, points: Vec<EnvPoint>) -> DisplaySeries {
        DisplaySeries {
            metric: HashMap::from([("__name__".to_string(), name.to_string())]),
            points,
            native_interval: 1.0,
            raw_points: 1000,
            reducer: Reducer::Boxplot,
            band: [0.25, 0.75],
            decimated: true,
        }
    }

    /// The four series described above, in the order they appear in the body.
    pub fn mixed_flags() -> Vec<DisplaySeries> {
        let p = |t: f64, v: f64| EnvPoint::new(t, v - 2.0, v - 1.0, v, v + 1.0, v + 2.0);
        vec![
            // both flags, 4 points, and the band is present on only some of
            // them — the shape a hole actually produces.
            series(
                "both",
                vec![
                    p(10.0, 100.0).with_band(Some((90.0, 110.0))),
                    p(11.0, 200.0).with_interpolated(true),
                    p(12.0, 300.0).with_interpolated(true),
                    p(13.0, 400.0).with_band(Some((390.0, 410.0))),
                ],
            ),
            // unc only, 2 points
            series(
                "unc_only",
                vec![
                    p(20.0, 500.0).with_band(Some((490.0, 510.0))),
                    p(21.0, 600.0).with_band(Some((590.0, 610.0))),
                ],
            ),
            // interp only, 3 points
            series(
                "interp_only",
                vec![
                    p(30.0, 700.0),
                    p(31.0, 800.0).with_interpolated(true),
                    p(32.0, 900.0),
                ],
            ),
            // neither, 2 points — the plain case, last, so it only decodes
            // correctly if every series before it was sized right.
            series("neither", vec![p(40.0, 1000.0), p(41.0, 1100.0)]),
        ]
    }
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

    /// Path to the cross-language golden, relative to this crate.
    const FIXTURE: &str = "../../tests/fixtures/display_wire_mixed.json";

    fn to_hex(bytes: &[u8]) -> String {
        let mut s = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    /// Encode the shared fixture and pin it against the checked-in golden.
    ///
    /// This is also the generator: `UPDATE_FIXTURES=1 cargo test -p dashboard`
    /// rewrites the file. Regenerating is the correct response to a DELIBERATE
    /// layout change — and the diff is then the review artifact, showing exactly
    /// which bytes moved. It is the wrong response to a surprise; if this fails
    /// and you did not mean to change the wire, a decoder somewhere is about to
    /// start reading the wrong columns.
    #[test]
    fn the_shared_fixture_matches_the_encoder() {
        let body = encode_display_binary(&fixture::mixed_flags(), fixture::BUDGET);
        let (header, cols) = decode(&body);

        // The expected decode travels WITH the bytes so a consumer in another
        // language has something to assert against without reimplementing the
        // encoder — which is the whole point, since a reimplementation is
        // exactly what drifts.
        let mut expected = Vec::new();
        let mut c = 0usize;
        for s in header["series"].as_array().unwrap() {
            let n = 6
                + if s["unc"].as_bool().unwrap() { 2 } else { 0 }
                + if s["interp"].as_bool().unwrap() { 1 } else { 0 };
            let names: Vec<&str> = ["t", "min", "lo", "median", "hi", "max"]
                .into_iter()
                .chain(if s["unc"].as_bool().unwrap() {
                    vec!["uncLo", "uncHi"]
                } else {
                    vec![]
                })
                // Named `interpCol` rather than `interp` so it cannot collide
                // with the header FLAG of the same name — and it matches what
                // data.js calls the decoded column.
                .chain(if s["interp"].as_bool().unwrap() {
                    vec!["interpCol"]
                } else {
                    vec![]
                })
                .collect();
            let mut m = serde_json::Map::new();
            m.insert("name".into(), s["metric"]["__name__"].clone());
            m.insert("n".into(), s["n"].clone());
            m.insert("unc".into(), s["unc"].clone());
            m.insert("interp".into(), s["interp"].clone());
            for (name, col) in names.iter().zip(cols[c..c + n].iter()) {
                // NaN has no JSON form; it marks a point with no band, so it is
                // written as null and the consumer treats a non-finite edge as
                // "no band" exactly as it does when decoding the raw f64.
                let vals: Vec<serde_json::Value> = col
                    .iter()
                    .map(|v| {
                        if v.is_finite() {
                            serde_json::json!(v)
                        } else {
                            serde_json::Value::Null
                        }
                    })
                    .collect();
                m.insert((*name).to_string(), serde_json::Value::Array(vals));
            }
            expected.push(serde_json::Value::Object(m));
            c += n;
        }

        let doc = serde_json::json!({
            // These two travel INSIDE the file on purpose. A consumer that
            // fetches the raw JSON by URL — which is how a third decoder should
            // read it, rather than vendoring a copy that goes stale — gets the
            // bytes and NOT the neighbouring README. Putting the warning only
            // there would separate it from the thing it guards, which is the
            // exact failure the warning is about.
            "_comment": concat!(
                "Generated by dashboard::display_wire tests. Do not hand-edit. ",
                "Regenerate with: UPDATE_FIXTURES=1 cargo test -p dashboard ",
                "the_shared_fixture -- that is the RIGHT response to a ",
                "deliberate wire change (the diff then shows which bytes ",
                "moved) and the WRONG response to a test that went red on its ",
                "own. A red test here means some decoder is about to read the ",
                "wrong columns; regenerating would only hide it. See ",
                "tests/fixtures/README.md.",
            ),
            "_layout": concat!(
                "[u32 LE headerLen][JSON header][pad to 8B][f64 LE columns]. ",
                "Per series: t,min,lo,median,hi,max, then uncLo,uncHi iff ",
                "header `unc`, then interp iff header `interp`. Read the flags ",
                "in that order; a decoder that does not will silently misalign ",
                "every later series -- it will not error, it will return ",
                "plausible numbers from the wrong bytes.",
            ),
            "budget": fixture::BUDGET,
            "hex": to_hex(&body),
            "expected": expected,
        });
        let rendered = format!("{}\n", serde_json::to_string_pretty(&doc).unwrap());

        if std::env::var("UPDATE_FIXTURES").is_ok() {
            std::fs::write(FIXTURE, &rendered).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(FIXTURE).unwrap_or_else(|e| {
            panic!("{FIXTURE} unreadable ({e}); regenerate with UPDATE_FIXTURES=1")
        });
        assert_eq!(
            on_disk, rendered,
            "display-binary layout no longer matches the shared fixture. If the \
             change was deliberate, regenerate with UPDATE_FIXTURES=1 and review \
             the diff; otherwise a decoder is about to read the wrong columns."
        );
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
