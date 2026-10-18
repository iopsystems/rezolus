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

use metriken_query::{
    DisplayOptions, DisplayResult, MetricsSource, QueryError, QueryOptions, QueryResult,
};

/// The recording label that names a comparison's arm.
pub const ARM_LABEL: &str = "arm";

/// The reserved label that tells apart recordings whose label sets are
/// equal (metriken-storage's `labeled::RECORDING_LABEL`).
pub const RECORDING_LABEL: &str = "__recording__";

/// The View metadata key a restricted section carries its matchers under,
/// so the page can restrict the queries it builds itself (the cgroup
/// discovery) the same way.
pub const SCOPE_KEY: &str = "scope";

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

/// What [`layout`] reads about one recording.
#[derive(Clone, Debug, Default)]
pub struct RecordingSummary {
    /// The recording's labels, as the archive's catalog holds them.
    pub labels: BTreeMap<String, String>,
    /// The span of its rows in nanoseconds, `None` when it holds none.
    pub time_range_ns: Option<(u64, u64)>,
}

/// Whether an archive opens combined or as a comparison.
///
/// A comparison when `compare_requested` (the server's `--baseline` or
/// `--experiment`, the WASM registry's `set_compare_requested`), when any
/// recording carries an [`ARM_LABEL`] label, or when the archive holds two
/// or more recordings with rows and no two of their time ranges overlap:
/// recordings of different runs, which is what `recording combine a.rez b.rez`
/// assembles and which no `arm` label marks. Combined otherwise, a single
/// recording included.
pub fn layout(recordings: &[RecordingSummary], compare_requested: bool) -> Layout {
    if compare_requested || recordings.iter().any(|r| r.labels.contains_key(ARM_LABEL)) {
        return Layout::Compare;
    }
    let mut spans: Vec<(u64, u64)> = recordings.iter().filter_map(|r| r.time_range_ns).collect();
    if spans.len() < 2 {
        return Layout::Combined;
    }
    spans.sort_unstable();
    let disjoint = spans.windows(2).all(|w| w[0].1 < w[1].0);
    if disjoint {
        Layout::Compare
    } else {
        Layout::Combined
    }
}

/// The matchers that select each recording's series out of a combined
/// source: for each of `series_labels` (the labels each recording's series
/// carry, `__recording__` included where it applies), its values for the
/// keys whose values are not the same for every recording. A key a
/// recording lacks is left out of its matchers. One recording, or recordings
/// whose series carry equal labels, get no matchers.
pub fn scope_matchers(series_labels: &[BTreeMap<String, String>]) -> Vec<BTreeMap<String, String>> {
    let keys = discriminating_keys(series_labels);
    series_labels
        .iter()
        .map(|labels| {
            keys.iter()
                .filter_map(|k| labels.get(*k).map(|v| ((*k).to_string(), v.clone())))
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
/// value when hosts differ, else the differing values joined by `/`, else
/// `recording N` (1-based).
pub fn instance_names(labels: &[BTreeMap<String, String>]) -> Vec<String> {
    let keys = discriminating_keys(labels);
    labels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if keys.contains(&"host")
                && let Some(host) = l.get("host")
            {
                return host.clone();
            }
            let values: Vec<&str> = keys
                .iter()
                .filter(|k| **k != RECORDING_LABEL)
                .filter_map(|k| l.get(*k).map(String::as_str))
                .collect();
            if values.is_empty() {
                format!("recording {}", i + 1)
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
/// name gains `{k="v",...}`, and a selector's braces (`x{a="b"}`,
/// `{__name__="x"}`) gain each matcher on a label they do not already
/// match. A selector that already has a matcher on one of the keys keeps
/// its own for that key, so a query written for a source (`{source="vllm"}`)
/// or restricted before (a service's throughput query placed into another
/// source's Overview) is left as it asks.
///
/// The query is read by its tokens rather than parsed, so it works for the
/// histogram functions PromQL does not have (`histogram_quantiles([0.5],
/// x)`), and every character outside the inserted matchers is kept as
/// written. Strings, `[...]` ranges and quantile lists, the label lists of
/// `by`, `without`, `on`, `ignoring`, `group_left` and `group_right`, and an
/// `offset` duration are skipped; an identifier followed by `(` is a
/// function or an aggregation.
pub fn restrict_query(query: &str, matchers: &BTreeMap<String, String>) -> String {
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

/// The braces of a selector, `inner` being what is between them, with each
/// matcher on a label `inner` does not already match added.
fn restrict_braces(inner: &str, matchers: &BTreeMap<String, String>) -> String {
    let named = matched_labels(inner);
    let missing: Vec<(&String, &String)> = matchers
        .iter()
        .filter(|(k, _)| !named.contains(k.as_str()))
        .collect();
    let mut out = String::with_capacity(inner.len() + 32);
    out.push('{');
    if missing.is_empty() {
        out.push_str(inner);
        out.push('}');
        return out;
    }
    let kept = inner.trim_end();
    out.push_str(kept);
    if !kept.trim().is_empty() && !kept.ends_with(',') {
        out.push(',');
    }
    push_matchers(&mut out, missing.into_iter());
    out.push('}');
    out
}

/// The label names matched between a selector's braces.
fn matched_labels(inner: &str) -> HashSet<&str> {
    let bytes = inner.as_bytes();
    let mut names = HashSet::new();
    let mut i = 0;
    let mut at_name = true;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' || c == b'\'' || c == b'`' {
            i = skip_string(bytes, i);
            at_name = false;
        } else if c == b',' {
            at_name = true;
            i += 1;
        } else if at_name && is_ident_start(c) {
            let end = skip_ident(bytes, i);
            names.insert(&inner[i..end]);
            at_name = false;
            i = end;
        } else {
            if !c.is_ascii_whitespace() {
                at_name = false;
            }
            i += 1;
        }
    }
    names
}

fn push_matchers<'a>(out: &mut String, matchers: impl Iterator<Item = (&'a String, &'a String)>) {
    for (n, (k, v)) in matchers.enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push_str(k);
        out.push_str("=\"");
        for c in v.chars() {
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

/// One recording's part of a combined source: its matchers, header and
/// metadata.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    /// What selects the recording's series; see [`scope_matchers`].
    pub matchers: BTreeMap<String, String>,
    pub header: Header,
    pub metadata: HashMap<String, String>,
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
            .filter(|l| self.scope.matchers.iter().all(|(k, v)| l.get(k) == Some(v)))
            .collect()
    }

    fn names(
        &self,
        names: Vec<String>,
        labels: impl Fn(&str) -> Vec<BTreeMap<String, String>>,
    ) -> Vec<String> {
        if self.scope.matchers.is_empty() {
            return names;
        }
        names
            .into_iter()
            .filter(|n| !self.keep(labels(n)).is_empty())
            .collect()
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
        self.names(self.inner.counter_names(), |n| self.inner.counter_labels(n))
    }
    fn gauge_names(&self) -> Vec<String> {
        self.names(self.inner.gauge_names(), |n| self.inner.gauge_labels(n))
    }
    fn histogram_names(&self) -> Vec<String> {
        self.names(self.inner.histogram_names(), |n| {
            self.inner.histogram_labels(n)
        })
    }
    fn counter_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.keep(self.inner.counter_labels(name))
    }
    fn gauge_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.keep(self.inner.gauge_labels(name))
    }
    fn histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.keep(self.inner.histogram_labels(name))
    }
    fn sample_timestamps(&self) -> Vec<u64> {
        self.inner.sample_timestamps()
    }
}

// ── A parquet file's source as a series label ───────────────────────────

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
    let source = MetricsSource::source(&reader);
    if !crate::source_templates::is_one_source_name(&source) {
        return reader;
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
    if native {
        return reader;
    }
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

    fn m(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn rec(pairs: &[(&str, &str)], span: Option<(u64, u64)>) -> RecordingSummary {
        RecordingSummary {
            labels: m(pairs),
            time_range_ns: span,
        }
    }

    #[test]
    fn one_run_opens_combined_and_a_request_or_an_arm_opens_a_comparison() {
        let one_run = [
            rec(&[("source", "rezolus"), ("host", "a")], Some((0, 100))),
            rec(&[("source", "svc"), ("host", "a")], Some((10, 90))),
        ];
        assert_eq!(layout(&one_run, false), Layout::Combined);
        assert_eq!(layout(&one_run, true), Layout::Compare);
        assert_eq!(layout(&one_run[..1], false), Layout::Combined);
        let armed = [
            rec(
                &[("source", "rezolus"), ("arm", "baseline")],
                Some((0, 100)),
            ),
            rec(
                &[("source", "rezolus"), ("arm", "experiment")],
                Some((0, 100)),
            ),
        ];
        assert_eq!(layout(&armed, false), Layout::Compare);
    }

    /// Two runs at different times (`recording combine a.rez b.rez`) open
    /// as a comparison; a recording with no rows decides nothing.
    #[test]
    fn recordings_of_different_runs_open_as_a_comparison() {
        let runs = [
            rec(&[("source", "rezolus")], Some((0, 100))),
            rec(&[("source", "rezolus")], Some((200, 300))),
        ];
        assert_eq!(layout(&runs, false), Layout::Compare);
        let touching = [
            rec(&[("source", "rezolus")], Some((0, 200))),
            rec(&[("source", "rezolus")], Some((200, 300))),
        ];
        assert_eq!(layout(&touching, false), Layout::Combined);
        let one_empty = [
            rec(&[("source", "rezolus")], Some((0, 100))),
            rec(&[("source", "svc")], None),
        ];
        assert_eq!(layout(&one_empty, false), Layout::Combined);
        let three = [
            rec(&[("host", "a")], Some((0, 100))),
            rec(&[("host", "b")], Some((50, 150))),
            rec(&[("host", "c")], Some((400, 500))),
        ];
        assert_eq!(layout(&three, false), Layout::Combined);
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
                m(&[("source", "rezolus"), ("host", "a")]),
                m(&[("source", "rezolus"), ("host", "b")]),
                m(&[("source", "svc"), ("host", "a")]),
            ]
        );
        assert_eq!(scope_matchers(&labels[..1]), [m(&[])]);
        let twins = [
            m(&[("source", "rezolus"), (RECORDING_LABEL, "0")]),
            m(&[("source", "rezolus"), (RECORDING_LABEL, "1")]),
        ];
        assert_eq!(
            scope_matchers(&twins),
            [m(&[(RECORDING_LABEL, "0")]), m(&[(RECORDING_LABEL, "1")])]
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
        assert_eq!(base_route("/cpu@web-01"), "/cpu");
        assert_eq!(base_route("/cpu"), "/cpu");
    }

    fn h1() -> BTreeMap<String, String> {
        m(&[("host", "h1")])
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
        let both = m(&[("host", "h1"), ("source", "svc")]);
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
        let odd = m(&[("host", "a\"b\\c")]);
        assert_eq!(restrict_query("x", &odd), r#"x{host="a\"b\\c"}"#);
        assert_eq!(restrict_query("sum(x)", &m(&[])), "sum(x)");
    }
}
