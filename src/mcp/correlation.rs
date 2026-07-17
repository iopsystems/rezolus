use metriken_query::{MatrixSample, MetricsSource, QueryResult};
use rayon::prelude::*;
use std::collections::HashMap;

// Fixed time intervals optimized for system performance analysis
// Balances coverage with computational efficiency
const TIME_LAGS: &[i32] = &[
    // Immediate effects (key correlations happen here)
    1, 2, 3, 5, 10, // Short-term effects (every 5s)
    15, 20, 25, 30, // Medium-term effects (every 10s)
    40, 50, 60, // Longer-term effects (every 30s)
    90, 120, 150, 180, 210, 240, 270, 300,
];

/// Overall-correlation summary tuple: `(max_r, optimal_lag_s, sample_count,
/// r_band)`. Collected per series pair, then reduced to the strongest |r|.
type CorrSummary = (f64, i64, usize, Option<(f64, f64)>);

#[derive(Debug, Clone)]
pub struct CorrelationResult {
    pub metric1: String,
    pub metric2: String,
    pub metric1_name: Option<String>, // Human-readable name
    pub metric2_name: Option<String>, // Human-readable name
    pub max_correlation: f64,
    /// Measurement-uncertainty range of `max_correlation` from input value bands
    /// (rate/histogram acquisition uncertainty). Every value in the range is an
    /// achievable Pearson r given the boxes; `None` when inputs carry no bands.
    pub r_band: Option<(f64, f64)>,
    pub optimal_lag: i64, // Lag in seconds (positive = metric2 lags metric1)
    pub sample_count: usize,
    pub correlations_at_lag: Vec<LagCorrelation>,
    pub series_pairs: Vec<SeriesCorrelation>,
}

#[derive(Debug, Clone)]
pub struct LagCorrelation {
    pub lag: i64,
    pub correlation: f64,
}

#[derive(Debug, Clone)]
pub struct SeriesCorrelation {
    pub labels1: HashMap<String, String>,
    pub labels2: HashMap<String, String>,
    pub max_correlation: f64,
    pub r_band: Option<(f64, f64)>,
    pub optimal_lag: i64,
    pub sample_count: usize,
}

/// Calculate cross-correlation between two PromQL expressions
///
/// IMPORTANT: Counter metrics must be wrapped in rate() or irate() functions
/// for meaningful correlation analysis. Raw counter values are monotonically
/// increasing and will show spurious correlations.
///
/// This handles various cases:
/// - Simple metrics: `cpu_usage` vs `memory_used` (for gauges)
/// - Rate queries: `irate(cpu_cycles[5m])` vs `irate(instructions[5m])` (for counters)
/// - Aggregations: `sum by (name) (irate(cgroup_cpu_usage[5m]))` vs `sum by (id) (irate(cpu_usage[5m]))`
/// - Complex expressions: `cpu_usage / cpu_total` vs `memory_used / memory_total`
///
/// It also detects lag relationships - for example, if memory pressure leads to
/// increased CPU usage due to garbage collection after a delay.
///
/// The time range and step are determined automatically from the underlying TSDB.
///
/// Note: Use describe_metrics tool to identify counter vs gauge metrics.
pub fn calculate_correlation(
    data: &dyn MetricsSource,
    expr1: &str,
    expr2: &str,
) -> Result<CorrelationResult, Box<dyn std::error::Error>> {
    let (start, end) = data.time_range().unwrap_or((0.0, 0.0));
    let step = data.interval();

    let result1 = data.query_range(expr1, start, end, step)?;
    let result2 = data.query_range(expr2, start, end, step)?;

    let samples1 = extract_matrix_samples(&result1)?;
    let samples2 = extract_matrix_samples(&result2)?;

    if samples1.is_empty() || samples2.is_empty() {
        return Err("No data returned from queries".into());
    }

    let series_results: Vec<_> = samples1
        .par_iter()
        .flat_map(|s1| {
            samples2.par_iter().filter_map(move |s2| {
                calculate_series_cross_correlation(s1, s2, step).map(|corr| {
                    (
                        SeriesCorrelation {
                            labels1: s1.metric.clone(),
                            labels2: s2.metric.clone(),
                            max_correlation: corr.max_correlation,
                            r_band: corr.r_band,
                            optimal_lag: corr.optimal_lag,
                            sample_count: corr.sample_count,
                        },
                        (
                            corr.max_correlation,
                            corr.optimal_lag,
                            corr.sample_count,
                            corr.r_band,
                        ),
                    )
                })
            })
        })
        .collect();

    let mut series_pairs: Vec<SeriesCorrelation> = Vec::new();
    let mut all_correlations: Vec<CorrSummary> = Vec::new();

    for (series_corr, corr_tuple) in series_results {
        series_pairs.push(series_corr);
        all_correlations.push(corr_tuple);
    }

    if all_correlations.is_empty() {
        return Err("No valid correlations found (insufficient overlapping data)".into());
    }

    // Best overall correlation = highest absolute value
    let (max_correlation, optimal_lag, total_samples, r_band) = if all_correlations.len() == 1 {
        all_correlations[0]
    } else {
        all_correlations
            .iter()
            .max_by(|a, b| {
                a.0.abs()
                    .partial_cmp(&b.0.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
            .unwrap_or((0.0, 0, 0, None))
    };

    let correlations_at_lag = if samples1.len() == 1 && samples2.len() == 1 {
        calculate_lag_correlations(&samples1[0], &samples2[0], step)
    } else {
        vec![]
    };

    series_pairs.sort_by(|a, b| {
        b.max_correlation
            .abs()
            .partial_cmp(&a.max_correlation.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(CorrelationResult {
        metric1: expr1.to_string(),
        metric2: expr2.to_string(),
        metric1_name: Some(expr1.to_string()),
        metric2_name: Some(expr2.to_string()),
        max_correlation,
        r_band,
        optimal_lag,
        sample_count: total_samples,
        correlations_at_lag,
        series_pairs,
    })
}

/// Calculate cross-correlation between two specific time series
fn calculate_series_cross_correlation(
    series1: &MatrixSample,
    series2: &MatrixSample,
    step: f64,
) -> Option<CrossCorrelationResult> {
    let data1 = prepare_series_data(series1);
    let data2 = prepare_series_data(series2);

    if data1.len() < 10 || data2.len() < 10 {
        return None; // Need sufficient data for meaningful correlation
    }

    // Calculate the maximum lag based on recording duration
    let series_len = data1.len().min(data2.len());
    let duration_seconds = series_len as f64 * step;

    // Determine max lag to test:
    // - Start with the highest configured lag (e.g., 300s from TIME_LAGS)
    // - Cap at 1/3 of recording duration (for statistical validity)
    let max_configured_lag = *TIME_LAGS.last().unwrap_or(&300) as f64;
    let one_third_duration = duration_seconds / 3.0;

    // Take the smaller of: configured max or 1/3 duration
    let max_lag_seconds = max_configured_lag.min(one_third_duration);

    let max_lag = ((max_lag_seconds / step) as usize).max(1);

    // Find optimal lag using cross-correlation
    // For efficiency, we sample lags rather than testing every single one
    let mut best_correlation: f64 = 0.0;
    let mut best_lag = 0i64;
    let mut best_lag_samples = 0i64;
    let mut best_sample_count = 0;

    // Use a consistent set of time lags regardless of sampling rate
    // This ensures comparable results across different data resolutions
    let mut samples = vec![0]; // Always test zero lag

    // Convert time points to lag samples based on step size
    for &seconds in TIME_LAGS {
        let lag = (seconds as f64 / step) as i64; // Truncate towards zero
        if lag > 0 && lag <= max_lag as i64 {
            samples.push(lag);
            samples.push(-lag);
        }
    }

    // Sort and deduplicate (handles cases where rounding creates duplicates)
    samples.sort();
    samples.dedup();
    let lag_samples = samples;

    // Calculate correlations for all lags in parallel
    let lag_correlations: Vec<(i64, f64, usize)> = lag_samples
        .into_par_iter()
        .filter_map(|lag| {
            calculate_correlation_at_lag(&data1, &data2, lag)
                .map(|(corr, count)| (lag, corr, count))
        })
        .collect();

    for (lag, corr, count) in lag_correlations {
        if corr.abs() > best_correlation.abs() {
            best_correlation = corr;
            best_lag = lag * step as i64; // Convert to seconds
            best_lag_samples = lag;
            best_sample_count = count;
        }
    }

    // Need at least 5 overlapping points for valid correlation
    if best_sample_count < 5 {
        return None;
    }

    // Measurement-uncertainty band on r: only when both series carry bands.
    let r_band = match (prepare_series_boxes(series1), prepare_series_boxes(series2)) {
        (Some(bx), Some(by)) => correlation_band_at_lag(&bx, &by, best_lag_samples),
        _ => None,
    };

    Some(CrossCorrelationResult {
        max_correlation: best_correlation,
        optimal_lag: best_lag,
        sample_count: best_sample_count,
        r_band,
    })
}

#[derive(Debug, Clone)]
struct CrossCorrelationResult {
    max_correlation: f64,
    optimal_lag: i64,
    sample_count: usize,
    r_band: Option<(f64, f64)>,
}

/// Prepare time series data by normalizing and detrending
fn prepare_series_data(series: &MatrixSample) -> Vec<f64> {
    let values: Vec<f64> = series.values.iter().map(|(_, v)| *v).collect();

    if values.is_empty() {
        return values;
    }

    // Remove linear trend (detrending)
    let detrended = detrend(&values);

    // Normalize to zero mean and unit variance
    normalize(&detrended)
}

/// Remove linear trend from data
fn detrend(data: &[f64]) -> Vec<f64> {
    let n = data.len() as f64;
    if n < 2.0 {
        return data.to_vec();
    }

    // Linear regression coefficients
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xx = 0.0;
    let mut sum_xy = 0.0;

    for (i, &y) in data.iter().enumerate() {
        let x = i as f64;
        sum_x += x;
        sum_y += y;
        sum_xx += x * x;
        sum_xy += x * y;
    }

    let slope = (n * sum_xy - sum_x * sum_y) / (n * sum_xx - sum_x * sum_x);
    let intercept = (sum_y - slope * sum_x) / n;

    data.iter()
        .enumerate()
        .map(|(i, &y)| y - (slope * i as f64 + intercept))
        .collect()
}

/// Normalize data to zero mean and unit variance
fn normalize(data: &[f64]) -> Vec<f64> {
    if data.is_empty() {
        return vec![];
    }

    let mean: f64 = data.iter().sum::<f64>() / data.len() as f64;
    let variance: f64 = data.iter().map(|&x| (x - mean).powi(2)).sum::<f64>() / data.len() as f64;

    let std_dev = variance.sqrt();

    if std_dev < 1e-10 {
        // Constant series
        return vec![0.0; data.len()];
    }

    data.iter().map(|&x| (x - mean) / std_dev).collect()
}

/// Calculate correlation at a specific lag
fn calculate_correlation_at_lag(data1: &[f64], data2: &[f64], lag: i64) -> Option<(f64, usize)> {
    let n1 = data1.len();
    let n2 = data2.len();

    // Determine overlap based on lag
    let (start1, end1, start2, end2) = if lag >= 0 {
        // data2 lags behind data1
        let lag_idx = lag as usize;
        if lag_idx >= n1 {
            return None;
        }
        (lag_idx, n1, 0, (n2).min(n1 - lag_idx))
    } else {
        // data1 lags behind data2
        let lag_idx = (-lag) as usize;
        if lag_idx >= n2 {
            return None;
        }
        (0, (n1).min(n2 - lag_idx), lag_idx, n2)
    };

    let slice1 = &data1[start1..end1];
    let slice2 = &data2[start2..end2];

    let overlap_len = slice1.len().min(slice2.len());
    if overlap_len < 3 {
        return None;
    }

    // Calculate Pearson correlation on the overlapping segments
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xx = 0.0;
    let mut sum_yy = 0.0;
    let mut sum_xy = 0.0;

    for i in 0..overlap_len {
        let x = slice1[i];
        let y = slice2[i];
        sum_x += x;
        sum_y += y;
        sum_xx += x * x;
        sum_yy += y * y;
        sum_xy += x * y;
    }

    let n = overlap_len as f64;
    let numerator = n * sum_xy - sum_x * sum_y;
    let denominator = ((n * sum_xx - sum_x * sum_x) * (n * sum_yy - sum_y * sum_y)).sqrt();

    if denominator < 1e-10 {
        // One or both series are constant in this window
        Some((0.0, overlap_len))
    } else {
        Some((numerator / denominator, overlap_len))
    }
}

/// Detrended value boxes for a series: `(nominal, lo, hi)` per point, or `None`
/// when the series carries no measurement bands. The linear trend is estimated
/// from the **nominal** values and subtracted from all three (a fixed shift that
/// preserves each band's width), matching the detrend the nominal correlation
/// uses so the box's nominal r equals the reported r.
fn prepare_series_boxes(series: &MatrixSample) -> Option<Vec<(f64, f64, f64)>> {
    let intervals = series.intervals.as_ref()?;
    if intervals.len() != series.values.len() || series.values.len() < 2 {
        return None;
    }
    let nominal: Vec<f64> = series.values.iter().map(|(_, v)| *v).collect();
    let n = nominal.len() as f64;

    // Linear trend from nominal (same as `detrend`).
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xx = 0.0;
    let mut sum_xy = 0.0;
    for (i, &y) in nominal.iter().enumerate() {
        let x = i as f64;
        sum_x += x;
        sum_y += y;
        sum_xx += x * x;
        sum_xy += x * y;
    }
    let denom = n * sum_xx - sum_x * sum_x;
    let slope = if denom.abs() < 1e-12 {
        0.0
    } else {
        (n * sum_xy - sum_x * sum_y) / denom
    };
    let intercept = (sum_y - slope * sum_x) / n;

    Some(
        nominal
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let trend = slope * i as f64 + intercept;
                let (lo, hi) = intervals[i];
                (v - trend, lo - trend, hi - trend)
            })
            .collect(),
    )
}

/// Slice both series' boxes to the overlap at `lag` (in samples) and bound the
/// Pearson r over the boxes. Mirrors `calculate_correlation_at_lag`'s overlap.
fn correlation_band_at_lag(
    x: &[(f64, f64, f64)],
    y: &[(f64, f64, f64)],
    lag: i64,
) -> Option<(f64, f64)> {
    let (n1, n2) = (x.len(), y.len());
    let (s1, e1, s2, e2) = if lag >= 0 {
        let li = lag as usize;
        if li >= n1 {
            return None;
        }
        (li, n1, 0, n2.min(n1 - li))
    } else {
        let li = (-lag) as usize;
        if li >= n2 {
            return None;
        }
        (0, n1.min(n2 - li), li, n2)
    };
    let sx = &x[s1..e1];
    let sy = &y[s2..e2];
    let m = sx.len().min(sy.len());
    if m < 3 {
        return None;
    }
    correlation_band(&sx[..m], &sy[..m])
}

/// Bound Pearson r over box-constrained points (measurement-uncertainty bands).
///
/// Each aligned point is `(nominal, lo, hi)` — the value is known only to lie in
/// `[lo, hi]`. Returns the `(r_min, r_max)` range of Pearson correlation
/// achievable as every point independently varies within its box, found by
/// greedy corner coordinate-search from several seeds and folded together with
/// the nominal r. Because corners are valid in-box points and the box is
/// connected, the whole returned range is genuinely *achievable* (a subset of
/// the true range — never an over-claim of tightness). Returns `None` when
/// there are too few points or the series is constant (undefined correlation).
fn correlation_band(x: &[(f64, f64, f64)], y: &[(f64, f64, f64)]) -> Option<(f64, f64)> {
    let n = x.len();
    if n < 3 || y.len() != n {
        return None;
    }

    // Running Pearson state over the currently chosen point values.
    struct State {
        n: f64,
        sx: f64,
        sy: f64,
        sxx: f64,
        syy: f64,
        sxy: f64,
        xs: Vec<f64>,
        ys: Vec<f64>,
    }
    impl State {
        fn new(xs: Vec<f64>, ys: Vec<f64>) -> Self {
            let mut s = State {
                n: xs.len() as f64,
                sx: 0.0,
                sy: 0.0,
                sxx: 0.0,
                syy: 0.0,
                sxy: 0.0,
                xs,
                ys,
            };
            for i in 0..s.xs.len() {
                let (xi, yi) = (s.xs[i], s.ys[i]);
                s.sx += xi;
                s.sy += yi;
                s.sxx += xi * xi;
                s.syy += yi * yi;
                s.sxy += xi * yi;
            }
            s
        }
        // Pearson r for the current assignment; None when a series is constant.
        fn r(&self) -> Option<f64> {
            let num = self.n * self.sxy - self.sx * self.sy;
            let den = ((self.n * self.sxx - self.sx * self.sx)
                * (self.n * self.syy - self.sy * self.sy))
                .sqrt();
            (den > 1e-12).then_some(num / den)
        }
        // Set point i to (xc, yc), updating running sums in O(1).
        fn set(&mut self, i: usize, xc: f64, yc: f64) {
            let (xo, yo) = (self.xs[i], self.ys[i]);
            self.sx += xc - xo;
            self.sy += yc - yo;
            self.sxx += xc * xc - xo * xo;
            self.syy += yc * yc - yo * yo;
            self.sxy += xc * yc - xo * yo;
            self.xs[i] = xc;
            self.ys[i] = yc;
        }
    }

    // Greedy corner coordinate-search: from the given seed, repeatedly move each
    // point to the box corner that best improves r in `dir` (+1 max, -1 min),
    // until a full pass makes no change. Returns the best achievable r.
    let optimize = |seed: &[(f64, f64)], dir: f64| -> Option<f64> {
        let xs: Vec<f64> = seed.iter().map(|&(a, _)| a).collect();
        let ys: Vec<f64> = seed.iter().map(|&(_, b)| b).collect();
        let mut st = State::new(xs, ys);
        let mut best = st.r()?;
        loop {
            let mut improved = false;
            for i in 0..n {
                let (_, xlo, xhi) = x[i];
                let (_, ylo, yhi) = y[i];
                let (cur_x, cur_y) = (st.xs[i], st.ys[i]);
                let mut local_best = best;
                let mut local_pick = (cur_x, cur_y);
                for &xc in &[xlo, xhi] {
                    for &yc in &[ylo, yhi] {
                        st.set(i, xc, yc);
                        if let Some(r) = st.r() {
                            if r * dir > local_best * dir {
                                local_best = r;
                                local_pick = (xc, yc);
                            }
                        }
                    }
                }
                st.set(i, local_pick.0, local_pick.1);
                if local_pick != (cur_x, cur_y) {
                    best = local_best;
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
        Some(best)
    };

    // Seeds spanning the box: nominal, both diagonals (push +r), and the
    // cross corners (push -r). Search from each to escape local optima.
    let nominal: Vec<(f64, f64)> = x.iter().zip(y).map(|(&(a, ..), &(b, ..))| (a, b)).collect();
    let lo_lo: Vec<(f64, f64)> = x
        .iter()
        .zip(y)
        .map(|(&(_, a, _), &(_, b, _))| (a, b))
        .collect();
    let hi_hi: Vec<(f64, f64)> = x.iter().zip(y).map(|(&(.., a), &(.., b))| (a, b)).collect();
    let lo_hi: Vec<(f64, f64)> = x
        .iter()
        .zip(y)
        .map(|(&(_, a, _), &(.., b))| (a, b))
        .collect();
    let hi_lo: Vec<(f64, f64)> = x
        .iter()
        .zip(y)
        .map(|(&(.., a), &(_, b, _))| (a, b))
        .collect();
    let seeds = [&nominal, &lo_lo, &hi_hi, &lo_hi, &hi_lo];

    // Nominal r is always achievable, so fold it in to guarantee containment.
    let r_nominal = State::new(
        nominal.iter().map(|&(a, _)| a).collect(),
        nominal.iter().map(|&(_, b)| b).collect(),
    )
    .r()?;

    let mut r_min = r_nominal;
    let mut r_max = r_nominal;
    for seed in seeds {
        if let Some(r) = optimize(seed, 1.0) {
            r_max = r_max.max(r);
        }
        if let Some(r) = optimize(seed, -1.0) {
            r_min = r_min.min(r);
        }
    }
    // Correlation is bounded to [-1, 1]; clamp away tiny numerical overshoot.
    Some((r_min.max(-1.0), r_max.min(1.0)))
}

/// Calculate correlations at multiple lags for display
fn calculate_lag_correlations(
    series1: &MatrixSample,
    series2: &MatrixSample,
    step: f64,
) -> Vec<LagCorrelation> {
    let data1 = prepare_series_data(series1);
    let data2 = prepare_series_data(series2);

    if data1.len() < 10 || data2.len() < 10 {
        return vec![];
    }

    // Use same duration-based lag calculation as main correlation
    let series_len = data1.len().min(data2.len());
    let duration_seconds = series_len as f64 * step;

    let max_configured_lag = *TIME_LAGS.last().unwrap_or(&0) as f64;

    let max_lag_seconds = max_configured_lag.min(duration_seconds / 3.0);
    let max_lag = ((max_lag_seconds / step) as usize).max(1);

    // Show all the TIME_LAGS we actually tested - no need to filter
    let mut lag_samples = vec![0]; // Always include zero lag

    for &seconds in TIME_LAGS {
        let lag = (seconds as f64 / step) as i64;
        if lag > 0 && lag <= max_lag as i64 {
            lag_samples.push(lag);
            lag_samples.push(-lag);
        }
    }

    lag_samples.sort();
    lag_samples.dedup();

    lag_samples
        .into_par_iter()
        .filter_map(|lag| {
            calculate_correlation_at_lag(&data1, &data2, lag).map(|(corr, _)| LagCorrelation {
                lag: lag * step as i64,
                correlation: corr,
            })
        })
        .collect()
}

/// Extract matrix samples from a query result
fn extract_matrix_samples(
    result: &QueryResult,
) -> Result<Vec<MatrixSample>, Box<dyn std::error::Error>> {
    match result {
        QueryResult::Matrix { result } => Ok(result.clone()),
        QueryResult::Vector { result } => {
            // Convert vector to single-sample matrix
            Ok(result
                .iter()
                .map(|s| MatrixSample {
                    metric: s.metric.clone(),
                    values: vec![s.value],
                    intervals: s.interval.map(|iv| vec![iv]),
                })
                .collect())
        }
        QueryResult::Scalar { result } => {
            // Convert scalar to single-sample matrix
            Ok(vec![MatrixSample {
                metric: HashMap::new(),
                values: vec![*result],
                intervals: None,
            }])
        }
        QueryResult::HistogramHeatmap { .. } => {
            // Histogram heatmap data cannot be converted to matrix samples
            Err(
                "Histogram heatmap data is not suitable for correlation analysis. \
                Use histogram_quantiles() instead."
                    .into(),
            )
        }
    }
}

/// Format correlation result for display
pub fn format_correlation_result(result: &CorrelationResult) -> String {
    let mut output = String::new();

    // Use human-readable names if available, otherwise fall back to queries
    let display1 = result.metric1_name.as_ref().unwrap_or(&result.metric1);
    let display2 = result.metric2_name.as_ref().unwrap_or(&result.metric2);

    output.push_str(&format!(
        "Cross-Correlation Analysis\n\
         ==========================\n\
         Metric 1: {display1}\n"
    ));

    // If we have a name, also show the query
    if result.metric1_name.is_some() {
        output.push_str(&format!("  Query: {}\n", result.metric1));
    }

    output.push_str(&format!("Metric 2: {display2}\n"));

    if result.metric2_name.is_some() {
        output.push_str(&format!("  Query: {}\n", result.metric2));
    }

    output.push_str(&format!("\nMax correlation: {:.4}", result.max_correlation));
    if let Some((lo, hi)) = result.r_band {
        // Achievable range of r given measurement-uncertainty bands on the inputs.
        output.push_str(&format!(" [{lo:.4}, {hi:.4}]"));
    }
    output.push_str(&format!("\nOptimal lag: {} seconds\n", result.optimal_lag));

    if result.optimal_lag > 0 {
        output.push_str(&format!(
            "  → {} leads {} by {} seconds\n",
            display1,
            display2,
            result.optimal_lag.abs()
        ));
    } else if result.optimal_lag < 0 {
        output.push_str(&format!(
            "  → {} leads {} by {} seconds\n",
            display2,
            display1,
            result.optimal_lag.abs()
        ));
    } else {
        output.push_str("  → No lag detected (synchronous correlation)\n");
    }

    output.push_str(&format!(
        "Sample pairs: {}\n\
         Interpretation: {}\n",
        result.sample_count,
        interpret_correlation(result.max_correlation)
    ));

    if !result.correlations_at_lag.is_empty() {
        output.push_str("\nCorrelation at different lags:\n");
        for lag_corr in &result.correlations_at_lag {
            output.push_str(&format!(
                "  Lag {:+3}s: {:.4}\n",
                lag_corr.lag, lag_corr.correlation
            ));
        }
    }

    // Always show series details to identify which specific series correlated
    if !result.series_pairs.is_empty() {
        // Check if we have complex expressions
        let expr1_is_complex = result.metric1.contains('/')
            || result.metric1.contains('*')
            || result.metric1.contains('+')
            || result.metric1.contains('-');
        let expr2_is_complex = result.metric2.contains('/')
            || result.metric2.contains('*')
            || result.metric2.contains('+')
            || result.metric2.contains('-');

        if expr1_is_complex || expr2_is_complex {
            output.push_str("\nNote: Complex expressions used in correlation\n");
            if expr1_is_complex {
                output.push_str(&format!("  Expression 1: {}\n", result.metric1));
            }
            if expr2_is_complex {
                output.push_str(&format!("  Expression 2: {}\n", result.metric2));
            }
        }

        output.push_str(&format!(
            "\nSeries-level correlations ({} pair{}):\n",
            result.series_pairs.len(),
            if result.series_pairs.len() == 1 {
                ""
            } else {
                "s"
            }
        ));

        // Show top correlations (or the single correlation if only one)
        let show_count = 10.min(result.series_pairs.len()); // Show up to 10 for better visibility

        for (i, pair) in result.series_pairs.iter().take(show_count).enumerate() {
            let band = pair
                .r_band
                .map(|(lo, hi)| format!(" [{lo:.4}, {hi:.4}]"))
                .unwrap_or_default();
            output.push_str(&format!(
                "\n{}. r={:.4}{} at lag={}s (n={})",
                i + 1,
                pair.max_correlation,
                band,
                pair.optimal_lag,
                pair.sample_count
            ));

            // Format labels compactly with deterministic ordering
            let format_labels = |labels: &HashMap<String, String>| -> String {
                let metric_name = labels
                    .get("metric")
                    .or_else(|| labels.get("__name__"))
                    .map(|s| s.as_str());

                // Labels to omit from the label selector
                let omit_labels = ["metric", "metric_type", "unit", "__name__"];

                let mut label_parts = Vec::new();

                // 'id' goes first if present
                if let Some(id_value) = labels.get("id") {
                    label_parts.push(format!("id=\"{id_value}\""));
                }

                let mut remaining_labels: Vec<(String, String)> = labels
                    .iter()
                    .filter(|(k, _)| k.as_str() != "id" && !omit_labels.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                remaining_labels.sort_by(|a, b| a.0.cmp(&b.0));

                for (k, v) in remaining_labels {
                    label_parts.push(format!("{k}=\"{v}\""));
                }

                // Format based on whether we have a metric name
                match metric_name {
                    Some(name)
                        if !name.is_empty()
                            && name != "division_result"
                            && name != "multiplication_result" =>
                    {
                        // Normal metric with a proper name
                        if label_parts.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}{{{}}}", name, label_parts.join(","))
                        }
                    }
                    _ => {
                        // Complex expression or unknown metric - just show labels
                        if label_parts.is_empty() {
                            "{}".to_string()
                        } else {
                            format!("{{{}}}", label_parts.join(","))
                        }
                    }
                }
            };

            output.push_str(&format!(
                "\n   {} vs {}",
                format_labels(&pair.labels1),
                format_labels(&pair.labels2)
            ));
        }

        if result.series_pairs.len() > show_count {
            output.push_str(&format!(
                "\n... and {} more pairs",
                result.series_pairs.len() - show_count
            ));
        }
    }

    if result.r_band.is_some() || result.series_pairs.iter().any(|p| p.r_band.is_some()) {
        output.push_str(
            "\n[lo, hi] after r is the correlation's measurement-uncertainty range: the span of\n\
             Pearson r still achievable given the input value bands (rate/histogram acquisition\n\
             uncertainty). A wide range means the correlation is measurement-limited.\n",
        );
    }
    output.push_str(
        "\nNote: Data has been detrended and normalized to detect non-linear relationships.\n",
    );
    output.push_str(
        "Positive lag means metric 2 follows metric 1 (metric 1 is leading indicator).\n",
    );

    output
}

fn interpret_correlation(r: f64) -> &'static str {
    let abs_r = r.abs();
    if abs_r >= 0.9 {
        if r > 0.0 {
            "Very strong positive correlation"
        } else {
            "Very strong negative correlation"
        }
    } else if abs_r >= 0.7 {
        if r > 0.0 {
            "Strong positive correlation"
        } else {
            "Strong negative correlation"
        }
    } else if abs_r >= 0.5 {
        if r > 0.0 {
            "Moderate positive correlation"
        } else {
            "Moderate negative correlation"
        }
    } else if abs_r >= 0.3 {
        if r > 0.0 {
            "Weak positive correlation"
        } else {
            "Weak negative correlation"
        }
    } else {
        "Very weak or no correlation"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Plain Pearson r over aligned nominal slices, for asserting containment.
    fn pearson(x: &[f64], y: &[f64]) -> f64 {
        let n = x.len() as f64;
        let sx: f64 = x.iter().sum();
        let sy: f64 = y.iter().sum();
        let sxx: f64 = x.iter().map(|v| v * v).sum();
        let syy: f64 = y.iter().map(|v| v * v).sum();
        let sxy: f64 = x.iter().zip(y).map(|(a, b)| a * b).sum();
        (n * sxy - sx * sy) / ((n * sxx - sx * sx) * (n * syy - sy * sy)).sqrt()
    }

    // Build (nominal, lo, hi) boxes with a symmetric half-width around nominal.
    fn boxes(vals: &[f64], hw: f64) -> Vec<(f64, f64, f64)> {
        vals.iter().map(|&v| (v, v - hw, v + hw)).collect()
    }

    #[test]
    fn zero_width_bands_collapse_to_nominal_r() {
        let xv = [1.0, 2.0, 3.0, 4.0, 5.0];
        let yv = [1.0, 2.0, 3.0, 4.0, 5.0];
        let x = boxes(&xv, 0.0);
        let y = boxes(&yv, 0.0);
        let (lo, hi) = correlation_band(&x, &y).expect("band");
        let r = pearson(&xv, &yv);
        assert!((lo - r).abs() < 1e-9, "lo {lo} == nominal {r}");
        assert!((hi - r).abs() < 1e-9, "hi {hi} == nominal {r}");
    }

    #[test]
    fn wide_bands_widen_and_contain_nominal() {
        // Perfectly correlated nominally (r = 1), but wide bands on y let the
        // correlation drop well below 1. r can never exceed 1.
        let xv = [1.0, 2.0, 3.0, 4.0, 5.0];
        let yv = [1.0, 2.0, 3.0, 4.0, 5.0];
        let x = boxes(&xv, 0.0);
        let y = boxes(&yv, 2.0);
        let (lo, hi) = correlation_band(&x, &y).expect("band");
        let r = pearson(&xv, &yv); // 1.0
        assert!(
            lo <= r + 1e-9 && r <= hi + 1e-9,
            "nominal {r} in [{lo}, {hi}]"
        );
        assert!(lo < r - 0.05, "wide bands drop r below nominal: lo={lo}");
        assert!(hi <= 1.0 + 1e-9, "r cannot exceed 1: hi={hi}");
    }

    #[test]
    fn bands_can_lift_negative_correlation() {
        // Anti-correlated nominally (r = -1); wide bands let r rise toward 0+.
        let xv = [1.0, 2.0, 3.0, 4.0, 5.0];
        let yv = [5.0, 4.0, 3.0, 2.0, 1.0];
        let x = boxes(&xv, 0.0);
        let y = boxes(&yv, 3.0);
        let (lo, hi) = correlation_band(&x, &y).expect("band");
        let r = pearson(&xv, &yv); // -1.0
        assert!(
            lo <= r + 1e-9 && r <= hi + 1e-9,
            "nominal {r} in [{lo}, {hi}]"
        );
        assert!(hi > r + 0.1, "wide bands lift r above -1: hi={hi}");
        assert!(lo >= -1.0 - 1e-9, "r cannot go below -1: lo={lo}");
    }

    fn ramp_sample(offset: f64, n: usize, band_hw: Option<f64>) -> MatrixSample {
        let values: Vec<(f64, f64)> = (0..n)
            .map(|i| (i as f64, offset + i as f64 + (i % 3) as f64))
            .collect();
        let intervals = band_hw.map(|hw| values.iter().map(|(_, v)| (v - hw, v + hw)).collect());
        MatrixSample {
            metric: HashMap::new(),
            values,
            intervals,
        }
    }

    #[test]
    fn series_correlation_carries_r_band_when_inputs_have_bands() {
        // Two nearly-parallel ramps → strong positive correlation.
        let s1 = ramp_sample(0.0, 15, Some(1.5));
        let s2 = ramp_sample(10.0, 15, Some(1.5));
        let corr = calculate_series_cross_correlation(&s1, &s2, 1.0).expect("correlation");
        let (lo, hi) = corr.r_band.expect("r_band present when inputs carry bands");
        assert!(
            lo <= corr.max_correlation + 1e-9 && corr.max_correlation <= hi + 1e-9,
            "nominal r={} within band [{lo}, {hi}]",
            corr.max_correlation
        );
        assert!(lo >= -1.0 - 1e-9 && hi <= 1.0 + 1e-9, "band within [-1, 1]");
    }

    #[test]
    fn series_correlation_has_no_r_band_without_input_bands() {
        let s1 = ramp_sample(0.0, 15, None);
        let s2 = ramp_sample(10.0, 15, None);
        let corr = calculate_series_cross_correlation(&s1, &s2, 1.0).expect("correlation");
        assert!(
            corr.r_band.is_none(),
            "no band emitted when inputs lack intervals"
        );
    }
}
