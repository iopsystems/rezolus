//! The queries the viewer's dashboards issue (the built-in sections rendered
//! from the embedded section template, as the viewers render them), with
//! every embedded service template's section and the inference-library
//! category, run against a dendro archive that holds every metric they
//! name. The dashboards are generated against that archive, and the metrics their
//! data gates ask about (per-CPU and per-operation plots, whole sections such
//! as ext4) are added to it, so gated plots are included. Its series carry
//! the label values the selectors match. A query that errors or returns no
//! series fails the test, and so does a gate that asks for a metric as a
//! kind no plot reads it as.
//!
//! Not reached: plots gated on a label value the generator compares with a list
//! of its own (Intel GPU engine classes), since the fixture's label values are
//! `0` and `1`; and the `blockio_latency` name a pre-rename recording carries,
//! read only when `blockio_device_latency` is absent. Cgroup queries are sent
//! with the `__SELECTED_CGROUPS__` placeholder, which the viewer replaces with
//! the selected pattern; the fixture holds a cgroup of that name.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use dashboard::dashboard::{
    build_dashboard_context, build_template_context, generate_section, render_section,
};
use dashboard::source_templates::embedded_dashboard_template;
use dashboard::{ServiceExtension, TemplateRegistry};
use metriken::Window;
use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc, Snapshot, SnapshotV3};
use metriken_query::MetricsSource;

use crate::hindsight::buffer::HindsightBuffer;
use crate::hindsight::buffer::Seed;
use dendro::seal::SealPolicy;

const ANCHOR: u64 = 1_700_000_000_000_000_000;
const SECOND: u64 = 1_000_000_000;
const TICKS: u64 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Counter,
    Gauge,
    Histogram,
}

/// A query and where it came from.
struct Use {
    origin: String,
    query: String,
    /// The plot draws a histogram: the viewer wraps the query in a
    /// percentile or heatmap function.
    histogram_plot: bool,
}

/// What the archive holds: each metric's kind and the label sets of its
/// series.
#[derive(Default)]
struct Fixture {
    kinds: BTreeMap<String, Kind>,
    series: BTreeMap<String, BTreeSet<BTreeMap<String, String>>>,
    /// Metrics added for a data gate, whose kind is a guess until a query
    /// reads them.
    guessed: BTreeSet<String>,
}

/// What the generators asked about one metric: the kinds its labels were
/// asked for as, in order (`has_metric` asks each kind in turn), the kinds
/// it was asked to be (`has_counter` and the like, which a metric of another
/// kind fails), and the label keys asked for.
#[derive(Clone, Default)]
struct Asked {
    kinds: Vec<Kind>,
    exactly: BTreeSet<Kind>,
    keys: BTreeSet<String>,
}

impl Asked {
    fn merge(&mut self, other: &Asked) {
        for kind in &other.kinds {
            if !self.kinds.contains(kind) {
                self.kinds.push(*kind);
            }
        }
        self.exactly.extend(other.exactly.iter().copied());
        self.keys.extend(other.keys.iter().cloned());
    }
}

/// A `MetricsSource` that answers from `inner` and records each metric a
/// dashboard generator asks about, with the label keys it asks for and
/// the kind it asked as.
struct Probe {
    inner: Arc<dyn MetricsSource>,
    asked: std::sync::Mutex<BTreeMap<String, Asked>>,
}

impl Probe {
    fn note(&self, name: &str, kind: Option<Kind>, key: Option<&str>) {
        let mut asked = self.asked.lock().unwrap();
        let entry = asked.entry(name.to_string()).or_default();
        if let Some(kind) = kind {
            if !entry.kinds.contains(&kind) {
                entry.kinds.push(kind);
            }
        }
        if let Some(key) = key {
            entry.keys.insert(key.to_string());
        }
    }

    fn note_exactly(&self, name: &str, kind: Kind) {
        self.note(name, Some(kind), None);
        let mut asked = self.asked.lock().unwrap();
        asked.get_mut(name).unwrap().exactly.insert(kind);
    }
}

impl MetricsSource for Probe {
    fn query_range_opts(
        &self,
        expr: &str,
        start_s: f64,
        end_s: f64,
        step_s: f64,
        opts: &metriken_query::QueryOptions,
    ) -> Result<metriken_query::QueryResult, metriken_query::QueryError> {
        self.inner
            .query_range_opts(expr, start_s, end_s, step_s, opts)
    }
    fn query(
        &self,
        expr: &str,
        time: Option<f64>,
    ) -> Result<metriken_query::QueryResult, metriken_query::QueryError> {
        self.inner.query(expr, time)
    }
    fn columns(&self, query: &str) -> Result<HashSet<String>, metriken_query::QueryError> {
        self.inner.columns(query)
    }
    fn time_range(&self) -> Option<(f64, f64)> {
        self.inner.time_range()
    }
    fn time_range_ns(&self) -> Option<(u64, u64)> {
        self.inner.time_range_ns()
    }
    fn interval(&self) -> f64 {
        self.inner.interval()
    }
    fn source(&self) -> String {
        self.inner.source()
    }
    fn version(&self) -> String {
        self.inner.version()
    }
    fn filename(&self) -> Option<String> {
        self.inner.filename()
    }
    fn metadata_get(&self, key: &str) -> Option<String> {
        self.inner.metadata_get(key)
    }
    fn file_metadata(&self) -> HashMap<String, String> {
        self.inner.file_metadata()
    }
    fn counter_names(&self) -> Vec<String> {
        self.inner.counter_names()
    }
    fn gauge_names(&self) -> Vec<String> {
        self.inner.gauge_names()
    }
    fn histogram_names(&self) -> Vec<String> {
        self.inner.histogram_names()
    }
    fn counter_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.note(name, Some(Kind::Counter), None);
        self.inner.counter_labels(name)
    }
    fn gauge_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.note(name, Some(Kind::Gauge), None);
        self.inner.gauge_labels(name)
    }
    fn histogram_labels(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        self.note(name, Some(Kind::Histogram), None);
        self.inner.histogram_labels(name)
    }
    fn label_values(&self, metric: &str, key: &str) -> HashSet<String> {
        self.note(metric, None, Some(key));
        self.inner.label_values(metric, key)
    }
    fn has_counter(&self, name: &str) -> bool {
        self.note_exactly(name, Kind::Counter);
        self.inner.has_counter(name)
    }
    fn has_gauge(&self, name: &str) -> bool {
        self.note_exactly(name, Kind::Gauge);
        self.inner.has_gauge(name)
    }
    fn has_histogram(&self, name: &str) -> bool {
        self.note_exactly(name, Kind::Histogram);
        self.inner.has_histogram(name)
    }
}

/// The embedded service templates, and its category (`inference-library`).
fn templates() -> (Vec<ServiceExtension>, TemplateRegistry) {
    let registry = TemplateRegistry::embedded().unwrap();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/dashboard/templates");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let exts = names
        .iter()
        .filter_map(|n| registry.get(n).cloned())
        .collect();
    (exts, registry)
}

/// The queries of every dashboard section, every template's section, and
/// the `inference-library` category over vllm and sglang, as the viewers
/// render them against `data`.
///
/// Each section is also generated by its Rust generator, and the two must
/// render the same JSON. The generator's reads are what tell [`Probe`] which
/// metrics a data gate asks about: the template's expander reads presence
/// from the metric name lists, which say nothing about which metric a gate
/// wanted, so the fixture grows from the generators' reads.
fn dashboard_queries(
    data: &dyn MetricsSource,
    exts: &[ServiceExtension],
    registry: &TemplateRegistry,
) -> Vec<Use> {
    let named: Vec<(&str, &ServiceExtension)> =
        exts.iter().map(|e| (e.service_name.as_str(), e)).collect();
    let category = registry.get_category("inference-library").unwrap();
    let members: Vec<(&str, &ServiceExtension)> = ["vllm", "sglang"]
        .iter()
        .map(|n| (*n, registry.get(n).unwrap()))
        .collect();
    let contexts = [
        (named.as_slice(), None),
        (members.as_slice(), Some(("inference-library", category))),
    ];
    let mut out = Vec::new();
    for (services, category) in contexts {
        let generated = build_dashboard_context(None, services, category, &[]);
        let ctx =
            build_template_context(embedded_dashboard_template(), None, services, category, &[]);
        assert_eq!(
            serde_json::to_string(&generated.sections).unwrap(),
            serde_json::to_string(&ctx.sections).unwrap()
        );
        for section in &ctx.sections {
            let want = generate_section(data, &section.route, &generated);
            let Some(rendered) = render_section(data, &section.route, &ctx) else {
                assert!(want.is_none(), "{} is not rendered", section.route);
                continue;
            };
            assert!(
                rendered.error.is_none(),
                "{}: {:?}",
                section.route,
                rendered.error
            );
            let json = serde_json::to_value(&rendered.view).unwrap();
            assert_eq!(
                want.map(|v| serde_json::to_value(&v).unwrap()).as_ref(),
                Some(&json),
                "{} renders differently from its generator",
                section.route
            );
            collect_plot_queries(&section.route, &json, &mut out);
        }
    }
    out
}

fn collect_plot_queries(origin: &str, v: &serde_json::Value, out: &mut Vec<Use>) {
    match v {
        serde_json::Value::Object(map) => {
            let opts = map.get("opts");
            let histogram_plot =
                opts.and_then(|o| o.get("type")).and_then(|t| t.as_str()) == Some("histogram");
            for key in [
                "promql_query",
                "promql_query_total",
                "promql_query_experiment",
            ] {
                if let Some(q) = map.get(key).and_then(|q| q.as_str()) {
                    let title = opts
                        .and_then(|o| o.get("title"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("?");
                    out.push(Use {
                        origin: format!("{origin} [{title}]"),
                        query: q.to_string(),
                        histogram_plot,
                    });
                }
            }
            for child in map.values() {
                collect_plot_queries(origin, child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_plot_queries(origin, child, out);
            }
        }
        _ => {}
    }
}

/// The queries the viewer sends for a plot: a histogram plot's base query in
/// its quantile and heatmap forms (`buildHistogramQuery`, metric_types.js).
fn sent_queries(u: &Use) -> Vec<String> {
    if u.histogram_plot {
        vec![
            format!("histogram_quantiles([0.5, 0.9, 0.99], {})", u.query),
            format!("histogram_heatmap({})", u.query),
        ]
    } else {
        vec![u.query.clone()]
    }
}

/// The function call that directly encloses byte `at` of `query`, if any.
fn enclosing_function(query: &str, at: usize) -> Option<&str> {
    let bytes = query.as_bytes();
    let mut depth = 0i32;
    let mut i = at;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' => depth += 1,
            b'(' if depth > 0 => depth -= 1,
            b'(' => {
                let end = i;
                let mut start = end;
                while start > 0
                    && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')
                {
                    start -= 1;
                }
                if start == end {
                    continue;
                }
                let name = &query[start..end];
                // `sum by (x) (...)`: the parenthesised label list belongs to
                // the aggregation, which does not decide the metric's kind.
                if matches!(name, "by" | "without") {
                    continue;
                }
                return Some(name);
            }
            _ => {}
        }
    }
    None
}

/// The kind each metric of `u` has, by the function reading it.
fn metric_kinds(u: &Use) -> Vec<(String, Kind)> {
    let names = metriken_query::referenced_metrics(&u.query)
        .unwrap_or_else(|e| panic!("{}: {}: {e}", u.origin, u.query));
    names
        .into_iter()
        .map(|name| {
            if u.histogram_plot {
                return (name, Kind::Histogram);
            }
            let mut kind = Kind::Gauge;
            for (at, _) in u.query.match_indices(name.as_str()) {
                let word = |c: Option<char>| {
                    c.is_some_and(|c| c.is_ascii_alphanumeric() || "_/".contains(c))
                };
                if word(u.query[..at].chars().next_back())
                    || word(u.query[at + name.len()..].chars().next())
                {
                    continue;
                }
                match enclosing_function(&u.query, at) {
                    Some(f) if f.starts_with("histogram_") => kind = Kind::Histogram,
                    Some("rate" | "irate" | "increase" | "resets") => kind = Kind::Counter,
                    _ => {}
                }
            }
            (name, kind)
        })
        .collect()
}

/// The label sets `query`'s selectors match by equality, per metric: a
/// matcher `k="v"` or a regex matcher whose first alternative is a plain
/// value. A selector's other matchers (`!=`, `!~`) are met by any value
/// that is not theirs, which these sets are.
fn selector_label_sets(query: &str) -> Vec<(String, BTreeMap<String, String>)> {
    let selector = regex::Regex::new(r#"([A-Za-z_:][A-Za-z0-9_:/]*)?\{([^}]*)\}"#).unwrap();
    let matcher = regex::Regex::new(r#"([A-Za-z_][A-Za-z0-9_]*)\s*(=~|=)\s*"([^"]*)""#).unwrap();
    let mut out = Vec::new();
    for s in selector.captures_iter(query) {
        let mut name = s.get(1).map(|m| m.as_str().to_string());
        let mut labels = BTreeMap::new();
        for m in matcher.captures_iter(&s[2]) {
            let (key, op, value) = (&m[1], &m[2], &m[3]);
            let value = if op == "=~" {
                match value.split('|').next() {
                    Some(v)
                        if v.chars()
                            .all(|c| c.is_ascii_alphanumeric() || "_-/.".contains(c)) =>
                    {
                        v
                    }
                    _ => continue,
                }
            } else {
                value
            };
            if key == "__name__" {
                name = Some(value.to_string());
            } else {
                labels.insert(key.to_string(), value.to_string());
            }
        }
        if let Some(name) = name {
            out.push((name, labels));
        }
    }
    out
}

/// Add every metric `uses` name to `fixture`, with its kind and the label
/// sets its selectors match. Kinds found twice for one name are returned.
fn learn(fixture: &mut Fixture, uses: &[Use]) -> BTreeSet<String> {
    let ignoring = regex::Regex::new(r#"ignoring\s*\(([^)]*)\)"#).unwrap();
    let by_id =
        regex::Regex::new(r#"\b(by|on|without)\s*\([^)]*\bid\b|\bid\s*(=|!=|=~|!~)"#).unwrap();
    let mut conflicts = BTreeSet::new();
    for u in uses {
        for (name, kind) in metric_kinds(u) {
            match fixture.kinds.get(&name) {
                Some(_) if fixture.guessed.remove(&name) => {
                    fixture.kinds.insert(name.clone(), kind);
                }
                Some(k) if *k != kind => {
                    conflicts.insert(format!("{name}: {k:?} and {kind:?} ({})", u.origin));
                }
                Some(_) => {}
                None => {
                    fixture.kinds.insert(name.clone(), kind);
                }
            }
            // A metric grouped, matched or selected on `id` gets two
            // series apart in `id`, so the plots that need more than one are
            // generated; any other gets one series with no labels, as a
            // machine-wide gauge such as `cpu_cores` has.
            let sets = fixture.series.entry(name).or_default();
            if by_id.is_match(&u.query) {
                for id in ["0", "1"] {
                    sets.insert([("id".to_string(), id.to_string())].into_iter().collect());
                }
            } else {
                sets.insert(BTreeMap::new());
            }
        }
        // A selector's series also carries the labels the query's other
        // selectors match on and it does not name, except those an
        // `ignoring(..)` sets aside: in `a{direction="x"} / ignoring(direction)
        // b{vendor="y"}`, `a` carries `vendor` in a recording and `b` has no
        // `direction`.
        let ignored: BTreeSet<String> = ignoring
            .captures_iter(&u.query)
            .flat_map(|c| {
                c[1].split(',')
                    .map(|l| l.trim().to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        let sets = selector_label_sets(&u.query);
        for (name, labels) in &sets {
            if !fixture.kinds.contains_key(name) || labels.is_empty() {
                continue;
            }
            let mut merged = labels.clone();
            for (_, other) in &sets {
                for (k, v) in other {
                    if !ignored.contains(k) {
                        merged.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                }
            }
            let series = fixture.series.entry(name.clone()).or_default();
            series.insert(labels.clone());
            series.insert(merged);
        }
    }
    conflicts
}

/// Add the metrics `probe`'s generators asked about and `fixture` lacks,
/// so the plots gated on them are generated next round: each as the first
/// kind it was asked as (a gauge when only its label values were asked),
/// with two series apart in each label key asked for, or one with no
/// labels.
fn learn_gates(fixture: &mut Fixture, probe: &Probe) {
    for (name, Asked { kinds, keys, .. }) in probe.asked.lock().unwrap().iter() {
        if fixture.kinds.contains_key(name) {
            let sets = fixture.series.entry(name.clone()).or_default();
            for key in keys {
                if !sets.iter().any(|l| l.contains_key(key)) {
                    for v in ["0", "1"] {
                        sets.insert([(key.clone(), v.to_string())].into_iter().collect());
                    }
                }
            }
            continue;
        }
        fixture
            .kinds
            .insert(name.clone(), kinds.first().copied().unwrap_or(Kind::Gauge));
        fixture.guessed.insert(name.clone());
        let sets = fixture.series.entry(name.clone()).or_default();
        if keys.is_empty() {
            sets.insert(BTreeMap::new());
        }
        for key in keys {
            for v in ["0", "1"] {
                sets.insert([(key.clone(), v.to_string())].into_iter().collect());
            }
        }
    }
}

/// A dendro archive holding `fixture`, `TICKS` one-second ticks: one
/// acquisition group per metric (`fixture/<metric>`), a member per label set,
/// as an agent streams them.
fn archive(fixture: &Fixture) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dashboards.dendro");
    let seed = Seed {
        // No recording labels: a template's selectors name their own
        // `source`, which the series carry.
        labels: BTreeMap::new(),
        metadata: [("sampling_interval_ms".to_string(), "1000".to_string())]
            .into_iter()
            .collect(),
        clock_anchor_wall_ns: ANCHOR,
    };
    let policy = SealPolicy {
        max_bytes: usize::MAX,
        max_rows: 10,
        max_age: Duration::from_secs(3600),
        ..SealPolicy::default()
    };
    let mut buf =
        HindsightBuffer::create_dendro(&path, seed, Duration::from_secs(3600), policy).unwrap();
    let schemas: Vec<(String, Kind, Arc<GroupSchema>)> = fixture
        .kinds
        .iter()
        .map(|(name, kind)| {
            let members: Vec<MetricDesc> = fixture.series[name]
                .iter()
                .enumerate()
                .map(|(slot, labels)| {
                    let mut metadata = labels.clone();
                    metadata.insert("metric".to_string(), name.clone());
                    if *kind == Kind::Histogram {
                        metadata.insert("grouping_power".to_string(), "7".to_string());
                        metadata.insert("max_value_power".to_string(), "64".to_string());
                    }
                    MetricDesc {
                        name: format!("{name}/{slot}"),
                        metadata,
                    }
                })
                .collect();
            let mut schema = GroupSchema::default();
            match kind {
                Kind::Counter => schema.counters = members,
                Kind::Gauge => schema.gauges = members,
                Kind::Histogram => schema.histograms = members,
            }
            (name.clone(), *kind, Arc::new(schema))
        })
        .collect();
    for i in 0..TICKS {
        let ts = ANCHOR + i * SECOND;
        let groups = schemas
            .iter()
            .map(|(name, kind, schema)| {
                let n = fixture.series[name].len();
                let mut group = GroupSnapshot {
                    name: format!("fixture/{name}"),
                    schema_hash: schema.hash(),
                    schema: Some(Arc::clone(schema)),
                    window: Some(Window::new(ts - SECOND / 2, ts)),
                    counters: Vec::new(),
                    gauges: Vec::new(),
                    histograms: Vec::new(),
                };
                match kind {
                    Kind::Counter => group.counters = vec![Some(1_000 * i); n],
                    Kind::Gauge => group.gauges = vec![Some(1 + i as i64); n],
                    Kind::Histogram => {
                        let mut h = histogram::Histogram::new(7, 64).unwrap();
                        for v in [100, 1_000 + i, 10_000] {
                            h.add(v, i + 1).unwrap();
                        }
                        group.histograms = vec![Some(h); n];
                    }
                }
                group
            })
            .collect();
        let snapshot = Snapshot::V3(SnapshotV3 {
            systemtime: std::time::SystemTime::UNIX_EPOCH + Duration::from_nanos(ts),
            duration: Duration::ZERO,
            metadata: HashMap::new(),
            groups,
        });
        buf.ingest(&snapshot, ts, 0).unwrap();
    }
    buf.sync().unwrap();
    drop(buf);
    (dir, path)
}

/// Every dashboard and template query answers on a dendro archive with at
/// least one series, and every dashboard section has plots.
#[test]
fn every_dashboard_and_template_query_answers_on_a_dendro_archive() {
    let (exts, registry) = templates();
    assert!(exts.len() >= 10, "{} templates", exts.len());

    // Generate against the archive the previous round described until the
    // queries and gates name nothing it lacks. Each round adds the metrics the queries name and
    // the metrics the generators' data gates asked about, so a plot gated
    // on a metric only it reads is generated the round after.
    let empty: Arc<dyn MetricsSource> = Arc::new(metriken_query::MemoryStore::builder().build());
    let mut fixture = Fixture::default();
    let mut probe = Probe {
        inner: empty,
        asked: Default::default(),
    };
    let mut uses = dashboard_queries(&probe, &exts, &registry);
    let mut opened = None;
    let mut converged = false;
    // Every round's asks: a gate can run only while another metric is
    // absent (a fallback name), so the last round need not reach it.
    let mut asked: BTreeMap<String, Asked> = BTreeMap::new();
    for _ in 0..8 {
        for (name, a) in probe.asked.lock().unwrap().iter() {
            asked.entry(name.clone()).or_default().merge(a);
        }
        let before = (fixture.kinds.clone(), fixture.series.clone());
        let conflicts = learn(&mut fixture, &uses);
        assert!(
            conflicts.is_empty(),
            "metrics read as two kinds: {conflicts:#?}"
        );
        learn_gates(&mut fixture, &probe);
        // The archive the queries were generated against already holds
        // everything they and the gates name.
        if opened.is_some() && before == (fixture.kinds.clone(), fixture.series.clone()) {
            converged = true;
            break;
        }
        let (dir, path) = archive(&fixture);
        probe = Probe {
            inner: crate::mcp::open_source(&path).unwrap(),
            asked: Default::default(),
        };
        uses = dashboard_queries(&probe, &exts, &registry);
        opened = Some(dir);
    }
    assert!(converged, "the fixture kept growing");
    let _dir = opened.unwrap();
    let source = &probe;

    let routes: BTreeSet<&str> = uses
        .iter()
        .filter_map(|u| u.origin.split(' ').next())
        .collect();
    let ctx = build_dashboard_context(None, &[], None, &[]);
    // A gate asks for a metric as a kind; a plot reads it as one. A gate
    // asking for a kind no plot reads hides its plots on every recording:
    // the kinds `has_counter` and the like asked for, and the kinds a
    // metric's labels were asked for, must include it. `has_metric`
    // asks for a metric's labels as each kind in turn until one answers, so
    // the kinds asked of a metric it checks include the metric's own, and a
    // wrong-kind `*_labels` gate on that metric is not seen; nor is a wrong
    // exact gate on a metric another gate asks for correctly.
    let wrong_kind: Vec<String> = asked
        .iter()
        .filter(|(name, _)| !fixture.guessed.contains(*name))
        .filter_map(|(name, asked)| {
            let kind = fixture.kinds.get(name)?;
            let exact_wrong = !asked.exactly.is_empty() && !asked.exactly.contains(kind);
            let labels_wrong = !asked.kinds.is_empty() && !asked.kinds.contains(kind);
            (exact_wrong || labels_wrong).then(|| {
                format!(
                    "{name}: read as {kind:?}, asked as {:?}, required to be {:?}",
                    asked.kinds, asked.exactly
                )
            })
        })
        .collect();
    assert!(
        wrong_kind.is_empty(),
        "gates asking for the wrong kind: {wrong_kind:#?}"
    );

    let missing: Vec<&str> = ctx
        .sections
        .iter()
        .map(|s| s.route.as_str())
        .filter(|r| *r != "/query" && !routes.contains(r))
        .collect();
    assert!(missing.is_empty(), "sections with no plots: {missing:?}");

    let start = (ANCHOR / SECOND) as f64 + 5.0;
    let end = (ANCHOR / SECOND) as f64 + (TICKS - 1) as f64;
    let mut failures = Vec::new();
    let mut empty_results = Vec::new();
    let mut seen = BTreeSet::new();
    for u in &uses {
        for query in sent_queries(u) {
            if !seen.insert(query.clone()) {
                continue;
            }
            match source.query_range(&query, start, end, 1.0) {
                Err(e) => failures.push(format!("{}: {query}\n    {e}", u.origin)),
                Ok(metriken_query::QueryResult::Matrix { result }) if result.is_empty() => {
                    empty_results.push(format!("{}: {query}", u.origin))
                }
                Ok(_) => {}
            }
        }
    }
    assert!(seen.len() > 850, "{} queries", seen.len());
    assert!(
        failures.is_empty(),
        "{} of {} queries failed:\n{}",
        failures.len(),
        seen.len(),
        failures.join("\n")
    );
    assert!(
        empty_results.is_empty(),
        "{} of {} queries returned no series:\n{}",
        empty_results.len(),
        seen.len(),
        empty_results.join("\n")
    );
}
