//! The time window of a `query` (start, end, step) and the per-series
//! summary of its result, shared by the MCP `query` tool and
//! `rezolus mcp query`.

use metriken_query::QueryResult;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// The step when none is given, in seconds.
pub(crate) const DEFAULT_STEP: f64 = 1.0;

/// The most points one series of a window may have: (end - start) / step,
/// over the part of the window the recording covers.
const MAX_POINTS: f64 = 1_000_000.0;

/// Unix seconds past this are the year 2286: milliseconds or nanoseconds
/// sent as seconds.
const MAX_UNIX_SECONDS: f64 = 1e10;

/// A query's evaluation window, in Unix seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Window {
    /// The window asked for, in Unix seconds.
    pub start: f64,
    pub end: f64,
    pub step: f64,
    /// The range the engine is asked for: the part of `start..=end` the
    /// recording covers (see [`query`]). The points kept are those from
    /// `start` to `covered_end`, both included.
    pub read_start: f64,
    pub covered_end: f64,
}

/// The window from the arguments given, with `start` and `end` defaulting
/// to the span of the recording, `range`. `step` defaults to
/// [`DEFAULT_STEP`], or to the whole number of seconds that keeps the
/// covered part of the window within [`MAX_POINTS`] points when that is
/// coarser. Refuses a non-positive step, a window that does not overlap the
/// recording, a start after the end, and a given step that makes more than
/// [`MAX_POINTS`] points.
pub(crate) fn resolve(
    range: Option<(f64, f64)>,
    start: Option<f64>,
    end: Option<f64>,
    step: Option<f64>,
) -> Result<Window, String> {
    let (first, last) = range.unwrap_or((0.0, 0.0));
    if let Some(step) = step {
        if !(step.is_finite() && step > 0.0) {
            return Err(format!(
                "step must be a positive number of seconds, not {step}"
            ));
        }
    }
    let start = start.unwrap_or(first);
    let end = end.unwrap_or(last);
    if range.is_some() && (end < first || start > last) {
        return Err(format!(
            "the window {start} to {end} does not overlap the recording, which spans \
             {first} to {last} (Unix seconds)"
        ));
    }
    if start > end {
        return Err(format!("start {start} is after end {end} (Unix seconds)"));
    }
    let (read_start, covered_end) = match range {
        Some(_) => (start.max(first), end.min(last)),
        None => (start, end),
    };
    let step =
        step.unwrap_or_else(|| DEFAULT_STEP.max(((covered_end - read_start) / MAX_POINTS).ceil()));
    if (covered_end - read_start) / step > MAX_POINTS {
        return Err(format!(
            "{start} to {end} at a step of {step} s is more than {MAX_POINTS} points; \
             use a larger step or a shorter window"
        ));
    }
    Ok(Window {
        start,
        end,
        step,
        read_start,
        covered_end,
    })
}

/// Run `query` over `window`. Points are at multiples of `step` (counted from
/// the Unix epoch) from `start` to `end`, except for a query that combines
/// samplers of different cadence, which metriken-query evaluates at the
/// slower one's rows. The engine is asked for the part of the window the
/// recording covers, `read_start` to `covered_end` (from the grid point at
/// or before `read_start` when the two are equal), and the points before
/// `start` or after `covered_end` are dropped. The engine reads the rows
/// after `covered_end` that a point at it needs (metriken-query 0.50.3):
/// each series' next row for a grid `rate`/`irate`, and for the strided
/// histogram functions (`histogram_quantiles([q], h, STEP)`, and
/// `histogram_mean`, `histogram_count`, `histogram_sum` and
/// `histogram_heatmap` with a `STEP`) the rows until the windows at the end
/// are closed, except in the cases metriken-query's crate docs list (a
/// bucket or scale change after the end, every series going two of its row
/// intervals without a row across it, and a process group's series whose
/// next row is more than 16 of the metric's row intervals after the end).
///
/// An unstrided histogram function (`histogram_quantile(q, h)` and the
/// others metriken-query lists) gives one point per row wherever the step
/// falls; the strided form (`histogram_quantiles([q], h, STEP)`) gives one
/// per step.
pub(crate) fn query(
    reader: &dyn metriken_query::MetricsSource,
    query: &str,
    window: &Window,
) -> Result<QueryResult, metriken_query::QueryError> {
    // A range of one instant is evaluated where it is asked, not on the step
    // grid: start it at the grid point at or before, and `trim` keeps the
    // point only when that is the instant itself.
    let from = if window.covered_end > window.read_start {
        window.read_start
    } else {
        (window.read_start / window.step).floor() * window.step
    };
    let mut result = reader.query_range(query, from, window.covered_end, window.step)?;
    trim(&mut result, window);
    Ok(result)
}

/// Drop the points of `result` before the window's start or after the end
/// of the part the recording covers, and the series left with none.
fn trim(result: &mut QueryResult, window: &Window) {
    // A grid point and a bound written as the same decimal can differ in
    // the last bits. A microsecond covers that at today's Unix times (f64
    // spacing there is about 0.24 us); a quarter step keeps a finer step's
    // neighbouring point out.
    let tolerance = (window.step / 4.0).min(1e-6);
    // Up to the end of the part the recording covers, the end the engine
    // is asked for.
    let keep = |t: f64| t >= window.start - tolerance && t <= window.covered_end + tolerance;
    fn retain_by<T>(v: &mut Vec<T>, kept: &[bool]) {
        let mut i = 0;
        v.retain(|_| {
            i += 1;
            kept[i - 1]
        });
    }
    match result {
        QueryResult::Matrix { result, .. } => {
            for s in result.iter_mut() {
                let kept: Vec<bool> = s.values.iter().map(|(t, _)| keep(*t)).collect();
                retain_by(&mut s.values, &kept);
                if let Some(v) = s.intervals.as_mut() {
                    retain_by(v, &kept);
                }
                if let Some(v) = s.bands.as_mut() {
                    retain_by(v, &kept);
                }
                if let Some(v) = s.interpolated.as_mut() {
                    retain_by(v, &kept);
                }
            }
            result.retain(|s| !s.values.is_empty());
        }
        QueryResult::HistogramHeatmap { result, .. } => {
            let kept: Vec<usize> = (0..result.timestamps.len())
                .filter(|&i| keep(result.timestamps[i]))
                .collect();
            let index: HashMap<usize, usize> = kept
                .iter()
                .enumerate()
                .map(|(new, old)| (*old, new))
                .collect();
            result.timestamps = kept.iter().map(|&i| result.timestamps[i]).collect();
            result.data = result
                .data
                .iter()
                .filter_map(|(t, b, v)| index.get(t).map(|t| (*t, *b, *v)))
                .collect();
            let counts = result.data.iter().map(|(_, _, v)| *v);
            result.min_value = counts.clone().fold(f64::INFINITY, f64::min);
            result.max_value = counts.fold(f64::NEG_INFINITY, f64::max);
            if result.data.is_empty() {
                (result.min_value, result.max_value) = (0.0, 0.0);
            }
        }
        QueryResult::Vector { .. } | QueryResult::Scalar { .. } => {}
    }
}

/// An instant argument of a tool: RFC 3339, or Unix seconds as a number.
/// A digit-only string is refused as ambiguous, as `add_event` refuses it.
pub(crate) fn instant_of_value(v: &Value, key: &str) -> Result<f64, String> {
    match v {
        Value::Number(n) => unix_seconds(
            n.as_f64()
                .ok_or_else(|| format!("{key} is not a finite number"))?,
            key,
        ),
        Value::String(s) if s.trim().chars().all(|c| c.is_ascii_digit()) => Err(format!(
            "{key} {s:?} is a digit-only string, which is ambiguous; send Unix seconds \
             as a number or an RFC 3339 string"
        )),
        Value::String(s) => rfc3339(s, key),
        _ => Err(format!("{key} must be an RFC 3339 string or Unix seconds")),
    }
}

/// An instant on the command line: Unix seconds (`1700000000.5`) or RFC
/// 3339.
pub(crate) fn instant_of_str(s: &str, key: &str) -> Result<f64, String> {
    match s.trim().parse::<f64>() {
        Ok(secs) => unix_seconds(secs, key),
        Err(_) => rfc3339(s, key),
    }
}

/// A step on the command line: humantime (`10s`, `1m`) or seconds.
pub(crate) fn step_of_str(s: &str) -> Result<f64, String> {
    if let Ok(secs) = s.trim().parse::<f64>() {
        return Ok(secs);
    }
    humantime::parse_duration(s.trim())
        .map(|d| d.as_secs_f64())
        .map_err(|e| format!("step {s:?} is neither seconds nor a humantime duration: {e}"))
}

fn unix_seconds(secs: f64, key: &str) -> Result<f64, String> {
    if !secs.is_finite() || !(0.0..=MAX_UNIX_SECONDS).contains(&secs) {
        return Err(format!(
            "{key} must be Unix seconds (a number under 1e10), not {secs}"
        ));
    }
    Ok(secs)
}

fn rfc3339(s: &str, key: &str) -> Result<f64, String> {
    chrono::DateTime::parse_from_rfc3339(s.trim())
        .map(|dt| dt.timestamp_millis() as f64 / 1000.0)
        .map_err(|e| format!("{key} {s:?} is not RFC 3339: {e}"))
}

/// Statistics over one series' points.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Stats {
    /// Points in the series.
    pub points: usize,
    /// Points the statistics are over: those above the threshold, or every
    /// point that is not NaN when there is none.
    pub active: usize,
    /// `None` when `active` is 0.
    pub values: Option<Values>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Values {
    pub min: f64,
    pub max: f64,
    pub mean: f64,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
}

/// The statistics of `values`, over those above `above` when it is given.
/// A percentile is the nearest rank: the smallest value with at least that
/// fraction of the active points at or below it.
pub(crate) fn stats(points: &[f64], above: Option<f64>) -> Stats {
    let mut active: Vec<f64> = points
        .iter()
        .copied()
        .filter(|v| match above {
            Some(t) => *v > t,
            None => !v.is_nan(),
        })
        .collect();
    let n = active.len();
    // Summed in the series' order, as `rezolus mcp query` summed it before
    // the percentiles needed a sorted copy.
    let sum: f64 = active.iter().sum();
    active.sort_by(f64::total_cmp);
    let values = (n > 0).then(|| {
        let rank = |p: f64| active[((p * n as f64).ceil() as usize).clamp(1, n) - 1];
        Values {
            min: active[0],
            max: active[n - 1],
            mean: sum / n as f64,
            p50: rank(0.50),
            p90: rank(0.90),
            p99: rank(0.99),
        }
    });
    Stats {
        points: points.len(),
        active: n,
        values,
    }
}

/// A value as the query JSON writes one: a number when finite, else `+Inf`,
/// `-Inf` or `NaN` as a string, as Prometheus spells them.
fn json_number(v: f64) -> Value {
    if v.is_finite() {
        json!(v)
    } else if v.is_nan() {
        json!("NaN")
    } else if v > 0.0 {
        json!("+Inf")
    } else {
        json!("-Inf")
    }
}

/// The summary of `result` over `window`: one entry per series with its
/// labels, `points`, `active` and, when `active` is not 0, `min`, `max`,
/// `mean`, `p50`, `p90` and `p99`. A histogram heatmap has no series to
/// summarize and is refused.
pub(crate) fn summary_json(
    result: &QueryResult,
    window: &Window,
    above: Option<f64>,
) -> Result<Value, String> {
    let series: Vec<(&HashMap<String, String>, Vec<f64>)> = match result {
        QueryResult::Matrix { result, .. } => result
            .iter()
            .map(|s| (&s.metric, s.values.iter().map(|(_, v)| *v).collect()))
            .collect(),
        QueryResult::Vector { result, .. } => result
            .iter()
            .map(|s| (&s.metric, vec![s.value.1]))
            .collect(),
        QueryResult::Scalar { .. } => Vec::new(),
        QueryResult::HistogramHeatmap { .. } => {
            return Err(
                "a histogram heatmap has no series to summarize; summarize a \
                        histogram_quantile() of it instead"
                    .to_string(),
            )
        }
    };
    let scalar = match result {
        QueryResult::Scalar { result, .. } => Some(vec![result.1]),
        _ => None,
    };
    let empty = HashMap::new();
    let entries: Vec<Value> = series
        .iter()
        .map(|(metric, points)| (*metric, points.as_slice()))
        .chain(scalar.as_deref().map(|p| (&empty, p)))
        .map(|(metric, points)| {
            let s = stats(points, above);
            let mut entry = Map::new();
            entry.insert("metric".into(), json!(metric));
            entry.insert("points".into(), json!(s.points));
            entry.insert("active".into(), json!(s.active));
            if let Some(v) = s.values {
                for (k, x) in [
                    ("min", v.min),
                    ("max", v.max),
                    ("mean", v.mean),
                    ("p50", v.p50),
                    ("p90", v.p90),
                    ("p99", v.p99),
                ] {
                    entry.insert(k.into(), json_number(x));
                }
            }
            Value::Object(entry)
        })
        .collect();

    let mut out = Map::new();
    out.insert("resultType".into(), json!("summary"));
    out.insert("start".into(), json!(window.start));
    out.insert("end".into(), json!(window.end));
    out.insert("step".into(), json!(window.step));
    if let Some(t) = above {
        out.insert("above".into(), json!(t));
    }
    out.insert("result".into(), Value::Array(entries));
    if !result.notices().is_empty() {
        out.insert(
            "notices".into(),
            serde_json::to_value(result.notices()).map_err(|e| e.to_string())?,
        );
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_defaults_to_the_recording_at_one_second() {
        let w = resolve(Some((100.0, 200.0)), None, None, None).unwrap();
        assert_eq!((w.start, w.end, w.step), (100.0, 200.0, 1.0));
        assert_eq!((w.read_start, w.covered_end), (100.0, 200.0));
        let w = resolve(Some((100.0, 200.0)), Some(150.0), None, Some(10.0)).unwrap();
        assert_eq!((w.start, w.end, w.step), (150.0, 200.0, 10.0));
    }

    #[test]
    fn a_window_that_cannot_be_evaluated_is_refused() {
        let range = Some((100.0, 200.0));
        for (start, end, step, want) in [
            (None, None, Some(0.0), "step must be a positive"),
            (None, None, Some(-1.0), "step must be a positive"),
            (Some(160.0), Some(150.0), None, "is after end"),
            (Some(300.0), Some(400.0), None, "does not overlap"),
            (Some(10.0), Some(20.0), None, "does not overlap"),
            (None, None, Some(1e-5), "points"),
            (Some(300.0), None, None, "does not overlap"),
            (None, Some(50.0), None, "does not overlap"),
        ] {
            let err = resolve(range, start, end, step).unwrap_err();
            assert!(err.contains(want), "{start:?} {end:?} {step:?}: {err}");
        }
    }

    #[test]
    fn the_default_step_keeps_a_long_recording_within_the_point_limit() {
        let day = 86_400.0;
        let w = resolve(Some((0.0, 30.0 * day)), None, None, None).unwrap();
        assert_eq!(w.step, 3.0);
        assert!((w.end - w.start) / w.step <= MAX_POINTS);
        // The default step of any recording is accepted, and a window of
        // exactly a million points is too.
        for last in [999_999.0, 1_000_000.0, 2_999_998.0, 2_999_999.0] {
            let w = resolve(Some((0.0, last)), None, None, None).unwrap();
            assert_eq!(w.step, (last / MAX_POINTS).ceil().max(1.0), "{last}");
        }
        assert!(resolve(Some((0.0, 3e6)), Some(0.0), Some(1e6), Some(1.0)).is_ok());
        // A step that is given is kept, and refused when too fine.
        assert!(resolve(Some((0.0, 30.0 * day)), None, None, Some(1.0)).is_err());
        assert_eq!(
            resolve(Some((0.0, day)), None, None, None).unwrap().step,
            1.0
        );
    }

    /// The read is the part of the window the recording covers, and the
    /// default step is sized from it: an end long after the recording
    /// neither coarsens the step nor moves the read past the recording's
    /// last row.
    #[test]
    fn the_read_is_the_part_of_the_window_the_recording_covers() {
        let w = resolve(Some((100.0, 115.0)), Some(50.0), Some(5e6), None).unwrap();
        assert_eq!((w.start, w.end, w.step), (50.0, 5e6, 1.0));
        assert_eq!((w.read_start, w.covered_end), (100.0, 115.0));
    }

    #[test]
    fn instants_are_unix_seconds_or_rfc_3339() {
        assert_eq!(
            instant_of_str("1700000000.5", "--start").unwrap(),
            1_700_000_000.5
        );
        assert_eq!(
            instant_of_str("2023-11-14T22:13:20Z", "--start").unwrap(),
            1_700_000_000.0
        );
        // Milliseconds sent as seconds.
        assert!(instant_of_str("1700000000000", "--start")
            .unwrap_err()
            .contains("under 1e10"));
        assert!(instant_of_str("yesterday", "--start")
            .unwrap_err()
            .contains("RFC 3339"));
        assert_eq!(
            instant_of_value(&json!(1_700_000_000.0), "start").unwrap(),
            1_700_000_000.0
        );
        assert!(instant_of_value(&json!("1700000000"), "start")
            .unwrap_err()
            .contains("digit-only"));
        assert_eq!(step_of_str("10s").unwrap(), 10.0);
        assert_eq!(step_of_str("0.5").unwrap(), 0.5);
        assert!(step_of_str("often").is_err());
    }

    #[test]
    fn percentiles_are_the_nearest_rank() {
        let points: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = stats(&points, None);
        assert_eq!((s.points, s.active), (100, 100));
        let v = s.values.unwrap();
        assert_eq!((v.min, v.max, v.mean), (1.0, 100.0, 50.5));
        assert_eq!((v.p50, v.p90, v.p99), (50.0, 90.0, 99.0));

        let v = stats(&[3.0, 1.0, 2.0], None).values.unwrap();
        assert_eq!((v.p50, v.p90, v.p99), (2.0, 3.0, 3.0));
    }

    #[test]
    fn above_keeps_only_the_points_over_the_threshold() {
        let points = [0.0, 0.0, 10.0, 12.0, 14.0, 0.0, f64::NAN];
        let s = stats(&points, Some(5.0));
        assert_eq!((s.points, s.active), (7, 3));
        let v = s.values.unwrap();
        assert_eq!((v.min, v.mean, v.max), (10.0, 12.0, 14.0));

        // The mean sums the points in the series' order: sorted, this sum
        // would lose both 1.0s to the 1e16s.
        let v = stats(&[1e16, 1.0, -1e16, 1.0], None).values.unwrap();
        assert_eq!(v.mean, 0.25);

        // Strictly above: a point equal to the threshold is not active.
        assert_eq!(stats(&[5.0, 5.0, 6.0], Some(5.0)).active, 1);
        // Without a threshold, NaN is not active.
        assert_eq!(stats(&points, None).active, 6);
        // Nothing above: no statistics.
        assert_eq!(stats(&points, Some(100.0)).values, None);
    }

    /// Points at a decimal-fraction step are kept from start to end, and
    /// the neighbouring points are not.
    #[test]
    fn trim_keeps_the_points_from_start_to_end() {
        use metriken_query::MatrixSample;
        let t0: f64 = 1_700_000_000.0;
        let step: f64 = 0.1;
        // Grid times as the engine computes them, which differ from the
        // decimal bounds in the last bits.
        let first = (t0 / step).floor();
        let grid: Vec<(f64, f64)> = (0..20)
            .map(|k| ((first + k as f64) * step, k as f64))
            .collect();
        let mut result = QueryResult::Matrix {
            result: vec![MatrixSample::new(HashMap::new(), grid)
                .with_intervals(Some((0..20).map(|k| (k as f64, k as f64)).collect()))],
            notices: Vec::new(),
        };
        let w = resolve(
            Some((t0, t0 + 2.0)),
            Some(t0 + 0.3),
            Some(t0 + 0.8),
            Some(step),
        )
        .unwrap();
        trim(&mut result, &w);
        let QueryResult::Matrix { result, .. } = result else {
            unreachable!()
        };
        let kept: Vec<f64> = result[0].values.iter().map(|(_, v)| *v).collect();
        // The point at end, t0 + 0.8, is a little above the bound.
        assert_eq!(kept, [3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        assert_eq!(result[0].intervals.as_ref().unwrap().len(), 6);
    }

    /// The tolerance is at most a microsecond: at a coarse step, a grid
    /// point seconds before start is not kept.
    #[test]
    fn trim_keeps_no_coarse_grid_point_before_start() {
        use metriken_query::MatrixSample;
        let t0: f64 = 1_700_000_040.0;
        let mut result = QueryResult::Matrix {
            result: vec![MatrixSample::new(
                HashMap::new(),
                vec![(t0, 1.0), (t0 + 60.0, 2.0), (t0 + 120.0, 3.0)],
            )],
            notices: Vec::new(),
        };
        let w = resolve(
            Some((t0, t0 + 200.0)),
            Some(t0 + 10.0),
            Some(t0 + 120.0),
            Some(60.0),
        )
        .unwrap();
        trim(&mut result, &w);
        let QueryResult::Matrix { result, .. } = result else {
            unreachable!()
        };
        let kept: Vec<f64> = result[0].values.iter().map(|(_, v)| *v).collect();
        assert_eq!(kept, [2.0, 3.0]);
    }

    /// Points past the recording's last row are not kept, however far past
    /// it the window ends.
    #[test]
    fn trim_keeps_no_point_after_the_recording() {
        use metriken_query::MatrixSample;
        let mut result = QueryResult::Matrix {
            result: vec![MatrixSample::new(
                HashMap::new(),
                (0..10).map(|k| (100.0 + k as f64, k as f64)).collect(),
            )],
            notices: Vec::new(),
        };
        let w = resolve(Some((100.0, 104.5)), None, Some(500.0), None).unwrap();
        trim(&mut result, &w);
        let QueryResult::Matrix { result, .. } = result else {
            unreachable!()
        };
        let kept: Vec<f64> = result[0].values.iter().map(|(t, _)| *t).collect();
        assert_eq!(kept, [100.0, 101.0, 102.0, 103.0, 104.0]);
    }

    /// A heatmap keeps the columns in the window, renumbered, and its value
    /// range is that of the columns kept.
    #[test]
    fn trim_renumbers_a_heatmap_and_recomputes_its_range() {
        use metriken_query::HistogramHeatmapResult;
        let mut result = QueryResult::HistogramHeatmap {
            result: HistogramHeatmapResult {
                timestamps: vec![10.0, 11.0, 12.0, 13.0],
                bucket_bounds: vec![1.0, 2.0],
                data: vec![(0, 0, 1.0), (1, 0, 3.0), (2, 1, 5.0), (3, 1, 900.0)],
                min_value: 1.0,
                max_value: 900.0,
            },
            notices: Vec::new(),
        };
        let w = resolve(Some((10.0, 13.0)), Some(11.0), Some(12.0), Some(1.0)).unwrap();
        trim(&mut result, &w);
        let QueryResult::HistogramHeatmap { result, .. } = result else {
            unreachable!()
        };
        assert_eq!(result.timestamps, [11.0, 12.0]);
        assert_eq!(result.data, [(0, 0, 3.0), (1, 1, 5.0)]);
        assert_eq!((result.min_value, result.max_value), (3.0, 5.0));
    }

    #[test]
    fn a_summary_writes_infinity_as_prometheus_does() {
        use metriken_query::MatrixSample;
        let result = QueryResult::Matrix {
            result: vec![MatrixSample::new(
                [("__name__".to_string(), "q".to_string())].into(),
                vec![(1.0, 1.0), (2.0, f64::INFINITY)],
            )],
            notices: Vec::new(),
        };
        let w = resolve(Some((1.0, 2.0)), None, None, None).unwrap();
        let s = summary_json(&result, &w, None).unwrap();
        assert_eq!(s["resultType"], "summary");
        assert_eq!(s["result"][0]["max"], "+Inf");
        assert_eq!(s["result"][0]["min"], 1.0);
        assert_eq!(s["result"][0]["metric"]["__name__"], "q");
        assert!(s.get("above").is_none());

        let s = summary_json(&result, &w, Some(5.0)).unwrap();
        assert_eq!(s["above"], 5.0);
        assert_eq!(s["result"][0]["active"], 1);

        let s = summary_json(&result, &w, Some(f64::INFINITY)).unwrap();
        assert_eq!(s["result"][0]["active"], 0);
        assert!(s["result"][0].get("mean").is_none());
    }
}
