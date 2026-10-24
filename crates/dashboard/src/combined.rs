//! An archive of several recordings opened as one combined view, shared by
//! both viewer backends (the axum server in `src/viewer/` and the WASM crate
//! in `crates/viewer/`).
//!
//! An archive's series carry their recording's labels (`source`, `host`,
//! `record --label`s, `arm`, and `__recording__` for recordings whose label
//! sets are equal; metriken-storage's `ArchiveReader::series_labels`). So one
//! reader over every recording answers a query for each of them, and a
//! matcher on a recording label selects one recording's series.
//!
//! - [`layout`] decides whether an archive opens combined or as an A/B
//!   comparison.
//! - [`restrict_query`] adds a recording's label matchers to every selector
//!   of a PromQL query, and [`Scoped`] is a source that answers for one
//!   recording only: its catalog, its header, and its queries restricted.
//! - [`Member`] describes one recording of a combined source, which
//!   [`crate::dashboard::build_combined_context`] turns into one instance of
//!   each template the recording renders.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use metriken_query::{
    DisplayOptions, DisplayResult, MetricsSource, QueryError, QueryOptions, QueryResult,
};
use metriken_storage::ArchiveReader;

/// The recording label that names a comparison's arm.
pub const ARM_LABEL: &str = "arm";

/// The reserved label that tells apart recordings whose label sets are
/// equal (metriken-storage's `labeled::RECORDING_LABEL`).
pub const RECORDING_LABEL: &str = "__recording__";

/// The View metadata key a restricted section carries its matchers under,
/// so the page can restrict the queries it builds itself (the cgroup
/// discovery) the same way.
pub const SCOPE_KEY: &str = "scope";

/// The label a query of a section shown beside a comparison carries to name
/// the recording it reads (`__capture__="<key>"`); [`Routed`] answers it
/// from that recording and removes it. Internal (`__`): no series has it.
pub const CAPTURE_LABEL: &str = "__capture__";

/// The View metadata key of a section that renders as a single capture,
/// valued by its recording's key: a recording beside a comparison.
pub const SINGLE_CAPTURE_KEY: &str = "single_capture";

/// A comparison's baseline source with the recordings shown beside it
/// (`dashboard::add_beside_sections`): a query whose selectors carry
/// `__capture__="<key>"` is answered by that recording's source, without
/// the matcher; every other query, and the catalog and header, by the
/// baseline.
pub struct Routed {
    primary: std::sync::Arc<dyn MetricsSource>,
    beside: Vec<(String, std::sync::Arc<dyn MetricsSource>)>,
}

impl Routed {
    pub fn new(
        primary: std::sync::Arc<dyn MetricsSource>,
        beside: Vec<(String, std::sync::Arc<dyn MetricsSource>)>,
    ) -> Self {
        Self { primary, beside }
    }

    /// The source a query names and the query without its
    /// `__capture__` matchers.
    fn route(&self, query: &str) -> (&dyn MetricsSource, String) {
        let marker = format!("{CAPTURE_LABEL}=\"");
        let Some(at) = query.find(&marker) else {
            return (self.primary.as_ref(), query.to_string());
        };
        let rest = &query[at + marker.len()..];
        let key = rest.split('"').next().unwrap_or_default();
        let Some((_, source)) = self.beside.iter().find(|(k, _)| k == key) else {
            return (self.primary.as_ref(), query.to_string());
        };
        let m = format!("{CAPTURE_LABEL}=\"{key}\"");
        let stripped = query
            .replace(&format!("{{{m}}}"), "")
            .replace(&format!(",{m}"), "")
            .replace(&format!("{m},"), "")
            .replace(&m, "");
        (source.as_ref(), stripped)
    }
}

impl MetricsSource for Routed {
    fn query_range_opts(
        &self,
        expr: &str,
        start_s: f64,
        end_s: f64,
        step_s: f64,
        opts: &QueryOptions,
    ) -> Result<QueryResult, QueryError> {
        let (source, expr) = self.route(expr);
        source.query_range_opts(&expr, start_s, end_s, step_s, opts)
    }
    fn query_range_display_opts(
        &self,
        expr: &str,
        start_s: f64,
        end_s: f64,
        step_s: f64,
        opts: &DisplayOptions,
        qopts: &QueryOptions,
    ) -> Result<DisplayResult, QueryError> {
        let (source, expr) = self.route(expr);
        source.query_range_display_opts(&expr, start_s, end_s, step_s, opts, qopts)
    }
    fn query(&self, expr: &str, time: Option<f64>) -> Result<QueryResult, QueryError> {
        let (source, expr) = self.route(expr);
        source.query(&expr, time)
    }
    fn columns(&self, query: &str) -> Result<HashSet<String>, QueryError> {
        let (source, query) = self.route(query);
        source.columns(&query)
    }
    // The baseline's span, which the experiment's is aligned against. Only
    // a baseline with no rows (a report trimmed to charts of a beside
    // recording) takes the span of the recordings beside it, so the page
    // does not draw over 0..0.
    fn time_range(&self) -> Option<(f64, f64)> {
        self.primary.time_range().or_else(|| {
            self.beside
                .iter()
                .filter_map(|(_, s)| s.time_range())
                .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
        })
    }
    fn time_range_ns(&self) -> Option<(u64, u64)> {
        self.primary.time_range_ns().or_else(|| {
            self.beside
                .iter()
                .filter_map(|(_, s)| s.time_range_ns())
                .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
        })
    }
    fn interval(&self) -> f64 {
        self.primary.interval()
    }
    fn source(&self) -> String {
        self.primary.source()
    }
    fn version(&self) -> String {
        self.primary.version()
    }
    fn filename(&self) -> Option<String> {
        self.primary.filename()
    }
    fn metadata_get(&self, key: &str) -> Option<String> {
        self.primary.metadata_get(key)
    }
    fn file_metadata(&self) -> HashMap<String, String> {
        self.primary.file_metadata()
    }
    fn counter_names(&self) -> Vec<String> {
        self.primary.counter_names()
    }
    fn gauge_names(&self) -> Vec<String> {
        self.primary.gauge_names()
    }
    fn histogram_names(&self) -> Vec<String> {
        self.primary.histogram_names()
    }
    fn counter_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.primary.counter_labels(name)
    }
    fn gauge_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.primary.gauge_labels(name)
    }
    fn histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.primary.histogram_labels(name)
    }
    fn gauge_histogram_names(&self) -> Vec<String> {
        self.primary.gauge_histogram_names()
    }
    fn gauge_histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.primary.gauge_histogram_labels(name)
    }
    fn sample_timestamps(&self) -> Vec<u64> {
        self.primary.sample_timestamps()
    }
}

/// The file-metadata keys of a saved report: its selection, its report
/// marker and its events (`report_save::{KEY_SELECTION, KEY_REPORT,
/// KEY_EVENTS}`).
pub const REPORT_KEYS: [&str; 3] = ["selection", "report", "events"];

/// A report's [`REPORT_KEYS`] in an archive of several recordings, all
/// from the first of `sources` whose metadata carries a selection (the
/// baseline first, so its own wins), with the keys that recording has. A
/// report is saved onto the baseline's recording, but one saved before the
/// baseline was chosen by arm, or from a comparison of an archive that now
/// opens combined, sits on another. Empty when none carries a selection.
pub fn report_markers(sources: &[&dyn MetricsSource]) -> Vec<(&'static str, String)> {
    let Some(meta) = sources
        .iter()
        .map(|s| s.file_metadata())
        .find(|m| m.contains_key(REPORT_KEYS[0]))
    else {
        return Vec::new();
    };
    REPORT_KEYS
        .iter()
        .filter_map(|k| meta.get(*k).map(|v| (*k, v.clone())))
        .collect()
}

/// The value of `key` among `markers` ([`report_markers`]).
pub fn marker(markers: &[(&'static str, String)], key: &str) -> Option<String> {
    markers
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.clone())
}

/// `metadata` (a capture's file metadata, a JSON object) with `markers`
/// set in it, each value embedded as JSON when it parses: the page reads a
/// report's events from the baseline's metadata. Unchanged when `markers`
/// is empty or `metadata` is not an object.
pub fn with_report_markers(metadata: &str, markers: &[(&'static str, String)]) -> String {
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str(metadata) else {
        return metadata.to_string();
    };
    if markers.is_empty() {
        return metadata.to_string();
    }
    for (k, v) in markers {
        let value =
            serde_json::from_str(v).unwrap_or_else(|_| serde_json::Value::String(v.clone()));
        map.insert(k.to_string(), value);
    }
    serde_json::to_string(&map).unwrap_or_else(|_| metadata.to_string())
}

/// How an archive of several recordings opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// One view over every recording: each series under its recording's
    /// labels, each template rendered once per recording that carries it.
    Combined,
    /// The A/B comparison: recordings fill the baseline and experiment
    /// slots, each its own capture.
    Compare,
}

/// Whether an archive opens combined or as a comparison.
///
/// A comparison is opt-in: when `compare_requested` (the server's
/// `--baseline` or `--experiment`, the WASM registry's
/// `set_compare_requested`, the static site's `?compare=1`), or when any
/// recording carries an [`ARM_LABEL`] label (`record --endpoint url,arm=..`,
/// `recording combine --ab`). Combined otherwise, a single recording
/// included. Nothing is inferred from when the recordings were taken: an A/B
/// can be recorded in one run, two endpoints under the same load, and
/// recordings of one run can be assembled from separate files.
pub fn layout(labels: &[BTreeMap<String, String>], compare_requested: bool) -> Layout {
    if compare_requested || labels.iter().any(|l| l.contains_key(ARM_LABEL)) {
        Layout::Compare
    } else {
        Layout::Combined
    }
}

/// Which recordings of a comparison fill the baseline and experiment slots
/// when no selector names them ([`arm_slots`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArmSlots {
    pub baseline: usize,
    pub experiment: Option<usize>,
    /// What the viewer should say about the choice: which arm is the
    /// baseline when the arms do not say. (The recordings shown beside the
    /// comparison are named by the viewer, which also counts the ones
    /// `--baseline`/`--experiment` did not name.)
    pub note: Option<String>,
}

/// The slots of a comparison from its recordings' `arm` labels, the same in
/// both viewers, whatever the archive's order:
///
/// - `arm=baseline` fills the baseline slot and `arm=experiment` the
///   experiment slot, matched ignoring ASCII case and surrounding
///   whitespace (`Baseline` counts);
/// - with two arms of which only one is standard, the other fills the other
///   slot (`arm=control` beside `arm=baseline` is the experiment);
/// - with two arms of which neither is, they fill the slots in the
///   archive's order, and the note says which is the baseline;
/// - recordings without an arm are shown beside the comparison (an agent
///   recorded with two arms of a service);
/// - with no recording carrying an arm (a comparison asked for with
///   `?compare=1`), the first two recordings fill the slots.
///
/// One recording per arm is the current limit: one armed recording among
/// unarmed ones, an arm two recordings share, or more than two arms is an
/// error, for the caller to list the recordings and point at
/// `--baseline`/`--experiment`. A single recording, armed or not, is the
/// baseline alone.
pub fn arm_slots(labels: &[BTreeMap<String, String>]) -> Result<ArmSlots, String> {
    let armed: Vec<(usize, &str)> = labels
        .iter()
        .enumerate()
        .filter_map(|(i, l)| l.get(ARM_LABEL).map(|a| (i, a.as_str())))
        .collect();
    if armed.is_empty() {
        return Ok(ArmSlots {
            baseline: 0,
            experiment: (labels.len() >= 2).then_some(1),
            note: None,
        });
    }
    let norm: Vec<String> = armed
        .iter()
        .map(|(_, a)| a.trim().to_ascii_lowercase())
        .collect();
    if norm.iter().collect::<BTreeSet<_>>().len() != norm.len() {
        return Err(
            "two recordings carry the same `arm`; a comparison takes one recording per arm".into(),
        );
    }
    if armed.len() > 2 {
        return Err(format!(
            "{} recordings carry an `arm`; a comparison takes one recording per arm, two arms",
            armed.len()
        ));
    }
    let mut notes = Vec::new();
    let (baseline, experiment) = match armed.as_slice() {
        [(only, _)] if labels.len() == 1 => (*only, None),
        [_] => {
            return Err("one recording carries an `arm`; a comparison takes two arms".into());
        }
        [(i, a), (j, b)] => {
            let pick = |want: &str| norm.iter().position(|n| n == want);
            let (b_at, e_at) = match (pick("baseline"), pick("experiment")) {
                (Some(x), Some(y)) => (x, y),
                (Some(x), None) => (x, 1 - x),
                (None, Some(y)) => (1 - y, y),
                (None, None) => {
                    notes.push(format!(
                        "neither arm is `baseline` or `experiment`: arm={a}, the archive's \
                         first, is the baseline and arm={b} the experiment"
                    ));
                    (0, 1)
                }
            };
            let at = [*i, *j];
            (at[b_at], Some(at[e_at]))
        }
        _ => unreachable!("one or two arms"),
    };
    Ok(ArmSlots {
        baseline,
        experiment,
        note: (!notes.is_empty()).then(|| notes.join("; ")),
    })
}

/// How a [`Matcher`] compares a label's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum MatchOp {
    /// `label="value"`.
    #[serde(rename = "=")]
    Eq,
    /// `label!="value"`, which a series without the label also passes.
    #[serde(rename = "!=")]
    Ne,
    /// `label!~"a|b"`: none of the alternatives, which a series without the
    /// label also passes.
    #[serde(rename = "!~")]
    NotRegex,
}

/// One label matcher of a recording's scope.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Matcher {
    pub label: String,
    pub op: MatchOp,
    pub value: String,
}

impl Matcher {
    pub fn eq(label: &str, value: &str) -> Self {
        Self {
            label: label.to_string(),
            op: MatchOp::Eq,
            value: value.to_string(),
        }
    }

    pub fn ne(label: &str, value: &str) -> Self {
        Self {
            label: label.to_string(),
            op: MatchOp::Ne,
            value: value.to_string(),
        }
    }

    /// Whether a series labelled `labels` passes.
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        let own = labels.get(&self.label).map(String::as_str);
        match self.op {
            MatchOp::Eq => own == Some(self.value.as_str()),
            MatchOp::Ne => own != Some(self.value.as_str()),
            MatchOp::NotRegex => own.is_none_or(|v| !self.value.split('|').any(|a| a == v)),
        }
    }

    fn push(&self, out: &mut String) {
        out.push_str(&self.label);
        out.push_str(match self.op {
            MatchOp::Eq => "=\"",
            MatchOp::Ne => "!=\"",
            MatchOp::NotRegex => "!~\"",
        });
        for c in self.value.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c => out.push(c),
            }
        }
        out.push('"');
    }
}

/// The matchers that select each recording's series out of a combined
/// source, from `series_labels` (the labels each recording's series carry,
/// `__recording__` included where it applies), over the keys whose values
/// are not the same for every recording: `k="v"` for a key the recording
/// has, and for a key it lacks `k!="v"` for the one value the others carry,
/// or `k!~"v1|v2"` for several (a selector may match a label once). One
/// recording, or recordings whose series carry equal labels, get none.
pub fn scope_matchers(series_labels: &[BTreeMap<String, String>]) -> Vec<Vec<Matcher>> {
    let keys = discriminating_keys(series_labels);
    series_labels
        .iter()
        .map(|labels| {
            keys.iter()
                .map(|k| match labels.get(*k) {
                    Some(v) => Matcher::eq(k, v),
                    None => {
                        let others: BTreeSet<&String> =
                            series_labels.iter().filter_map(|l| l.get(*k)).collect();
                        let values: Vec<&str> = others.into_iter().map(String::as_str).collect();
                        if values.len() == 1 {
                            Matcher::ne(k, values[0])
                        } else {
                            Matcher {
                                label: (*k).to_string(),
                                op: MatchOp::NotRegex,
                                value: values.join("|"),
                            }
                        }
                    }
                })
                .collect()
        })
        .collect()
}

/// The keys whose values (absence counted as a value) differ across `labels`.
fn discriminating_keys(labels: &[BTreeMap<String, String>]) -> Vec<&str> {
    let all: BTreeSet<&str> = labels
        .iter()
        .flat_map(|l| l.keys().map(String::as_str))
        .collect();
    all.into_iter()
        .filter(|k| {
            let values: HashSet<Option<&String>> = labels.iter().map(|l| l.get(*k)).collect();
            values.len() > 1
        })
        .collect()
}

/// A display name for each of `labels` that tells them apart: the `host`
/// value when every one has a host and no two share it, else the differing
/// values joined by `/`, else `recording N` (1-based).
pub fn instance_names(labels: &[BTreeMap<String, String>]) -> Vec<String> {
    names_or(labels, |i, _| format!("recording {}", i + 1))
}

/// [`instance_names`] with `fallback` naming a recording that no label
/// tells apart from the others (given its index and labels).
pub fn names_or(
    labels: &[BTreeMap<String, String>],
    fallback: impl Fn(usize, &BTreeMap<String, String>) -> String,
) -> Vec<String> {
    let keys = discriminating_keys(labels);
    let hosts: Vec<Option<&String>> = labels.iter().map(|l| l.get("host")).collect();
    let unique_hosts = hosts.iter().all(Option::is_some)
        && hosts.iter().collect::<HashSet<_>>().len() == hosts.len();
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if unique_hosts && let Some(host) = l.get("host") {
                return host.clone();
            }
            let values: Vec<&str> = keys
                .iter()
                .filter(|k| **k != RECORDING_LABEL)
                .filter_map(|k| l.get(*k).map(String::as_str))
                .collect();
            if values.is_empty() {
                fallback(i, l)
            } else {
                values.join("/")
            }
        })
        .collect()
}

/// A route suffix for each of `names`: lower-cased, every character outside
/// `a-z`, `0-9`, `.`, `_` and `-` replaced by `-`, and made unique by a
/// numeric suffix. An instance's routes are its template's routes followed
/// by `@` and this key (`/cpu@web-02`).
pub fn instance_keys(names: &[String]) -> Vec<String> {
    let mut used: HashSet<String> = HashSet::new();
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut key: String = name
                .chars()
                .map(|c| {
                    let c = c.to_ascii_lowercase();
                    if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                        c
                    } else {
                        '-'
                    }
                })
                .collect();
            if key.is_empty() {
                key = (i + 1).to_string();
            }
            let mut unique = key.clone();
            let mut n = 2;
            while !used.insert(unique.clone()) {
                unique = format!("{key}-{n}");
                n += 1;
            }
            unique
        })
        .collect()
}

/// The separator between a template's route and an instance's key.
pub const INSTANCE_SEPARATOR: char = '@';

/// `route` without its instance key: `/cpu@web-02` is `/cpu`.
pub fn base_route(route: &str) -> &str {
    route
        .split_once(INSTANCE_SEPARATOR)
        .map_or(route, |(base, _)| base)
}

// ── Restricting a query ─────────────────────────────────────────────────

/// Keywords after which a parenthesized list names labels, not series.
const LABEL_LIST_KEYWORDS: &[&str] = &[
    "by",
    "without",
    "on",
    "ignoring",
    "group_left",
    "group_right",
];

/// Other words that are never a metric name in an expression.
const KEYWORDS: &[&str] = &["bool", "and", "or", "unless", "atan2", "inf", "nan"];

/// PromQL's aggregation operators, which may take `by`/`without` before
/// their parenthesized argument (`sum by (id) (x)`).
const AGGREGATORS: &[&str] = &[
    "sum",
    "avg",
    "min",
    "max",
    "count",
    "stddev",
    "stdvar",
    "topk",
    "bottomk",
    "quantile",
    "count_values",
    "group",
    "limitk",
    "limit_ratio",
];

/// `query` with `matchers` added to every series selector: a bare metric
/// name gains `{k="v",...}`, and each `or` branch of a selector's braces
/// (`x{a="b"}`, `{__name__="x"}`, `{a="1" or b="2"}`) gains each matcher.
/// A branch with its own `k="v"` (an `=` with a value) on a matcher's label
/// keeps it in place of the matcher, so a query written for a source
/// (`{source="vllm"}`) or restricted before (a service's throughput query
/// placed into another source's Overview) is left as it asks. Any other
/// matcher of its own on the label (`host!=""`, `host=~".+"`) is kept and
/// the scope's is added too; the engine refuses a selector that matches a
/// label twice, so such a query errors rather than reading every
/// recording.
///
/// The query is read by its tokens rather than parsed, so it works for the
/// histogram functions PromQL does not have (`histogram_quantiles([0.5],
/// x)`), and every character outside the inserted matchers is kept as
/// written. Strings, `#` comments, `[...]` ranges and quantile lists, the
/// label lists of `by`, `without`, `on`, `ignoring`, `group_left` and
/// `group_right`, and an `offset` duration are skipped; an identifier
/// followed by `(` is a function or an aggregation.
pub fn restrict_query(query: &str, matchers: &[Matcher]) -> String {
    if matchers.is_empty() {
        return query.to_string();
    }
    let bytes = query.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(query.len() + 32);
    let mut i = 0;
    while i < len {
        let c = bytes[i];
        match c {
            b'"' | b'\'' | b'`' => {
                let end = skip_string(bytes, i);
                out.push_str(&query[i..end]);
                i = end;
            }
            b'#' => {
                let end = skip_comment(bytes, i);
                out.push_str(&query[i..end]);
                i = end;
            }
            b'[' => {
                let end = skip_until(bytes, i + 1, b']');
                out.push_str(&query[i..end]);
                i = end;
            }
            b'{' => {
                let end = skip_until(bytes, i + 1, b'}');
                let closed = end <= len && bytes[end - 1] == b'}';
                let inner = if closed {
                    &query[i + 1..end - 1]
                } else {
                    &query[i + 1..end]
                };
                out.push_str(&restrict_braces(inner, matchers));
                i = end;
            }
            b'0'..=b'9' => {
                let end = skip_number(bytes, i);
                out.push_str(&query[i..end]);
                i = end;
            }
            b'.' if bytes.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                let end = skip_number(bytes, i);
                out.push_str(&query[i..end]);
                i = end;
            }
            c if is_ident_start(c) => {
                let end = skip_ident(bytes, i);
                let ident = &query[i..end];
                out.push_str(ident);
                i = end;
                let lower = ident.to_ascii_lowercase();
                let next = skip_ws(bytes, i);
                if LABEL_LIST_KEYWORDS.contains(&lower.as_str()) {
                    if bytes.get(next) == Some(&b'(') {
                        let close = skip_until(bytes, next + 1, b')');
                        out.push_str(&query[i..close]);
                        i = close;
                    }
                    continue;
                }
                if lower == "offset" {
                    let mut j = next;
                    if bytes.get(j) == Some(&b'-') {
                        j += 1;
                    }
                    while j < len && bytes[j].is_ascii_alphanumeric() {
                        j += 1;
                    }
                    out.push_str(&query[i..j]);
                    i = j;
                    continue;
                }
                if KEYWORDS.contains(&lower.as_str()) {
                    continue;
                }
                match bytes.get(next) {
                    // A function call or an aggregation.
                    Some(b'(') => continue,
                    // A selector with braces: they are handled next.
                    Some(b'{') => continue,
                    _ => {}
                }
                if AGGREGATORS.contains(&lower.as_str()) {
                    let word_end = skip_ident(bytes, next);
                    let word = query[next..word_end].to_ascii_lowercase();
                    if word == "by" || word == "without" {
                        continue;
                    }
                }
                out.push('{');
                push_matchers(&mut out, matchers.iter());
                out.push('}');
            }
            _ => {
                // Copy one character (a multi-byte one whole).
                let ch = query[i..].chars().next().expect("i is a char boundary");
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// The braces of a selector, `inner` being what is between them, with
/// each `or` branch restricted by [`restrict_branch`].
fn restrict_braces(inner: &str, matchers: &[Matcher]) -> String {
    let mut out = String::with_capacity(inner.len() + 32);
    out.push('{');
    let mut start = 0;
    for (or_start, or_end) in or_separators(inner) {
        out.push_str(&restrict_branch(&inner[start..or_start], matchers));
        out.push_str(&inner[or_start..or_end]);
        start = or_end;
    }
    out.push_str(&restrict_branch(&inner[start..], matchers));
    out.push('}');
    out
}

/// One `or` branch of a selector's braces with each matcher added whose
/// label the branch does not match with `=` and a value. Whitespace around
/// the branch is kept.
fn restrict_branch(branch: &str, matchers: &[Matcher]) -> String {
    let pinned = pinned_labels(branch);
    let missing: Vec<&Matcher> = matchers
        .iter()
        .filter(|m| !pinned.contains(m.label.as_str()))
        .collect();
    if missing.is_empty() {
        return branch.to_string();
    }
    let body_end = branch.trim_end().len();
    let (body, tail) = branch.split_at(body_end);
    let mut out = String::with_capacity(branch.len() + 32);
    out.push_str(body);
    if !body.trim().is_empty() && !body.ends_with(',') {
        out.push(',');
    }
    push_matchers(&mut out, missing.into_iter());
    out.push_str(tail);
    out
}

/// The spans of the top-level `or` keywords between a selector's braces.
fn or_separators(inner: &str) -> Vec<(usize, usize)> {
    let bytes = inner.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' || c == b'\'' || c == b'`' {
            i = skip_string(bytes, i);
        } else if c == b'#' {
            i = skip_comment(bytes, i);
        } else if is_ident_start(c) {
            let end = skip_ident(bytes, i);
            let word = &inner[i..end];
            let before = i == 0 || bytes[i - 1].is_ascii_whitespace();
            let after = end == bytes.len() || bytes[end].is_ascii_whitespace();
            if word.eq_ignore_ascii_case("or") && before && after {
                spans.push((i, end));
            }
            i = end;
        } else {
            i += 1;
        }
    }
    spans
}

/// The labels a branch of a selector's braces matches with `=` and a
/// non-empty value.
fn pinned_labels(branch: &str) -> HashSet<&str> {
    let bytes = branch.as_bytes();
    let mut pinned = HashSet::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' || c == b'\'' || c == b'`' {
            i = skip_string(bytes, i);
        } else if c == b'#' {
            i = skip_comment(bytes, i);
        } else if is_ident_start(c) {
            let end = skip_ident(bytes, i);
            let name = &branch[i..end];
            let j = skip_ws(bytes, end);
            // `=` alone: not `=~`, `!=` or `!~`.
            let eq = bytes.get(j) == Some(&b'=') && bytes.get(j + 1) != Some(&b'~');
            i = end;
            if eq {
                let k = skip_ws(bytes, j + 1);
                if matches!(bytes.get(k), Some(b'"' | b'\'' | b'`')) {
                    let close = skip_string(bytes, k);
                    if close > k + 2 {
                        pinned.insert(name);
                    }
                    i = close;
                }
            }
        } else {
            i += 1;
        }
    }
    pinned
}

fn push_matchers<'a>(out: &mut String, matchers: impl Iterator<Item = &'a Matcher>) {
    for (n, m) in matchers.enumerate() {
        if n > 0 {
            out.push(',');
        }
        m.push(out);
    }
}

/// The index of the newline ending the `#` comment at `i`, or the end.
fn skip_comment(bytes: &[u8], i: usize) -> usize {
    bytes[i..]
        .iter()
        .position(|b| *b == b'\n')
        .map_or(bytes.len(), |n| i + n)
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b':'
}

fn skip_ident(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b':')
    {
        i += 1;
    }
    i
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// The index after the string literal starting at `i`. A backtick string
/// has no escapes.
fn skip_string(bytes: &[u8], i: usize) -> usize {
    let quote = bytes[i];
    let mut j = i + 1;
    while j < bytes.len() {
        if bytes[j] == b'\\' && quote != b'`' {
            j += 2;
            continue;
        }
        if bytes[j] == quote {
            return j + 1;
        }
        j += 1;
    }
    bytes.len()
}

/// The index after the first `close` at or after `i` outside a string, or
/// the end.
fn skip_until(bytes: &[u8], mut i: usize, close: u8) -> usize {
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => i = skip_string(bytes, i),
            b'#' => i = skip_comment(bytes, i),
            c if c == close => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// The index after the number starting at `i`: digits, letters (hex,
/// exponent, `inf`-free durations), `.` and `_`, and a sign after an
/// exponent.
fn skip_number(bytes: &[u8], i: usize) -> usize {
    let hex = bytes.get(i) == Some(&b'0') && matches!(bytes.get(i + 1), Some(b'x' | b'X'));
    let mut j = i;
    while j < bytes.len() {
        let c = bytes[j];
        let exponent_sign = (c == b'+' || c == b'-') && !hex && matches!(bytes[j - 1], b'e' | b'E');
        if c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || exponent_sign {
            j += 1;
        } else {
            break;
        }
    }
    j
}

// ── One recording of a combined source ──────────────────────────────────

/// What a recording's own reader reports, kept for when it is read through
/// a combined one: the header of each section its templates render.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Header {
    pub interval: f64,
    pub source: String,
    pub version: String,
    pub time_range_ns: Option<(u64, u64)>,
}

impl Header {
    /// `data`'s own header.
    pub fn of(data: &dyn MetricsSource) -> Self {
        Self {
            interval: data.interval(),
            source: data.source(),
            version: data.version(),
            time_range_ns: data.time_range_ns(),
        }
    }
}

/// One recording of a combined source.
#[derive(Clone, Debug, Default)]
pub struct Member {
    /// The labels the recording's series carry in the combined source.
    pub series_labels: BTreeMap<String, String>,
    /// The recording's own metadata (`MetricsSource::file_metadata`).
    pub metadata: HashMap<String, String>,
    /// The recording's own header.
    pub header: Header,
}

/// One recording for [`assemble_members`]: its labels, the labels its
/// series carry in the joined reader, its header and its metadata.
pub type RecordingParts = (
    BTreeMap<String, String>,
    Option<BTreeMap<String, String>>,
    Header,
    HashMap<String, String>,
);

/// The [`Member`]s of a combined source, from each recording's labels (as
/// the catalog holds them), the labels its series carry in the joined reader
/// (`None` for a recording with no tables yet, which then counts by its
/// labels, so an endpoint whose rows have not arrived still has its
/// sections and is not merged into another's), its header and its
/// metadata; and the source's systeminfo: the one recording's that carries
/// one, or with several, an object of each by its instance name
/// ([`instance_names`]), the shape the System Info page shows per node.
pub fn assemble_members(recordings: Vec<RecordingParts>) -> (Vec<Member>, Option<String>) {
    let members: Vec<Member> = recordings
        .into_iter()
        .map(|(labels, series, header, metadata)| Member {
            series_labels: series.unwrap_or(labels),
            metadata,
            header,
        })
        .collect();
    let with_info: Vec<&Member> = members
        .iter()
        .filter(|m| m.metadata.contains_key("systeminfo"))
        .collect();
    let systeminfo = match with_info.as_slice() {
        [] => None,
        [one] => one.metadata.get("systeminfo").cloned(),
        several => {
            let labels: Vec<BTreeMap<String, String>> =
                several.iter().map(|m| m.series_labels.clone()).collect();
            let names = instance_names(&labels);
            let mut nodes = serde_json::Map::new();
            for (name, m) in names.into_iter().zip(several) {
                let raw = &m.metadata["systeminfo"];
                let value = serde_json::from_str(raw)
                    .unwrap_or_else(|_| serde_json::Value::String(raw.clone()));
                nodes.insert(name, value);
            }
            serde_json::to_string(&nodes).ok()
        }
    };
    (members, systeminfo)
}

/// One recording's part of a combined source: its matchers, header and
/// metadata.
#[derive(Clone, Default)]
pub struct Scope {
    /// What selects the recording's series; see [`scope_matchers`].
    pub matchers: Vec<Matcher>,
    pub header: Header,
    pub metadata: HashMap<String, String>,
    /// The archive the combined source reads, when the shell has it: the
    /// recording's names and label sets are then answered from its
    /// in-memory catalog and that recording's tables, rather than from
    /// every recording's ([`ScopeCatalog`]). `None` lists through the
    /// source and filters, which is exact but opens every table holding a
    /// name.
    pub catalog: Option<Arc<dyn ScopeCatalog>>,
}

impl std::fmt::Debug for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scope")
            .field("matchers", &self.matchers)
            .field("header", &self.header)
            .field("metadata", &self.metadata)
            .field("catalog", &self.catalog.is_some())
            .finish()
    }
}

/// A metric kind, for [`ScopeCatalog`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Counter,
    Gauge,
    Histogram,
    GaugeHistogram,
}

/// The archive behind a combined source, for answering one recording's
/// catalog questions ([`Scoped`]) without opening other recordings' tables.
///
/// A combined source's series carry their recording's labels
/// (`ArchiveReader::series_labels`), and a series' own label under one of
/// those keys is moved to `exported_<key>`, so a matcher on such a key is
/// decided by the recording's labels alone. A table whose recording fails one
/// is skipped unopened; one whose recording passes every matcher answers a
/// name from its in-memory catalog; only a matcher on a key the recording
/// lacks (a series' own label may then decide it) opens the table to look.
pub trait ScopeCatalog: Send + Sync {
    /// The archive as it reads now (a live archive's current reader).
    fn archive(&self) -> Arc<dyn std::ops::Deref<Target = ArchiveReader> + Send + Sync>;
}

/// A [`ScopeCatalog`] over a reader that does not change.
pub struct FixedCatalog<T>(pub Arc<T>);

impl<T> ScopeCatalog for FixedCatalog<T>
where
    T: std::ops::Deref<Target = ArchiveReader> + Send + Sync + 'static,
{
    fn archive(&self) -> Arc<dyn std::ops::Deref<Target = ArchiveReader> + Send + Sync> {
        Arc::clone(&self.0) as Arc<dyn std::ops::Deref<Target = ArchiveReader> + Send + Sync>
    }
}

/// Whether `table`'s series can pass `matchers`: `Some(true)` when its
/// recording's labels pass every one, `Some(false)` when they fail one,
/// `None` when a matcher names a key the recording lacks.
fn table_passes(extra: &BTreeMap<String, String>, matchers: &[Matcher]) -> Option<bool> {
    let mut undecided = false;
    for m in matchers {
        if extra.contains_key(&m.label) {
            if !m.matches(extra) {
                return Some(false);
            }
        } else {
            undecided = true;
        }
    }
    (!undecided).then_some(true)
}

fn holds(table: &metriken_storage::reader::SamplerReader, kind: Kind, name: &str) -> bool {
    match kind {
        Kind::Counter => table.holds_counter(name),
        Kind::Gauge => table.holds_gauge(name),
        Kind::Histogram => table.holds_histogram(name),
        Kind::GaugeHistogram => table.holds_gauge_histogram(name),
    }
}

/// The label sets of `name` in `table`, with its recording's labels.
fn table_labels(
    table: &metriken_storage::reader::SamplerReader,
    extra: &metriken_storage::Labels,
    kind: Kind,
    name: &str,
) -> Vec<BTreeMap<String, String>> {
    let Some(reader) = table.reader() else {
        return Vec::new();
    };
    let source = reader.source();
    let mut sets = match kind {
        Kind::Counter => source.counter_labels(name),
        Kind::Gauge => source.gauge_labels(name),
        Kind::Histogram => source.histogram_labels(name),
        Kind::GaugeHistogram => source.gauge_histogram_labels(name),
    };
    for set in &mut sets {
        metriken_storage::labeled::add_labels_map(set, extra);
    }
    sets
}

/// The names of `kind` with a series passing `matchers` ([`ScopeCatalog`]),
/// in `names`' order.
fn scoped_names(
    archive: &ArchiveReader,
    kind: Kind,
    names: Vec<String>,
    matchers: &[Matcher],
) -> Vec<String> {
    let extras = archive.series_labels();
    names
        .into_iter()
        .filter(|name| {
            archive
                .owners_of(std::slice::from_ref(name))
                .into_iter()
                .filter(|t| holds(t, kind, name))
                .any(|t| {
                    let extra = extras.of(t);
                    match table_passes(&extra.inner, matchers) {
                        Some(pass) => pass,
                        None => table_labels(t, extra, kind, name)
                            .iter()
                            .any(|l| matchers.iter().all(|m| m.matches(l))),
                    }
                })
        })
        .collect()
}

/// The label sets of `name` passing `matchers` ([`ScopeCatalog`]), in the
/// order the archive lists them.
fn scoped_labels(
    archive: &ArchiveReader,
    kind: Kind,
    name: &str,
    matchers: &[Matcher],
) -> Vec<BTreeMap<String, String>> {
    let extras = archive.series_labels();
    let mut seen = std::collections::HashSet::new();
    archive
        .owners_of(&[name.to_string()])
        .into_iter()
        .filter(|t| holds(t, kind, name))
        .filter(|t| table_passes(&extras.of(t).inner, matchers) != Some(false))
        .flat_map(|t| table_labels(t, extras.of(t), kind, name))
        .filter(|l| matchers.iter().all(|m| m.matches(l)))
        // The sources of one read group (a recording and its copy) carry one
        // label set, so their tables list a series once each; it is listed
        // once, as the archive's own listing does.
        .filter(|l| seen.insert(l.clone()))
        .collect()
}

/// A combined source read as one recording of it: the catalog lists only
/// series whose labels include every matcher, the header and metadata are
/// the recording's, and every query is restricted by [`restrict_query`].
pub struct Scoped<'a> {
    inner: &'a dyn MetricsSource,
    scope: &'a Scope,
}

impl<'a> Scoped<'a> {
    pub fn new(inner: &'a dyn MetricsSource, scope: &'a Scope) -> Self {
        Self { inner, scope }
    }

    fn restrict(&self, query: &str) -> String {
        restrict_query(query, &self.scope.matchers)
    }

    fn keep(&self, labels: Vec<BTreeMap<String, String>>) -> Vec<BTreeMap<String, String>> {
        labels
            .into_iter()
            .filter(|l| self.scope.matchers.iter().all(|m| m.matches(l)))
            .collect()
    }

    fn names(&self, kind: Kind) -> Vec<String> {
        let names = match kind {
            Kind::Counter => self.inner.counter_names(),
            Kind::Gauge => self.inner.gauge_names(),
            Kind::Histogram => self.inner.histogram_names(),
            Kind::GaugeHistogram => self.inner.gauge_histogram_names(),
        };
        if self.scope.matchers.is_empty() {
            return names;
        }
        if let Some(catalog) = &self.scope.catalog {
            return scoped_names(&catalog.archive(), kind, names, &self.scope.matchers);
        }
        names
            .into_iter()
            .filter(|n| !self.keep(self.listing(kind, n)).is_empty())
            .collect()
    }

    /// The unfiltered listing of `name` from the source.
    fn listing(&self, kind: Kind, name: &str) -> Vec<BTreeMap<String, String>> {
        match kind {
            Kind::Counter => self.inner.counter_labels(name),
            Kind::Gauge => self.inner.gauge_labels(name),
            Kind::Histogram => self.inner.histogram_labels(name),
            Kind::GaugeHistogram => self.inner.gauge_histogram_labels(name),
        }
    }

    fn labels(&self, kind: Kind, name: &str) -> Vec<BTreeMap<String, String>> {
        match &self.scope.catalog {
            Some(catalog) if !self.scope.matchers.is_empty() => {
                scoped_labels(&catalog.archive(), kind, name, &self.scope.matchers)
            }
            _ => self.keep(self.listing(kind, name)),
        }
    }
}

impl MetricsSource for Scoped<'_> {
    fn query_range_opts(
        &self,
        expr: &str,
        start_s: f64,
        end_s: f64,
        step_s: f64,
        opts: &QueryOptions,
    ) -> Result<QueryResult, QueryError> {
        self.inner
            .query_range_opts(&self.restrict(expr), start_s, end_s, step_s, opts)
    }
    fn query_range_display_opts(
        &self,
        expr: &str,
        start_s: f64,
        end_s: f64,
        step_s: f64,
        opts: &DisplayOptions,
        qopts: &QueryOptions,
    ) -> Result<DisplayResult, QueryError> {
        self.inner.query_range_display_opts(
            &self.restrict(expr),
            start_s,
            end_s,
            step_s,
            opts,
            qopts,
        )
    }
    fn query(&self, expr: &str, time: Option<f64>) -> Result<QueryResult, QueryError> {
        self.inner.query(&self.restrict(expr), time)
    }
    fn columns(&self, query: &str) -> Result<HashSet<String>, QueryError> {
        self.inner.columns(&self.restrict(query))
    }
    fn time_range(&self) -> Option<(f64, f64)> {
        self.scope
            .header
            .time_range_ns
            .map(|(a, b)| (a as f64 / 1e9, b as f64 / 1e9))
    }
    fn time_range_ns(&self) -> Option<(u64, u64)> {
        self.scope.header.time_range_ns
    }
    fn interval(&self) -> f64 {
        self.scope.header.interval
    }
    fn source(&self) -> String {
        self.scope.header.source.clone()
    }
    fn version(&self) -> String {
        self.scope.header.version.clone()
    }
    fn filename(&self) -> Option<String> {
        self.inner.filename()
    }
    fn metadata_get(&self, key: &str) -> Option<String> {
        self.scope.metadata.get(key).cloned()
    }
    fn file_metadata(&self) -> HashMap<String, String> {
        self.scope.metadata.clone()
    }
    fn counter_names(&self) -> Vec<String> {
        self.names(Kind::Counter)
    }
    fn gauge_names(&self) -> Vec<String> {
        self.names(Kind::Gauge)
    }
    fn histogram_names(&self) -> Vec<String> {
        self.names(Kind::Histogram)
    }
    fn counter_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.labels(Kind::Counter, name)
    }
    fn gauge_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.labels(Kind::Gauge, name)
    }
    fn histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.labels(Kind::Histogram, name)
    }
    fn gauge_histogram_names(&self) -> Vec<String> {
        self.names(Kind::GaugeHistogram)
    }
    fn gauge_histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.labels(Kind::GaugeHistogram, name)
    }
    fn sample_timestamps(&self) -> Vec<u64> {
        self.inner.sample_timestamps()
    }
}

// ── A parquet file's source as a series label ───────────────────────────

/// The `source` label [`label_parquet_by_source`] gives `reader`'s series:
/// the file's `source` when it names one source and no series carries a
/// `source` label of its own; `None` otherwise. A parquet file converted to
/// `.dendro` gets its source labelled with it, so it presents as the file
/// does.
pub fn parquet_source_label(reader: &metriken_query::ParquetReader) -> Option<String> {
    let source = MetricsSource::source(reader);
    if !crate::source_templates::is_one_source_name(&source) {
        return None;
    }
    let carries_source =
        |labels: Vec<BTreeMap<String, String>>| labels.iter().any(|l| l.contains_key("source"));
    let native = reader
        .counter_names()
        .iter()
        .any(|n| carries_source(reader.counter_labels(n)))
        || reader
            .gauge_names()
            .iter()
            .any(|n| carries_source(reader.gauge_labels(n)))
        || reader
            .histogram_names()
            .iter()
            .any(|n| carries_source(reader.histogram_labels(n)));
    (!native).then_some(source)
}

/// `reader` with its file-level `source` as a label on every series, as an
/// archive recording's series carry its recording's labels, so a template
/// written for an archive (`{source="llm-perf"}`) matches the same producer's
/// parquet. A selector on `source` is decided against the file's value.
///
/// `reader` unchanged when its `source` does not name one source (a
/// combined parquet's is a JSON array, and its columns carry their own
/// `source`), or when any of its series already carries a `source` label: the
/// reader's composition replaces a series' own value rather than keeping it
/// under `exported_source`, as an archive does, and the second needs
/// metriken-storage's `LabeledSource` behind a `ParquetReader`.
pub fn label_parquet_by_source(
    reader: metriken_query::ParquetReader,
) -> metriken_query::ParquetReader {
    let Some(source) = parquet_source_label(&reader) else {
        return reader;
    };
    let filename = reader.filename().map(str::to_string);
    let labels: BTreeMap<String, String> = [("source".to_string(), source)].into();
    let reader = std::sync::Arc::new(reader);
    let mut builder = metriken_query::ParquetBuilder::new()
        .reader_labeled(std::sync::Arc::clone(&reader), labels);
    if let Some(name) = filename {
        builder = builder.filename(name);
    }
    match builder.build() {
        Ok(labelled) => labelled,
        // One source composed from an open reader does no I/O and has a
        // member, so this is not reached; keep the reader as it was.
        Err(_) => std::sync::Arc::try_unwrap(reader).unwrap_or_else(|_| unreachable!()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store whose rows span `ticks` seconds and whose metadata is `meta`.
    fn store(ticks: std::ops::Range<u64>, meta: &[(&str, &str)]) -> metriken_query::MemoryStore {
        let store = metriken_query::MemoryStore::builder().build();
        if !ticks.is_empty() {
            let ts: Vec<u64> = ticks.clone().map(|t| t * 1_000_000_000).collect();
            let values: Vec<u64> = ticks.map(|t| t * 10).collect();
            store
                .insert_counter_series("x", BTreeMap::<String, String>::new(), ts, values, None)
                .unwrap();
        }
        for (k, v) in meta {
            store.set_metadata(*k, *v);
        }
        store
    }

    /// `Routed` and `Scoped` forward the two gauge-histogram listings, which
    /// `MetricsSource` defaults to empty: a gauge histogram held by the
    /// source shows through both, and `Scoped` keeps only its recording's
    /// series.
    #[test]
    fn a_gauge_histogram_shows_through_routed_and_scoped() {
        let host = |h: &str| -> BTreeMap<String, String> {
            [("host".to_string(), h.to_string())].into_iter().collect()
        };
        let store = metriken_query::MemoryStore::builder().build();
        for h in ["a", "b"] {
            store
                .insert_gauge_histogram_series(
                    "depth",
                    host(h),
                    ::histogram::Config::new(2, 8).unwrap(),
                    vec![1_000_000_000],
                    vec![metriken_storage::HistogramSnapshot {
                        index: vec![1],
                        count: vec![1],
                    }],
                )
                .unwrap();
        }
        let store: Arc<dyn MetricsSource> = Arc::new(store);
        assert_eq!(store.gauge_histogram_names(), ["depth"]);

        let routed = Routed::new(Arc::clone(&store), Vec::new());
        assert_eq!(routed.gauge_histogram_names(), ["depth"]);
        assert_eq!(routed.gauge_histogram_labels("depth").len(), 2);

        let matchers = scope_matchers(&[host("a"), host("b")]);
        let scope = Scope {
            matchers: matchers[0].clone(),
            header: Header::of(&*store),
            metadata: HashMap::new(),
            catalog: None,
        };
        let scoped = Scoped::new(&*store, &scope);
        assert_eq!(scoped.gauge_histogram_names(), ["depth"]);
        assert_eq!(scoped.gauge_histogram_labels("depth"), [host("a")]);
    }

    /// The baseline capture spans its own rows, which the experiment's are
    /// aligned against; only a baseline with none takes the span of the
    /// recordings beside it.
    #[test]
    fn routed_spans_the_baseline_unless_it_is_empty() {
        let s = 1_000_000_000;
        let beside: Vec<(String, Arc<dyn MetricsSource>)> =
            vec![("h".into(), Arc::new(store(0..30, &[])))];
        let routed = Routed::new(Arc::new(store(10..20, &[])), beside.clone());
        assert_eq!(routed.time_range_ns(), Some((10 * s, 19 * s)));
        assert_eq!(routed.time_range(), Some((10.0, 19.0)));
        let empty = Routed::new(Arc::new(store(0..0, &[])), beside);
        assert_eq!(empty.time_range_ns(), Some((0, 29 * s)));
        assert_eq!(empty.time_range(), Some((0.0, 29.0)));
    }

    /// A report's selection, marker and events come together from the first
    /// recording that carries a selection: the baseline's own first.
    #[test]
    fn report_markers_come_from_one_recording_baseline_first() {
        let baseline = store(0..2, &[("selection", "B"), ("report", "trimmed")]);
        let agent = store(
            0..2,
            &[("selection", "A"), ("events", "{}"), ("report", "trimmed")],
        );
        let none = store(0..2, &[("events", "stray")]);
        // A marker without a selection is not a report's.
        let marker_only = store(0..2, &[("report", "trimmed"), ("events", "own")]);
        let markers = report_markers(&[&none, &marker_only, &baseline, &agent]);
        assert_eq!(
            markers,
            [
                ("selection", "B".to_string()),
                ("report", "trimmed".to_string())
            ]
        );
        assert_eq!(report_markers(&[&baseline, &agent]), markers);
        assert_eq!(
            report_markers(&[&agent, &baseline]),
            [
                ("selection", "A".to_string()),
                ("report", "trimmed".to_string()),
                ("events", "{}".to_string())
            ]
        );
        assert!(report_markers(&[&none]).is_empty());
        assert_eq!(marker(&markers, "selection").as_deref(), Some("B"));
        assert_eq!(marker(&markers, "events"), None);
        let meta = with_report_markers(r#"{"source":"s"}"#, &report_markers(&[&agent]));
        let meta: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(meta["events"], serde_json::json!({}));
        assert_eq!(meta["selection"], "A");
    }

    fn m(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_comparison_is_asked_for_or_labelled() {
        let one_run = [
            m(&[("source", "rezolus"), ("host", "a")]),
            m(&[("source", "svc"), ("host", "a")]),
        ];
        assert_eq!(layout(&one_run, false), Layout::Combined);
        assert_eq!(layout(&one_run, true), Layout::Compare);
        assert_eq!(layout(&one_run[..1], false), Layout::Combined);
        let armed = [
            m(&[("source", "rezolus"), ("arm", "baseline")]),
            m(&[("source", "rezolus"), ("arm", "experiment")]),
        ];
        assert_eq!(layout(&armed, false), Layout::Compare);
        let one_armed = [m(&[("arm", "baseline")]), m(&[("host", "b")])];
        assert_eq!(layout(&one_armed, false), Layout::Compare);
    }

    /// The slots come from the arms, not the archive's order.
    #[test]
    fn arms_fill_the_slots() {
        let slots = |arms: &[Option<&str>]| {
            let labels: Vec<_> = arms
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let mut l = m(&[("source", "rezolus")]);
                    l.insert("n".into(), i.to_string());
                    if let Some(a) = a {
                        l.insert("arm".into(), a.to_string());
                    }
                    l
                })
                .collect();
            arm_slots(&labels)
        };
        let at = |r: Result<ArmSlots, String>| {
            let r = r.unwrap();
            (r.baseline, r.experiment)
        };
        assert_eq!(
            at(slots(&[Some("experiment"), Some("baseline")])),
            (1, Some(0))
        );
        assert_eq!(
            at(slots(&[Some("Baseline"), Some(" EXPERIMENT ")])),
            (0, Some(1))
        );
        assert_eq!(at(slots(&[None, None])), (0, Some(1)));
        assert_eq!(at(slots(&[Some("experiment")])), (0, None));
        // One standard arm decides both.
        assert_eq!(
            at(slots(&[Some("control"), Some("baseline")])),
            (1, Some(0))
        );
        assert_eq!(
            at(slots(&[Some("experiment"), Some("control")])),
            (1, Some(0))
        );
        let named = slots(&[Some("redis"), Some("valkey")]).unwrap();
        assert_eq!((named.baseline, named.experiment), (0, Some(1)));
        assert!(
            named
                .note
                .unwrap()
                .contains("arm=redis, the archive's first")
        );
        assert!(
            slots(&[Some("control"), Some("baseline")])
                .unwrap()
                .note
                .is_none()
        );
        // Whitespace around a standard arm still names it.
        assert!(
            slots(&[Some(" Baseline "), Some("control")])
                .unwrap()
                .note
                .is_none()
        );
        // Two arms of a service and an agent without one: the arms fill the
        // slots, and the note says nothing (the viewer names the agent).
        let concurrent = slots(&[Some("baseline"), Some("experiment"), None]).unwrap();
        assert_eq!((concurrent.baseline, concurrent.experiment), (0, Some(1)));
        assert_eq!(concurrent.note, None);
        let agent_first = slots(&[None, Some("experiment"), Some("baseline")]).unwrap();
        assert_eq!((agent_first.baseline, agent_first.experiment), (2, Some(1)));
        for refused in [
            &[Some("baseline"), None][..],
            &[None, Some("experiment"), None][..],
            &[Some("baseline"), Some("baseline")][..],
            &[Some("baseline"), Some("Baseline")][..],
            &[Some("baseline"), Some("experiment"), Some("c")][..],
        ] {
            assert!(slots(refused).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn matchers_are_the_labels_that_differ() {
        let labels = [
            m(&[("source", "rezolus"), ("host", "a"), ("run", "1")]),
            m(&[("source", "rezolus"), ("host", "b"), ("run", "1")]),
            m(&[("source", "svc"), ("host", "a"), ("run", "1")]),
        ];
        assert_eq!(
            scope_matchers(&labels),
            [
                eqs(&[("host", "a"), ("source", "rezolus")]),
                eqs(&[("host", "b"), ("source", "rezolus")]),
                eqs(&[("host", "a"), ("source", "svc")]),
            ]
        );
        assert_eq!(scope_matchers(&labels[..1]), [eqs(&[])]);
        let twins = [
            m(&[("source", "rezolus"), (RECORDING_LABEL, "0")]),
            m(&[("source", "rezolus"), (RECORDING_LABEL, "1")]),
        ];
        assert_eq!(
            scope_matchers(&twins),
            [
                eqs(&[(RECORDING_LABEL, "0")]),
                eqs(&[(RECORDING_LABEL, "1")])
            ]
        );
    }

    /// A recording that lacks a label another carries is told apart by the
    /// other's value: `role!="x"` (the engine's `role=""` matches nothing and
    /// `role!~".+"` everything). Each scope selects its recording alone.
    #[test]
    fn a_recording_lacking_a_label_is_selected_by_its_absence() {
        let pair = [
            m(&[("source", "rezolus"), ("host", "a")]),
            m(&[("source", "rezolus"), ("host", "a"), ("role", "x")]),
        ];
        let scopes = scope_matchers(&pair);
        assert_eq!(
            scopes,
            [vec![Matcher::ne("role", "x")], eqs(&[("role", "x")])]
        );
        let three = [
            m(&[("host", "a"), ("role", "x")]),
            m(&[("host", "b")]),
            m(&[("host", "c"), ("role", "y")]),
            m(&[("host", "d")]),
        ];
        let scopes = scope_matchers(&three);
        assert_eq!(
            scopes[1],
            [
                Matcher::eq("host", "b"),
                Matcher {
                    label: "role".into(),
                    op: MatchOp::NotRegex,
                    value: "x|y".into(),
                },
            ]
        );
        for (labels, scope) in [(&pair[..], scope_matchers(&pair)), (&three[..], scopes)] {
            for (i, matchers) in scope.iter().enumerate() {
                let selected: Vec<usize> = (0..labels.len())
                    .filter(|j| matchers.iter().all(|m| m.matches(&labels[*j])))
                    .collect();
                assert_eq!(selected, [i], "{matchers:?}");
            }
        }
        assert_eq!(
            restrict_query("cpu_cores", &scope_matchers(&pair)[0]),
            r#"cpu_cores{role!="x"}"#
        );
    }

    #[test]
    fn instances_are_named_by_host_and_keyed_for_a_route() {
        let names = instance_names(&[
            m(&[("source", "rezolus"), ("host", "Web 01")]),
            m(&[("source", "rezolus"), ("host", "web/01")]),
        ]);
        assert_eq!(names, ["Web 01", "web/01"]);
        assert_eq!(instance_keys(&names), ["web-01", "web-01-2"]);
        let names = instance_names(&[m(&[("source", "a")]), m(&[("source", "b")])]);
        assert_eq!(names, ["a", "b"]);
        let names = instance_names(&[
            m(&[("source", "a"), (RECORDING_LABEL, "0")]),
            m(&[("source", "a"), (RECORDING_LABEL, "3")]),
        ]);
        assert_eq!(names, ["recording 1", "recording 2"]);
        // A host two recordings share names neither alone.
        let names = instance_names(&[
            m(&[("source", "rezolus"), ("host", "a")]),
            m(&[("source", "svc"), ("host", "a")]),
            m(&[("source", "rezolus"), ("host", "b")]),
        ]);
        assert_eq!(names, ["a/rezolus", "a/svc", "b/rezolus"]);
        assert_eq!(base_route("/cpu@web-01"), "/cpu");
        assert_eq!(base_route("/cpu"), "/cpu");
    }

    fn eqs(pairs: &[(&str, &str)]) -> Vec<Matcher> {
        pairs.iter().map(|(k, v)| Matcher::eq(k, v)).collect()
    }

    fn h1() -> Vec<Matcher> {
        eqs(&[("host", "h1")])
    }

    #[test]
    fn every_selector_gains_the_matchers() {
        let cases = [
            ("cpu_cores", r#"cpu_cores{host="h1"}"#),
            (
                "sum(irate(cpu_usage[5m])) / cpu_cores",
                r#"sum(irate(cpu_usage{host="h1"}[5m])) / cpu_cores{host="h1"}"#,
            ),
            (
                r#"sum by (id, state) (irate(cpu_usage{state="user"}[5m]))"#,
                r#"sum by (id, state) (irate(cpu_usage{state="user",host="h1"}[5m]))"#,
            ),
            (
                r#"sum(irate(x[1m])) by (cpu) / on(cpu) group_left(id) y"#,
                r#"sum(irate(x{host="h1"}[1m])) by (cpu) / on(cpu) group_left(id) y{host="h1"}"#,
            ),
            (
                "histogram_quantiles([0.5, 0.99], scheduler_runqueue_latency)",
                r#"histogram_quantiles([0.5, 0.99], scheduler_runqueue_latency{host="h1"})"#,
            ),
            (
                r#"sum by (op) (histogram_irate(blockio_latency{op="read"}))"#,
                r#"sum by (op) (histogram_irate(blockio_latency{op="read",host="h1"}))"#,
            ),
            (
                r#"sum(irate({__name__="redis/cmds",source="valkey"}[5s]))"#,
                r#"sum(irate({__name__="redis/cmds",source="valkey",host="h1"}[5s]))"#,
            ),
            ("x{}", r#"x{host="h1"}"#),
            (r#"x{a="b",}"#, r#"x{a="b",host="h1"}"#),
            ("x / 1e9 + 0x1F * .5", r#"x{host="h1"} / 1e9 + 0x1F * .5"#),
            (
                "x offset 5m > bool 1e-3",
                r#"x{host="h1"} offset 5m > bool 1e-3"#,
            ),
            (
                "rate(x[5m:1m]) and y",
                r#"rate(x{host="h1"}[5m:1m]) and y{host="h1"}"#,
            ),
            (
                r#"sum without (cpu) (x) unless count by (id) (y)"#,
                r#"sum without (cpu) (x{host="h1"}) unless count by (id) (y{host="h1"})"#,
            ),
            (
                r#"label_replace(cpu_usage, "dst", "{$1}", "src", "(.*)")"#,
                r#"label_replace(cpu_usage{host="h1"}, "dst", "{$1}", "src", "(.*)")"#,
            ),
            (
                r#"sum(irate(cgroup_syscall{op="read",name=~"__SELECTED_CGROUPS__"}[5m]))"#,
                r#"sum(irate(cgroup_syscall{op="read",name=~"__SELECTED_CGROUPS__",host="h1"}[5m]))"#,
            ),
            ("topk(5, x)", r#"topk(5, x{host="h1"})"#),
            ("sum (x)", r#"sum (x{host="h1"})"#),
            ("count", r#"count{host="h1"}"#),
        ];
        for (query, want) in cases {
            assert_eq!(restrict_query(query, &h1()), want, "{query}");
        }
    }

    #[test]
    fn a_selector_keeps_its_own_matcher_on_a_key() {
        let both = eqs(&[("host", "h1"), ("source", "svc")]);
        assert_eq!(
            restrict_query(r#"up{source="vllm"} + down"#, &both),
            r#"up{source="vllm",host="h1"} + down{host="h1",source="svc"}"#
        );
        assert_eq!(
            restrict_query(r#"up{source="svc",host="h1"}"#, &both),
            r#"up{source="svc",host="h1"}"#
        );
        // Restricting twice changes nothing.
        let once = restrict_query("sum(irate(x[5m])) / y", &both);
        assert_eq!(restrict_query(&once, &both), once);
        // Only `=` with a value stands in for the scope's matcher; any other
        // matcher on the label is kept and the scope's added too.
        assert_eq!(
            restrict_query(r#"x{host!=""} + y{host=~".+"} + z{host=""}"#, &h1()),
            r#"x{host!="",host="h1"} + y{host=~".+",host="h1"} + z{host="",host="h1"}"#
        );
    }

    /// Each `or` branch of a selector's braces gains the matchers, and a `#`
    /// comment is skipped, quotes and braces in it included.
    #[test]
    fn or_branches_and_comments() {
        assert_eq!(
            restrict_query(r#"x{a="1" or b="2"}"#, &h1()),
            r#"x{a="1",host="h1" or b="2",host="h1"}"#
        );
        assert_eq!(
            restrict_query(r#"x{host="h9" or b="2"}"#, &h1()),
            r#"x{host="h9" or b="2",host="h1"}"#
        );
        // `or` as a label value or name part is not a separator.
        assert_eq!(
            restrict_query(r#"x{color="or",orient="v"}"#, &h1()),
            r#"x{color="or",orient="v",host="h1"}"#
        );
        assert_eq!(
            restrict_query("sum(x) # don't {count} y\n/ z", &h1()),
            "sum(x{host=\"h1\"}) # don't {count} y\n/ z{host=\"h1\"}"
        );
    }

    fn fixture(name: &str) -> metriken_query::ParquetReader {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../site/viewer/data")
            .join(name);
        metriken_query::ParquetReader::open(&path).unwrap()
    }

    fn series(data: &dyn MetricsSource, query: &str) -> usize {
        let (start, end) = data.time_range().unwrap();
        match data.query_range(query, start, end, 1.0).unwrap() {
            metriken_query::QueryResult::Matrix { result } => result.len(),
            other => panic!("{query}: {other:?}"),
        }
    }

    /// A single-source parquet's series carry its `source`, so a matcher on
    /// it selects them, and one naming another source selects none.
    #[test]
    fn a_parquets_source_is_a_series_label() {
        let plain = fixture("demo.parquet");
        assert_eq!(series(&plain, r#"cpu_cores{source="rezolus"}"#), 0);
        let labelled = label_parquet_by_source(fixture("demo.parquet"));
        assert_eq!(series(&labelled, r#"cpu_cores{source="rezolus"}"#), 1);
        assert_eq!(series(&labelled, r#"cpu_cores{source="other"}"#), 0);
        assert!(
            labelled
                .gauge_labels("cpu_cores")
                .iter()
                .all(|l| l.get("source").map(String::as_str) == Some("rezolus"))
        );
        assert_eq!(labelled.filename(), plain.filename());
        assert_eq!(
            MetricsSource::source(&labelled),
            MetricsSource::source(&plain)
        );
        assert_eq!(labelled.time_range_ns(), plain.time_range_ns());
    }

    /// A combined parquet (its `source` a list, each column its own), and a
    /// parquet some of whose series carry `source` themselves, are left as
    /// they are.
    #[test]
    fn a_parquet_with_its_own_source_labels_is_left_alone() {
        for name in ["vllm.parquet", "simple_capture.parquet"] {
            let plain = fixture(name);
            let labelled = label_parquet_by_source(fixture(name));
            for n in plain.counter_names() {
                assert_eq!(
                    labelled.counter_labels(&n),
                    plain.counter_labels(&n),
                    "{name} {n}"
                );
            }
        }
    }

    #[test]
    fn values_are_escaped_and_no_matchers_change_nothing() {
        let odd = eqs(&[("host", "a\"b\\c")]);
        assert_eq!(restrict_query("x", &odd), r#"x{host="a\"b\\c"}"#);
        assert_eq!(restrict_query("sum(x)", &[]), "sum(x)");
    }
}
