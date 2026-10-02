//! `rezolus recording check`: evaluate the checks a recording's KPIs carry
//! and report a verdict per check.
//!
//! A check is `Kpi.check` (`crates/dashboard/src/service_extension.rs`): a
//! threshold (`above`/`below`), a minimum duration (`for`) and a severity.
//! The KPIs come from the recording's own embedded service extension, the
//! same lookup the viewer makes, unless `--queries` names a file. Each
//! check's query runs over the whole recording through the query engine
//! (`MetricsSource::query_range`, as `rezolus mcp query` runs it) and the
//! resulting series is scanned for runs of violating points.
//!
//! Where a value carries an acquisition-window band (`rate()`, `irate()`,
//! histogram quantiles), the band is what is compared: `above` fires only
//! when the whole band is above the threshold, `below` only when the whole
//! band is below it. A point whose band straddles the threshold, or that was
//! interpolated across a span the producer never read, is `indeterminate`,
//! and runs of those are reported as their own state rather than counted as
//! either pass or fail.
//!
//! `--annotate` writes each violation window into the recording as a
//! `kind = "check"` range event whose `details` carry the check JSON, so a
//! recording says what it was checked against even after the template that
//! defined the check changes. See
//! `docs/journal/2026-09-28-checks-with-verdicts.md`.

use clap::ArgMatches;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use metriken_query::{MatrixSample, MetricsSource, QueryResult};

use crate::mcp::{
    describe_candidates, render_labels, RecordingSelector, SelectError, SelectorSyntax,
};
use crate::recorder::rez::RezFormat;
use crate::viewer::{Event, ServiceExtension, TemplateRegistry};
use dashboard::{Bound, Check, Kpi, Severity};

/// Exit status when a check with `severity: fail` failed.
pub(super) const EXIT_FAIL: i32 = 1;
/// Exit status when any check could not be evaluated, or the command itself
/// failed (unreadable file, bad `--queries`, no such recording). It outranks
/// `EXIT_FAIL`: with a check unevaluated, the run's pass/fail answer is
/// incomplete.
pub(super) const EXIT_ERROR: i32 = 2;

/// A run continues across a gap of at most this many steps. Points are
/// omitted where the recording has no data (the engine emits no stale
/// point), so array-adjacent points can be far apart in time; without this
/// rule `for` would count time nobody observed.
const GAP_STEPS: f64 = 1.5;

// ─── pure evaluation ────────────────────────────────────────────────────

/// What one point of a series says about the condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PointState {
    Pass,
    Violating,
    /// The point's uncertainty band straddles the threshold, or the point
    /// is interpolated across a span the producer did not read.
    Indeterminate,
    /// NaN: the engine had nothing to say here. Ends any run.
    NoData,
}

/// Classify one point. `band` is the acquisition-window `[lo, hi]` when the
/// value carries one; without it the value is compared directly. Both
/// comparisons are strict: a value equal to the threshold passes.
pub(crate) fn classify(
    value: f64,
    band: Option<(f64, f64)>,
    interpolated: bool,
    bound: Bound,
) -> PointState {
    if value.is_nan() {
        return PointState::NoData;
    }
    if interpolated {
        return PointState::Indeterminate;
    }
    let (lo, hi) = match band {
        Some((lo, hi)) if lo.is_finite() && hi.is_finite() => (lo.min(hi), lo.max(hi)),
        _ => (value, value),
    };
    match bound {
        Bound::Above(t) => {
            if lo > t {
                PointState::Violating
            } else if hi > t {
                PointState::Indeterminate
            } else {
                PointState::Pass
            }
        }
        Bound::Below(t) => {
            if hi < t {
                PointState::Violating
            } else if lo < t {
                PointState::Indeterminate
            } else {
                PointState::Pass
            }
        }
    }
}

/// A maximal run of consecutive points in one state. `start` is the first
/// point's timestamp and `end` the last point's plus one step, both in
/// seconds since the epoch, so `end - start` is the span the run covers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Window {
    pub start: f64,
    pub end: f64,
    pub points: usize,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Windows {
    pub violating: Vec<Window>,
    pub indeterminate: Vec<Window>,
}

/// Find the runs in `states` (timestamp seconds, state), keeping those whose
/// span is at least `min_span` seconds.
///
/// A run is consecutive points that do not pass: it ends at a `Pass` or
/// `NoData` point and at a gap of more than `GAP_STEPS` steps. Its state is
/// `Violating` only if every point violates; one indeterminate point makes
/// the whole run `Indeterminate`. Splitting on the indeterminate point
/// instead would turn a long violation with an intermittent straddle into
/// short violating runs that never reach `for`, and report it as a pass; one
/// long uncertain window is the answer that loses nothing.
///
/// A run's span is its last timestamp minus its first plus `step`, so with
/// `min_span == 0` a single point is a window.
pub(crate) fn windows(states: &[(f64, PointState)], step: f64, min_span: f64) -> Windows {
    let mut out = Windows::default();
    // (every point violating so far, start, last timestamp, points)
    let mut run: Option<(bool, f64, f64, usize)> = None;

    fn flush(run: Option<(bool, f64, f64, usize)>, step: f64, min_span: f64, out: &mut Windows) {
        let Some((all_violating, start, last, points)) = run else {
            return;
        };
        let end = last + step;
        // A hair of tolerance so `for: 10s` accepts a run whose float span
        // is 9.999999999s.
        if end - start + 1e-9 < min_span {
            return;
        }
        let window = Window { start, end, points };
        if all_violating {
            out.violating.push(window);
        } else {
            out.indeterminate.push(window);
        }
    }

    let max_gap = step * GAP_STEPS + 1e-9;
    for &(ts, state) in states {
        let reported = matches!(state, PointState::Violating | PointState::Indeterminate);
        match run {
            Some((all, start, last, n)) if reported && ts - last <= max_gap => {
                run = Some((all && state == PointState::Violating, start, ts, n + 1));
            }
            _ => {
                flush(run.take(), step, min_span, &mut out);
                if reported {
                    run = Some((state == PointState::Violating, ts, ts, 1));
                }
            }
        }
    }
    flush(run, step, min_span, &mut out);
    out
}

/// The band at point `i`, if the series carries one there. `intervals` is
/// set only when every point has a band; `bands` when any does.
fn band_at(series: &MatrixSample, i: usize) -> Option<(f64, f64)> {
    series
        .intervals
        .as_ref()
        .and_then(|v| v.get(i).copied())
        .or_else(|| {
            series
                .bands
                .as_ref()
                .and_then(|b| b.get(i).copied().flatten())
        })
}

fn interpolated_at(series: &MatrixSample, i: usize) -> bool {
    series
        .interpolated
        .as_ref()
        .and_then(|v| v.get(i).copied())
        .unwrap_or(false)
}

/// The series' own point spacing: the lower median of consecutive deltas,
/// or `fallback` for a series with fewer than two points.
fn point_spacing(series: &MatrixSample, fallback: f64) -> f64 {
    let mut gaps: Vec<f64> = series
        .values
        .windows(2)
        .map(|w| w[1].0 - w[0].0)
        .filter(|g| *g > 0.0)
        .collect();
    if gaps.is_empty() {
        return fallback;
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    gaps[(gaps.len() - 1) / 2]
}

/// Scan one series against a check. `step` is the grid the series was
/// evaluated on; the run step is that or the series' own spacing, whichever
/// is coarser. The engine emits one point per read of a slow sampler and
/// holds nothing between reads, so a 10 s gauge on a 1 s grid yields points
/// 10 s apart: each one spans its 10 s, and the 10 s between them is not a
/// gap. A hole in the data is still a gap, because the median spacing stays
/// at the sampler's cadence.
pub(crate) fn evaluate_series(series: &MatrixSample, check: &Check, step: f64) -> Windows {
    let bound = check.bound();
    let states: Vec<(f64, PointState)> = series
        .values
        .iter()
        .enumerate()
        .map(|(i, &(ts, v))| {
            (
                ts,
                classify(v, band_at(series, i), interpolated_at(series, i), bound),
            )
        })
        .collect();
    let run_step = point_spacing(series, step).max(step);
    windows(&states, run_step, check.min_duration().as_secs_f64())
}

/// The PromQL a check evaluates. A histogram KPI's query names a
/// distribution, which has no single value to compare, so the check's
/// `quantile` picks one and the query is wrapped here; every other KPI type
/// is evaluated as written, and `quantile` on one is refused rather than
/// claimed by the condition text and never applied.
pub(crate) fn check_query(kpi: &Kpi, check: &Check) -> Result<String, String> {
    if kpi.metric_type == "histogram" {
        let q = check.quantile.ok_or_else(|| {
            "a check on a histogram KPI needs `quantile` (e.g. 0.99) to say which \
             quantile the threshold applies to"
                .to_string()
        })?;
        if kpi.query.contains("histogram_quantile") {
            return Err(
                "the KPI query already applies histogram_quantile; a histogram KPI's query \
                 names the raw histogram and the check wraps it in \
                 histogram_quantile(<quantile>, ...) itself"
                    .into(),
            );
        }
        Ok(format!("histogram_quantile({q}, {})", kpi.query))
    } else {
        if check.quantile.is_some() {
            return Err(format!(
                "`quantile` applies only to a histogram KPI; this KPI's type is {:?} and its \
                 query is compared as written",
                kpi.metric_type
            ));
        }
        Ok(kpi.query.clone())
    }
}

/// The grid step for `reader`, in seconds: its sampling interval, capped at
/// the 1 s `rezolus mcp query` uses so a sub-second recording is evaluated
/// at its own resolution rather than sampled every second.
fn eval_step(reader: &dyn MetricsSource) -> f64 {
    let interval = reader.interval();
    if interval.is_finite() && interval > 0.0 {
        interval.min(1.0)
    } else {
        1.0
    }
}

/// Run a check's query over the whole recording and scan the one series it
/// must produce. Every failure is an error, including a query that matches
/// nothing: a missing metric is not a pass.
fn evaluate(reader: &dyn MetricsSource, kpi: &Kpi, check: &Check) -> Result<Windows, String> {
    let query = check_query(kpi, check)?;
    let (start, end) = reader
        .time_range()
        .ok_or_else(|| "the recording holds no samples".to_string())?;
    let step = eval_step(reader);
    let result = reader
        .query_range(&query, start, end, step)
        .map_err(|e| format!("query failed: {e}"))?;
    let series = match result {
        QueryResult::Matrix { result } => result,
        QueryResult::Vector { .. } | QueryResult::Scalar { .. } => {
            return Err("query did not produce a time series".into());
        }
        QueryResult::HistogramHeatmap { .. } => {
            return Err(
                "query produced a heatmap, not a series; set the check's `quantile`".into(),
            );
        }
    };
    match series.len() {
        0 => Err(format!(
            "no data: `{query}` matched no series in the recording's range (the metric \
             exists; check its label matchers)"
        )),
        1 => Ok(evaluate_series(&series[0], check, step)),
        n => Err(format!(
            "`{query}` produced {n} series; a check needs exactly one (aggregate, e.g. sum(...), \
             or add label matchers)"
        )),
    }
}

// ─── results and output ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    Pass,
    Warn,
    Fail,
    Indeterminate,
    Error,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
            Status::Indeterminate => "INDETERMINATE",
            Status::Error => "ERROR",
        }
    }
}

#[derive(Debug, serde::Serialize)]
struct WindowOut {
    start: String,
    end: String,
    start_ns: u64,
    duration_ns: u64,
    points: usize,
}

impl From<&Window> for WindowOut {
    fn from(w: &Window) -> Self {
        WindowOut {
            start: fmt_ts(w.start),
            end: fmt_ts(w.end),
            start_ns: secs_to_ns(w.start),
            duration_ns: secs_to_ns(w.end).saturating_sub(secs_to_ns(w.start)),
            points: w.points,
        }
    }
}

/// One check's verdict, the unit of both the text and the JSON output.
#[derive(Debug, serde::Serialize)]
pub(crate) struct CheckResult {
    /// The recording's labels, for a `.rez`; absent for a parquet file.
    #[serde(skip_serializing_if = "Option::is_none")]
    recording: Option<BTreeMap<String, String>>,
    title: String,
    /// The query as evaluated (a histogram KPI's, wrapped in
    /// `histogram_quantile`).
    query: String,
    check: Check,
    status: Status,
    windows: Vec<WindowOut>,
    indeterminate: Vec<WindowOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip)]
    raw_windows: Vec<Window>,
}

impl CheckResult {
    fn new(
        recording: Option<BTreeMap<String, String>>,
        kpi: &Kpi,
        check: &Check,
        outcome: Result<Windows, String>,
    ) -> Self {
        let query = check_query(kpi, check).unwrap_or_else(|_| kpi.query.clone());
        let (status, windows, indeterminate, error, raw) = match outcome {
            Err(e) => (Status::Error, Vec::new(), Vec::new(), Some(e), Vec::new()),
            Ok(w) => {
                // A violating window decides the verdict; indeterminate
                // windows beside it are reported, not weighed.
                let status = if !w.violating.is_empty() {
                    match check.severity {
                        Severity::Fail => Status::Fail,
                        Severity::Warn => Status::Warn,
                    }
                } else if !w.indeterminate.is_empty() {
                    Status::Indeterminate
                } else {
                    Status::Pass
                };
                (
                    status,
                    w.violating.iter().map(WindowOut::from).collect(),
                    w.indeterminate.iter().map(WindowOut::from).collect(),
                    None,
                    w.violating,
                )
            }
        };
        CheckResult {
            recording,
            title: kpi.title.clone(),
            query,
            check: check.clone(),
            status,
            windows,
            indeterminate,
            error,
            raw_windows: raw,
        }
    }

    fn line(&self) -> String {
        let prefix = self
            .recording
            .as_ref()
            .map(|l| format!("[{}] ", render_labels(l)))
            .unwrap_or_default();
        let condition = self.check.condition();
        let tail = match self.status {
            Status::Error => format!("  error: {}", self.error.as_deref().unwrap_or("")),
            Status::Pass => String::new(),
            Status::Fail | Status::Warn => {
                let mut t = describe_windows(&self.windows);
                if !self.indeterminate.is_empty() {
                    t.push_str(&format!(" (+{} indeterminate)", self.indeterminate.len()));
                }
                t
            }
            Status::Indeterminate => describe_windows(&self.indeterminate),
        };
        format!(
            "{prefix}{:<13} {}  {condition}{tail}",
            self.status.label(),
            self.title
        )
    }

    /// The events `--annotate` writes: one per violation window. The `id`
    /// is a hash of what identifies the window (title, evaluated query,
    /// condition, start), so a re-run over the same recording produces the
    /// same ids and `events::append_events` rewrites rather than duplicates.
    /// The query is in the hash because titles repeat across services by
    /// design; two KPIs with one title and different queries are two checks.
    fn events(&self) -> Vec<Event> {
        let condition = self.check.condition();
        let check_json = serde_json::to_string(&self.check).unwrap_or_default();
        self.raw_windows
            .iter()
            .map(|w| {
                let start_ns = secs_to_ns(w.start);
                Event {
                    timestamp: start_ns,
                    description: self.title.clone(),
                    kind: Some("check".to_string()),
                    details: Some(format!(
                        "{} {condition}: {}\n{check_json}",
                        self.title,
                        self.check.severity.as_str()
                    )),
                    source: None,
                    node: None,
                    instance: None,
                    labels: BTreeMap::from([(
                        "severity".to_string(),
                        self.check.severity.as_str().to_string(),
                    )]),
                    duration_ns: Some(secs_to_ns(w.end).saturating_sub(start_ns)),
                    id: Some(event_id(&self.title, &self.query, &condition, start_ns)),
                    chart_id: None,
                }
            })
            .collect()
    }
}

fn describe_windows(windows: &[WindowOut]) -> String {
    match windows.first() {
        None => String::new(),
        Some(first) => format!(
            "  [{}..{}, {} window{}]",
            first.start,
            first.end,
            windows.len(),
            if windows.len() == 1 { "" } else { "s" }
        ),
    }
}

fn secs_to_ns(secs: f64) -> u64 {
    (secs * 1e9).round().max(0.0) as u64
}

fn fmt_ts(secs: f64) -> String {
    chrono::DateTime::from_timestamp_nanos(secs_to_ns(secs) as i64)
        .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
}

/// `check:<16 hex>` over (title, query, condition, window start).
fn event_id(title: &str, query: &str, condition: &str, start_ns: u64) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in [title, query, condition] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hasher.update(start_ns.to_le_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("check:{hex}")
}

// ─── the command ────────────────────────────────────────────────────────

/// One recording to check: a parquet file (no labels, index 0) or one
/// recording of a `.rez` (its labels and catalog index).
pub(crate) struct Target {
    /// Position in the archive's catalog order, which is also the index
    /// `RezAnnotation::per_recording_events` addresses.
    pub(crate) index: usize,
    pub(crate) labels: Option<BTreeMap<String, String>>,
    pub(crate) reader: Arc<dyn MetricsSource>,
}

/// Open the recordings `--recording` names. The same readers the MCP path
/// opens (`ParquetReader` for a parquet file, `RezReader::open_recordings`
/// for a `.rez`); what differs is that with no selector a multi-recording
/// archive is checked whole, one verdict list per recording, where the MCP
/// tools would ask for a choice.
pub(crate) fn open_targets(
    path: &Path,
    selector: &RecordingSelector,
) -> Result<(RezFormat, Vec<Target>, usize), String> {
    if !path.exists() {
        return Err(format!("Recording file not found: {}", path.display()));
    }
    let pool = metriken_query::BufferPool::new(256 * 1024 * 1024);
    let format = crate::recorder::rez::detect_rez_format(path).unwrap_or(RezFormat::NotRez);
    if format == RezFormat::NotRez {
        if !selector.is_empty() {
            return Err(format!(
                "{} is not a .rez archive and holds no recordings to select between; \
                 --recording applies only to a multi-recording .rez",
                path.display()
            ));
        }
        let reader = metriken_query::ParquetReader::open_with_pool(path, pool)
            .map_err(|e| format!("failed to load {}: {e}", path.display()))?;
        return Ok((
            format,
            vec![Target {
                index: 0,
                labels: None,
                reader: Arc::new(reader),
            }],
            1,
        ));
    }

    let recordings = crate::rez_reader::RezReader::open_recordings(path, pool)
        .map_err(|e| format!("failed to load {}: {e}", path.display()))?;
    if recordings.is_empty() {
        return Err(format!("{} holds no recordings", path.display()));
    }
    let total = recordings.len();
    let all: Vec<BTreeMap<String, String>> = recordings.iter().map(|(l, _)| l.clone()).collect();
    let syntax = SelectorSyntax::Flag("--recording");
    let chosen: Vec<usize> = if selector.is_empty() {
        (0..total).collect()
    } else {
        match selector.resolve(&all) {
            Ok(i) => vec![i],
            Err(SelectError::NoMatch) => {
                return Err(format!(
                    "no recording in {} matches {}. It holds:\n{}",
                    path.display(),
                    selector.render(syntax),
                    describe_candidates(&all, &[], syntax)
                ));
            }
            Err(SelectError::Ambiguous(hits)) => {
                return Err(format!(
                    "{} matches {} recordings in {}; add labels until it names one:\n{}",
                    selector.render(syntax),
                    hits.len(),
                    path.display(),
                    describe_candidates(&all, &hits, syntax)
                ));
            }
        }
    };
    let mut targets = Vec::with_capacity(chosen.len());
    for (index, (labels, reader)) in recordings.into_iter().enumerate() {
        if chosen.contains(&index) {
            targets.push(Target {
                index,
                labels: Some(labels),
                reader: Arc::new(reader),
            });
        }
    }
    Ok((format, targets, total))
}

/// The KPIs carrying a check for one recording: from `--queries` when
/// given, else from the recording's own metadata through the viewer's
/// lookup (`service_extensions_from_metadata`).
fn checked_kpis(
    reader: &dyn MetricsSource,
    override_ext: Option<&ServiceExtension>,
    registry: &TemplateRegistry,
) -> Vec<Kpi> {
    let exts: Vec<ServiceExtension> = match override_ext {
        Some(ext) => vec![ext.clone()],
        None => crate::viewer::metadata::service_extensions_from_metadata(
            &reader.file_metadata(),
            registry,
        )
        .into_iter()
        .map(|(_, ext)| ext)
        .collect(),
    };
    exts.into_iter()
        .flat_map(|ext| ext.kpis)
        .filter(|kpi| kpi.check.is_some())
        .collect()
}

fn load_queries(path: &Path) -> Result<ServiceExtension, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {path:?}: {e}"))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("invalid service extension JSON in {path:?}: {e}"))
}

/// Returns the process exit status.
pub(super) fn run(args: &ArgMatches, registry: &TemplateRegistry) -> i32 {
    match run_inner(args, registry) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            EXIT_ERROR
        }
    }
}

/// What one evaluation produced, for the CLI to print and the MCP tool to
/// serialize.
pub(crate) struct CheckRun {
    pub(crate) results: Vec<CheckResult>,
    /// Each recording's verdict events in catalog order, one entry per
    /// recording of the archive (empty for the ones not evaluated).
    pub(crate) per_recording_events: Vec<Vec<Event>>,
    pub(crate) format: RezFormat,
    /// KPIs carrying a check, across the evaluated recordings. Zero means
    /// there was nothing to run, which is not a failure.
    pub(crate) checks_seen: usize,
}

/// Pass/warn/fail/indeterminate/error counts over a run's results.
#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub(crate) struct Summary {
    pub(crate) pass: usize,
    pub(crate) warn: usize,
    pub(crate) fail: usize,
    pub(crate) indeterminate: usize,
    pub(crate) error: usize,
}

impl CheckRun {
    pub(crate) fn summary(&self) -> Summary {
        let count = |s: Status| self.results.iter().filter(|r| r.status == s).count();
        Summary {
            pass: count(Status::Pass),
            warn: count(Status::Warn),
            fail: count(Status::Fail),
            indeterminate: count(Status::Indeterminate),
            error: count(Status::Error),
        }
    }

    /// The process exit status the CLI maps a run to: error wins over
    /// fail, which wins over everything else.
    pub(crate) fn exit_code(&self) -> i32 {
        let s = self.summary();
        if s.error > 0 {
            EXIT_ERROR
        } else if s.fail > 0 {
            EXIT_FAIL
        } else {
            0
        }
    }

    /// Why there was nothing to run, when `checks_seen` is zero. `queries`
    /// is how the caller spells its override (`--queries` on the CLI, the
    /// `queries` argument in the MCP tool).
    pub(crate) fn nothing_to_run(&self, path: &Path, had_override: bool, queries: &str) -> String {
        format!(
            "no checks to run: no KPI in {} carries a \"check\"{}",
            path.display(),
            if had_override {
                format!(" (the {queries} payload defines none)")
            } else {
                format!(" (embed one with `recording annotate --queries`, or pass {queries})")
            }
        )
    }
}

/// Evaluate every KPI check over the recordings `selector` names (all of
/// them when it is empty). Shared by `recording check` and the MCP
/// `run_checks` tool; neither prints or writes here.
pub(crate) fn run_checks(
    path: &Path,
    selector: &RecordingSelector,
    override_ext: Option<&ServiceExtension>,
    registry: &TemplateRegistry,
) -> Result<CheckRun, String> {
    let (format, targets, total_recordings) = open_targets(path, selector)?;

    let mut results: Vec<CheckResult> = Vec::new();
    let mut per_recording_events: Vec<Vec<Event>> = vec![Vec::new(); total_recordings];
    let mut checks_seen = 0usize;
    for target in &targets {
        let kpis = checked_kpis(target.reader.as_ref(), override_ext, registry);
        checks_seen += kpis.len();
        for kpi in &kpis {
            let check = kpi.check.as_ref().expect("filtered to KPIs with a check");
            let outcome = evaluate(target.reader.as_ref(), kpi, check);
            let result = CheckResult::new(target.labels.clone(), kpi, check, outcome);
            per_recording_events[target.index].extend(result.events());
            results.push(result);
        }
    }
    drop(targets);
    Ok(CheckRun {
        results,
        per_recording_events,
        format,
        checks_seen,
    })
}

fn run_inner(args: &ArgMatches, registry: &TemplateRegistry) -> Result<i32, String> {
    let path = args.get_one::<PathBuf>("FILE").expect("clap requires FILE");
    let json = args.get_flag("json");
    let annotate = args.get_flag("annotate");
    let selector = RecordingSelector::parse(
        "--recording",
        args.get_many::<String>("RECORDING")
            .map(|it| it.cloned().collect::<Vec<_>>())
            .unwrap_or_default(),
    )?;
    let override_ext = args
        .get_one::<PathBuf>("queries")
        .map(|p| load_queries(p))
        .transpose()?;

    let run = run_checks(path, &selector, override_ext.as_ref(), registry)?;

    if run.checks_seen == 0 {
        eprintln!(
            "{}",
            run.nothing_to_run(path, override_ext.is_some(), "--queries")
        );
        return Ok(0);
    }

    let Summary {
        pass,
        warn,
        fail,
        indeterminate: ind,
        error: err,
    } = run.summary();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&run.results).map_err(|e| e.to_string())?
        );
    } else {
        for r in &run.results {
            println!("{}", r.line());
        }
        println!(
            "{} check{}: {pass} passed, {fail} failed, {warn} warned, {ind} indeterminate, {err} error{}",
            run.results.len(),
            if run.results.len() == 1 { "" } else { "s" },
            if err == 1 { "" } else { "s" }
        );
    }

    if annotate {
        let n: usize = run.per_recording_events.iter().map(Vec::len).sum();
        if n == 0 {
            eprintln!("nothing to annotate: no violation windows");
        } else {
            // With --json, stdout is the array and nothing else.
            let line = annotate_events(path, run.format, run.per_recording_events.clone())
                .map_err(|e| e.to_string())?;
            if json {
                eprintln!("{line}");
            } else {
                println!("{line}");
            }
        }
    }

    Ok(run.exit_code())
}

/// Write the violation windows into the recording as events, through the
/// same code `recording annotate` uses for each container. Returns the
/// report line; the caller decides which stream it goes to.
pub(crate) fn annotate_events(
    path: &Path,
    format: RezFormat,
    per_recording: Vec<Vec<Event>>,
) -> Result<String, Box<dyn std::error::Error>> {
    if format == RezFormat::NotRez {
        let events = per_recording.into_iter().flatten().collect();
        let appended = super::events::append_to_parquet(path, events)?;
        return Ok(if appended.counts.nothing_new() {
            format!(
                "Annotated {:?}: nothing new; {} check event(s) already present",
                path, appended.counts.unchanged
            )
        } else {
            format!(
                "Annotated {:?}: {} ({} event(s) total)",
                path,
                appended.counts.describe(),
                appended.events.events.len()
            )
        });
    }
    let annotation = super::annotate::RezAnnotation {
        ext_json: None,
        events: None,
        per_recording_events: Some(per_recording),
        per_recording_replace: None,
        report: super::annotate::ReportSink::capture(),
    };
    super::annotate::annotate_rez_any(path, format, &annotation)?;
    Ok(annotation
        .report
        .captured()
        .unwrap_or_else(|| format!("Annotated {:?}", path)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn check(json: &str) -> Check {
        serde_json::from_str(json).unwrap()
    }

    fn states(values: &[(f64, PointState)]) -> Vec<(f64, PointState)> {
        values.to_vec()
    }

    use PointState::*;

    #[test]
    fn classify_compares_the_value_without_a_band() {
        assert_eq!(classify(6.0, None, false, Bound::Above(5.0)), Violating);
        assert_eq!(classify(5.0, None, false, Bound::Above(5.0)), Pass);
        assert_eq!(classify(4.0, None, false, Bound::Below(5.0)), Violating);
        assert_eq!(classify(5.0, None, false, Bound::Below(5.0)), Pass);
        assert_eq!(classify(f64::NAN, None, false, Bound::Above(5.0)), NoData);
    }

    #[test]
    fn a_straddling_band_is_indeterminate() {
        // above: the whole band must clear the threshold
        assert_eq!(
            classify(6.0, Some((5.5, 6.5)), false, Bound::Above(5.0)),
            Violating
        );
        assert_eq!(
            classify(5.2, Some((4.8, 5.6)), false, Bound::Above(5.0)),
            Indeterminate
        );
        assert_eq!(
            classify(4.0, Some((3.5, 5.0)), false, Bound::Above(5.0)),
            Pass
        );
        // below: likewise
        assert_eq!(
            classify(4.0, Some((3.5, 4.5)), false, Bound::Below(5.0)),
            Violating
        );
        assert_eq!(
            classify(4.9, Some((4.5, 5.2)), false, Bound::Below(5.0)),
            Indeterminate
        );
        assert_eq!(
            classify(6.0, Some((5.0, 7.0)), false, Bound::Below(5.0)),
            Pass
        );
    }

    #[test]
    fn an_interpolated_point_is_indeterminate_whatever_its_value() {
        assert_eq!(classify(9.0, None, true, Bound::Above(5.0)), Indeterminate);
        assert_eq!(classify(1.0, None, true, Bound::Above(5.0)), Indeterminate);
        assert_eq!(classify(f64::NAN, None, true, Bound::Above(5.0)), NoData);
    }

    #[test]
    fn windows_finds_maximal_runs_split_by_a_passing_point() {
        let s = states(&[
            (0.0, Violating),
            (1.0, Violating),
            (2.0, Pass),
            (3.0, Violating),
            (4.0, NoData),
            (5.0, Violating),
        ]);
        let w = windows(&s, 1.0, 0.0);
        assert_eq!(
            w.violating,
            vec![
                Window {
                    start: 0.0,
                    end: 2.0,
                    points: 2
                },
                Window {
                    start: 3.0,
                    end: 4.0,
                    points: 1
                },
                Window {
                    start: 5.0,
                    end: 6.0,
                    points: 1
                },
            ]
        );
        assert!(w.indeterminate.is_empty());
    }

    #[test]
    fn windows_drops_runs_shorter_than_for() {
        let s = states(&[
            (0.0, Violating),
            (1.0, Violating),
            (2.0, Pass),
            (3.0, Violating),
            (4.0, Violating),
            (5.0, Violating),
        ]);
        // span of the first run is 2s (two points, 1s step); the second is 3s
        let w = windows(&s, 1.0, 3.0);
        assert_eq!(
            w.violating,
            vec![Window {
                start: 3.0,
                end: 6.0,
                points: 3
            }]
        );
        // a `for` longer than any run finds nothing
        assert!(windows(&s, 1.0, 4.0).violating.is_empty());
    }

    #[test]
    fn a_gap_in_the_data_ends_a_run() {
        // Two seconds above the threshold, five minutes with no points (the
        // agent was down), two more seconds above. With `for: 1m` neither
        // side qualifies; counting the hole would have made one 5m04s window.
        let s = states(&[
            (0.0, Violating),
            (1.0, Violating),
            (302.0, Violating),
            (303.0, Violating),
        ]);
        let w = windows(&s, 1.0, 60.0);
        assert!(w.violating.is_empty(), "{w:?}");
        let w = windows(&s, 1.0, 0.0);
        assert_eq!(
            w.violating,
            vec![
                Window {
                    start: 0.0,
                    end: 2.0,
                    points: 2
                },
                Window {
                    start: 302.0,
                    end: 304.0,
                    points: 2
                },
            ]
        );
        // A gap of exactly 1.5 steps still continues the run; anything wider
        // ends it.
        let s = states(&[(0.0, Violating), (1.5, Violating), (3.1, Violating)]);
        let w = windows(&s, 1.0, 0.0);
        assert_eq!(w.violating.len(), 2, "{w:?}");
        assert_eq!(w.violating[0].points, 2);
    }

    #[test]
    fn a_run_with_any_indeterminate_point_is_indeterminate() {
        // One run of four non-passing points; the violating one inside it
        // does not split it, and does not make it violating.
        let s = states(&[
            (0.0, Indeterminate),
            (1.0, Indeterminate),
            (2.0, Violating),
            (3.0, Indeterminate),
        ]);
        let w = windows(&s, 1.0, 0.0);
        assert!(w.violating.is_empty(), "{w:?}");
        assert_eq!(
            w.indeterminate,
            vec![Window {
                start: 0.0,
                end: 4.0,
                points: 4
            }]
        );
        // A passing point still separates a violating run from an
        // indeterminate one.
        let s = states(&[
            (0.0, Violating),
            (1.0, Violating),
            (2.0, Pass),
            (3.0, Indeterminate),
        ]);
        let w = windows(&s, 1.0, 0.0);
        assert_eq!(
            w.violating,
            vec![Window {
                start: 0.0,
                end: 2.0,
                points: 2
            }]
        );
        assert_eq!(
            w.indeterminate,
            vec![Window {
                start: 3.0,
                end: 4.0,
                points: 1
            }]
        );
    }

    #[test]
    fn an_intermittent_straddle_keeps_a_long_violation_as_one_window() {
        // 60 s above the threshold with every tenth point straddling. Split
        // on the straddles this would be runs of 9 s and 1 s, none reaching
        // `for: 30s`, and the check would pass.
        let s: Vec<(f64, PointState)> = (0..60)
            .map(|i| {
                let state = if i % 10 == 9 {
                    Indeterminate
                } else {
                    Violating
                };
                (i as f64, state)
            })
            .collect();
        let w = windows(&s, 1.0, 30.0);
        assert!(w.violating.is_empty(), "{w:?}");
        assert_eq!(
            w.indeterminate,
            vec![Window {
                start: 0.0,
                end: 60.0,
                points: 60
            }]
        );
    }

    #[test]
    fn a_slow_sampler_is_stepped_at_its_own_spacing() {
        // A 10 s gauge evaluated on a 1 s grid: nine points 10 s apart, a
        // gap-free run of 90 s. On the grid step each point would be its
        // own 1 s window and `for: 60s` could never be met.
        let values: Vec<(f64, f64)> = (0..9).map(|i| (i as f64 * 10.0, 9.0)).collect();
        let s = series(&values, None);
        let w = evaluate_series(&s, &check(r#"{"above": 5, "for": "60s"}"#), 1.0);
        assert_eq!(
            w.violating,
            vec![Window {
                start: 0.0,
                end: 90.0,
                points: 9
            }]
        );
        // A hole of 5 minutes in that series is still a gap.
        let mut values = values;
        values.push((380.0, 9.0));
        values.push((390.0, 9.0));
        let s = series(&values, None);
        let w = evaluate_series(&s, &check(r#"{"above": 5}"#), 1.0);
        assert_eq!(w.violating.len(), 2, "{w:?}");
        assert_eq!(w.violating[1].points, 2);
    }

    #[test]
    fn windows_of_an_empty_series_is_empty() {
        assert_eq!(windows(&[], 1.0, 0.0), Windows::default());
        assert_eq!(
            windows(&states(&[(0.0, Pass)]), 1.0, 0.0),
            Windows::default()
        );
    }

    fn series(values: &[(f64, f64)], bands: Option<Vec<(f64, f64)>>) -> MatrixSample {
        MatrixSample::new(HashMap::new(), values.to_vec()).with_intervals(bands)
    }

    #[test]
    fn evaluate_series_above_and_below_over_synthetic_values() {
        let s = series(&[(10.0, 1.0), (11.0, 7.0), (12.0, 8.0), (13.0, 2.0)], None);
        let above = evaluate_series(&s, &check(r#"{"above": 5}"#), 1.0);
        assert_eq!(
            above.violating,
            vec![Window {
                start: 11.0,
                end: 13.0,
                points: 2
            }]
        );
        let below = evaluate_series(&s, &check(r#"{"below": 5, "for": "1s"}"#), 1.0);
        assert_eq!(
            below.violating,
            vec![
                Window {
                    start: 10.0,
                    end: 11.0,
                    points: 1
                },
                Window {
                    start: 13.0,
                    end: 14.0,
                    points: 1
                },
            ]
        );
        // `for` longer than the single-point runs: nothing
        let below = evaluate_series(&s, &check(r#"{"below": 5, "for": "2s"}"#), 1.0);
        assert!(below.violating.is_empty());
    }

    #[test]
    fn evaluate_series_uses_the_band_when_present() {
        let s = series(
            &[(10.0, 6.0), (11.0, 6.0), (12.0, 6.0)],
            Some(vec![(5.5, 6.5), (4.5, 7.5), (5.9, 6.1)]),
        );
        let w = evaluate_series(&s, &check(r#"{"above": 5}"#), 1.0);
        // The straddling middle point makes the whole run indeterminate.
        assert!(w.violating.is_empty(), "{w:?}");
        assert_eq!(
            w.indeterminate,
            vec![Window {
                start: 10.0,
                end: 13.0,
                points: 3
            }]
        );
        // With the middle point clear of the threshold the run violates.
        let s = series(
            &[(10.0, 6.0), (11.0, 6.0), (12.0, 6.0)],
            Some(vec![(5.5, 6.5), (5.2, 6.8), (5.9, 6.1)]),
        );
        let w = evaluate_series(&s, &check(r#"{"above": 5}"#), 1.0);
        assert_eq!(
            w.violating,
            vec![Window {
                start: 10.0,
                end: 13.0,
                points: 3
            }]
        );
    }

    #[test]
    fn evaluate_series_treats_interpolated_points_as_indeterminate() {
        let s = series(&[(10.0, 9.0), (11.0, 9.0), (12.0, 9.0)], None)
            .with_interpolated(Some(vec![false, true, false]));
        let w = evaluate_series(&s, &check(r#"{"above": 5}"#), 1.0);
        assert!(w.violating.is_empty(), "{w:?}");
        assert_eq!(
            w.indeterminate,
            vec![Window {
                start: 10.0,
                end: 13.0,
                points: 3
            }]
        );
    }

    #[test]
    fn evaluate_series_uses_the_grid_step_for_the_span_and_the_gap_rule() {
        // A 100 ms recording: two consecutive points span 200 ms, so
        // `for: 150ms` keeps the run; a 1 s step would have called it 1.1 s.
        let s = series(&[(0.0, 9.0), (0.1, 9.0), (0.2, 1.0)], None);
        let w = evaluate_series(&s, &check(r#"{"above": 5, "for": "150ms"}"#), 0.1);
        assert_eq!(w.violating.len(), 1);
        assert!((w.violating[0].end - 0.2).abs() < 1e-9, "{w:?}");
        // and a 300 ms hole at that step splits the run
        let s = series(&[(0.0, 9.0), (0.1, 9.0), (0.2, 9.0), (0.5, 9.0)], None);
        let w = evaluate_series(&s, &check(r#"{"above": 5}"#), 0.1);
        assert_eq!(w.violating.len(), 2, "{w:?}");
    }

    fn kpi(json: &str) -> Kpi {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_histogram_kpi_needs_a_quantile_and_no_histogram_quantile_in_its_query() {
        let k = kpi(
            r#"{"role":"latency","title":"t","query":"lat","type":"histogram","check":{"above":1}}"#,
        );
        let err = check_query(&k, k.check.as_ref().unwrap()).unwrap_err();
        assert!(err.contains("quantile"), "{err}");

        let k = kpi(
            r#"{"role":"latency","title":"t","query":"lat","type":"histogram","check":{"above":1,"quantile":0.99}}"#,
        );
        assert_eq!(
            check_query(&k, k.check.as_ref().unwrap()).unwrap(),
            "histogram_quantile(0.99, lat)"
        );

        let k = kpi(
            r#"{"role":"latency","title":"t","query":"histogram_quantile(0.5, lat)","type":"histogram","check":{"above":1,"quantile":0.99}}"#,
        );
        let err = check_query(&k, k.check.as_ref().unwrap()).unwrap_err();
        assert!(err.contains("already applies histogram_quantile"), "{err}");
    }

    #[test]
    fn quantile_on_a_non_histogram_kpi_is_refused() {
        let k = kpi(
            r#"{"role":"q","title":"t","query":"depth","type":"gauge","check":{"above":1,"quantile":0.99}}"#,
        );
        let err = check_query(&k, k.check.as_ref().unwrap()).unwrap_err();
        assert!(err.contains("only to a histogram KPI"), "{err}");
    }

    #[test]
    fn event_ids_are_stable_and_distinct_per_window_and_query() {
        let a = event_id("p99", "lat", "above 5 for 10s", 1_000);
        assert_eq!(a, event_id("p99", "lat", "above 5 for 10s", 1_000));
        assert_ne!(a, event_id("p99", "lat", "above 5 for 10s", 2_000));
        assert_ne!(a, event_id("p99", "lat", "above 6 for 10s", 1_000));
        // Same title and condition, different query: a different check.
        assert_ne!(a, event_id("p99", "other_lat", "above 5 for 10s", 1_000));
        assert!(
            a.starts_with("check:") && a.len() == "check:".len() + 16,
            "{a}"
        );
    }

    #[test]
    fn two_checks_sharing_a_title_produce_two_events_for_one_window() {
        let windows = || Windows {
            violating: vec![Window {
                start: 100.0,
                end: 110.0,
                points: 10,
            }],
            indeterminate: Vec::new(),
        };
        let a = kpi(
            r#"{"role":"q","title":"Dup title","query":"a","type":"gauge","check":{"above":1}}"#,
        );
        let b = kpi(
            r#"{"role":"q","title":"Dup title","query":"b","type":"gauge","check":{"above":1}}"#,
        );
        let ea = CheckResult::new(None, &a, a.check.as_ref().unwrap(), Ok(windows())).events();
        let eb = CheckResult::new(None, &b, b.check.as_ref().unwrap(), Ok(windows())).events();
        assert_ne!(ea[0].id, eb[0].id);
        let appended = super::super::events::append_events(None, [ea, eb].concat());
        assert_eq!(appended.counts.new, 2);
        assert_eq!(appended.counts.duplicates, 0);
    }

    #[test]
    fn a_result_renders_its_line_and_events() {
        let k = kpi(
            r#"{"role":"latency","title":"p99 latency","query":"lat","type":"gauge","check":{"above":5,"for":"10s","severity":"warn"}}"#,
        );
        let check = k.check.clone().unwrap();
        let windows = Windows {
            violating: vec![Window {
                start: 1_700_000_000.0,
                end: 1_700_000_020.0,
                points: 20,
            }],
            indeterminate: vec![Window {
                start: 1_700_000_100.0,
                end: 1_700_000_101.0,
                points: 1,
            }],
        };
        let r = CheckResult::new(None, &k, &check, Ok(windows));
        assert_eq!(r.status, Status::Warn);
        assert_eq!(
            r.line(),
            "WARN          p99 latency  above 5 for 10s  [2023-11-14T22:13:20Z..2023-11-14T22:13:40Z, 1 window] (+1 indeterminate)"
        );
        let events = r.events();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.kind.as_deref(), Some("check"));
        assert_eq!(e.timestamp, 1_700_000_000_000_000_000);
        assert_eq!(e.duration_ns, Some(20_000_000_000));
        assert_eq!(e.description, "p99 latency");
        let details = e.details.as_deref().unwrap();
        assert!(
            details.starts_with("p99 latency above 5 for 10s: warn\n"),
            "{details}"
        );
        let embedded: Check = serde_json::from_str(details.lines().nth(1).unwrap()).unwrap();
        assert_eq!(embedded, check);

        let labels = BTreeMap::from([("source".to_string(), "redis".to_string())]);
        let r = CheckResult::new(Some(labels), &k, &check, Err("no data".into()));
        assert_eq!(r.status, Status::Error);
        assert_eq!(
            r.line(),
            "[source=redis] ERROR         p99 latency  above 5 for 10s  error: no data"
        );
        assert!(r.events().is_empty());
    }
}
