//! The section template against the Rust section generators.
//!
//! Every section the template defines must render the same JSON as the
//! generator for that route, on every fixture catalog. The catalogs are a
//! real recording's metric set (`bench`), every metric a generator names
//! with two series each (`sink`), the same with one series each and with
//! every metric a gauge, a recording from before the renames (`legacy`), a
//! host with three GPUs from two vendors, two vendors' GPUs sharing
//! `id="0"`, GPUs with no `vendor` label or a non-integer `id`, an Intel GPU
//! with every engine class and an unknown one, devices with ids `"01"` and
//! `"1"`, an empty recording, and `sink` with each named metric removed in
//! turn, so every presence predicate is seen both ways.
//!
//! Golden files under `tests/golden/` hold the generators' output for the
//! named catalogs, so the template stays checked once a generator is
//! removed. `REZOLUS_BLESS=1 cargo test -p dashboard section_template`
//! rewrites them from the generators.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use metriken_query::{MetricsSource, QueryError, QueryOptions, QueryResult};

use crate::dashboard::{DashboardContext, build_dashboard_context, generate_section};
use crate::section_template::SectionTemplate;

type Labels = BTreeMap<String, String>;

/// A metric catalog with no samples: what the generators and the template
/// read to decide what to show.
#[derive(Clone, Default)]
pub(crate) struct Catalog {
    counters: BTreeMap<String, Vec<Labels>>,
    gauges: BTreeMap<String, Vec<Labels>>,
    histograms: BTreeMap<String, Vec<Labels>>,
}

impl Catalog {
    /// Add a counter series per label set.
    pub(crate) fn counter(mut self, name: &str, series: &[&[(&str, &str)]]) -> Self {
        self.counters
            .insert(name.into(), series.iter().map(|s| labels(s)).collect());
        self
    }

    /// Add a gauge series per label set.
    pub(crate) fn gauge(mut self, name: &str, series: &[&[(&str, &str)]]) -> Self {
        self.gauges
            .insert(name.into(), series.iter().map(|s| labels(s)).collect());
        self
    }

    /// Add a histogram series per label set.
    pub(crate) fn histogram(mut self, name: &str, series: &[&[(&str, &str)]]) -> Self {
        self.histograms
            .insert(name.into(), series.iter().map(|s| labels(s)).collect());
        self
    }

    fn from_json(json: &str) -> Self {
        let mut doc: BTreeMap<String, BTreeMap<String, Vec<Labels>>> =
            serde_json::from_str(json).unwrap();
        Self {
            counters: doc.remove("counter").unwrap_or_default(),
            gauges: doc.remove("gauge").unwrap_or_default(),
            histograms: doc.remove("histogram").unwrap_or_default(),
        }
    }

    fn without(&self, name: &str) -> Self {
        let mut out = self.clone();
        out.counters.remove(name);
        out.gauges.remove(name);
        out.histograms.remove(name);
        out
    }

    /// Every metric reduced to its first series.
    fn single(&self) -> Self {
        let first = |m: &BTreeMap<String, Vec<Labels>>| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.iter().take(1).cloned().collect()))
                .collect()
        };
        Self {
            counters: first(&self.counters),
            gauges: first(&self.gauges),
            histograms: first(&self.histograms),
        }
    }

    /// Every metric as a gauge, so a predicate on the metric type is seen
    /// false while the metric is present.
    fn as_gauges(&self) -> Self {
        let mut gauges = self.gauges.clone();
        gauges.extend(self.counters.clone());
        gauges.extend(self.histograms.clone());
        Self {
            gauges,
            ..Self::default()
        }
    }

    fn rename(&mut self, from: &str, to: &str) {
        for m in [&mut self.counters, &mut self.gauges, &mut self.histograms] {
            if let Some(v) = m.remove(from) {
                m.insert(to.to_string(), v);
            }
        }
    }

    fn names(&self) -> BTreeSet<String> {
        self.counters
            .keys()
            .chain(self.gauges.keys())
            .chain(self.histograms.keys())
            .cloned()
            .collect()
    }
}

impl MetricsSource for Catalog {
    fn query_range_opts(
        &self,
        _: &str,
        _: f64,
        _: f64,
        _: f64,
        _: &QueryOptions,
    ) -> Result<QueryResult, QueryError> {
        Err(QueryError::Unsupported("catalog fixture".into()))
    }
    fn query(&self, _: &str, _: Option<f64>) -> Result<QueryResult, QueryError> {
        Err(QueryError::Unsupported("catalog fixture".into()))
    }
    fn columns(&self, _: &str) -> Result<HashSet<String>, QueryError> {
        Ok(HashSet::new())
    }
    fn time_range(&self) -> Option<(f64, f64)> {
        Some((1_700_000_000.0, 1_700_000_060.0))
    }
    fn time_range_ns(&self) -> Option<(u64, u64)> {
        Some((1_700_000_000_000_000_000, 1_700_000_060_000_000_000))
    }
    fn interval(&self) -> f64 {
        1.0
    }
    fn source(&self) -> String {
        "rezolus".into()
    }
    fn version(&self) -> String {
        "6.0.0".into()
    }
    fn filename(&self) -> Option<String> {
        None
    }
    fn metadata_get(&self, _: &str) -> Option<String> {
        None
    }
    fn file_metadata(&self) -> HashMap<String, String> {
        HashMap::new()
    }
    fn counter_names(&self) -> Vec<String> {
        self.counters.keys().cloned().collect()
    }
    fn gauge_names(&self) -> Vec<String> {
        self.gauges.keys().cloned().collect()
    }
    fn histogram_names(&self) -> Vec<String> {
        self.histograms.keys().cloned().collect()
    }
    fn counter_labels(&self, name: &str) -> Vec<Labels> {
        self.counters.get(name).cloned().unwrap_or_default()
    }
    fn gauge_labels(&self, name: &str) -> Vec<Labels> {
        self.gauges.get(name).cloned().unwrap_or_default()
    }
    fn histogram_labels(&self, name: &str) -> Vec<Labels> {
        self.histograms.get(name).cloned().unwrap_or_default()
    }
}

/// The generator sources, read for the metric names they mention.
const GENERATOR_SOURCES: &[&str] = &[
    include_str!("dashboard/blockio.rs"),
    include_str!("dashboard/cgroups.rs"),
    include_str!("dashboard/cpu.rs"),
    include_str!("dashboard/exceptions.rs"),
    include_str!("dashboard/ext4.rs"),
    include_str!("dashboard/filesystem.rs"),
    include_str!("dashboard/gpu.rs"),
    include_str!("dashboard/hw_sensors.rs"),
    include_str!("dashboard/memory.rs"),
    include_str!("dashboard/network.rs"),
    include_str!("dashboard/overview.rs"),
    include_str!("dashboard/rezolus.rs"),
    include_str!("dashboard/scheduler.rs"),
    include_str!("dashboard/softirq.rs"),
    include_str!("dashboard/syscall.rs"),
    include_str!("dashboard/xfs.rs"),
    include_str!("plot.rs"),
];

/// Every lowercase `a_b` token in a generator's string literals, plus the
/// names the CPU generator builds with `format!`. Over-inclusion is
/// harmless: a metric no generator reads changes nothing.
fn named_metrics() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for src in GENERATOR_SOURCES {
        for (i, literal) in src.split('"').enumerate() {
            if i % 2 == 0 {
                continue;
            }
            for token in
                literal.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
            {
                if token.contains('_')
                    && token.starts_with(|c: char| c.is_ascii_lowercase())
                    && !token.ends_with('_')
                {
                    out.insert(token.to_string());
                }
            }
        }
    }
    for level in [1, 2, 3, 6, 7, 8, 9, 10] {
        out.insert(format!("core_c{level}_residency"));
        out.insert(format!("package_c{level}_residency"));
    }
    out
}

fn labels(pairs: &[(&str, &str)]) -> Labels {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Every named metric, typed as in `bench` where it appears there and as a
/// counter otherwise, with two series that differ in every label a
/// generator splits on.
fn sink(bench: &Catalog) -> Catalog {
    let series = vec![
        labels(&[
            ("id", "0"),
            ("op", "read"),
            ("state", "user"),
            ("direction", "transmit"),
            ("mount", "/"),
            ("vendor", "nvidia"),
            ("engine_class", "render"),
            ("frequency", "actual"),
            ("sensor", "coretemp:native-0"),
            ("kind", "net_rx"),
        ]),
        labels(&[
            ("id", "1"),
            ("op", "write"),
            ("state", "system"),
            ("direction", "receive"),
            ("mount", "/data"),
            ("vendor", "nvidia"),
            ("engine_class", "video"),
            ("frequency", "requested"),
            ("sensor", "coretemp:native-1"),
            ("kind", "timer"),
        ]),
    ];
    let mut out = Catalog::default();
    for name in named_metrics() {
        let target = if bench.gauges.contains_key(&name) {
            &mut out.gauges
        } else if bench.histograms.contains_key(&name) {
            &mut out.histograms
        } else {
            &mut out.counters
        };
        target.insert(name, series.clone());
    }
    out
}

/// The `bench` fixture with every series that has an `id` label repeated
/// for ids 0 to `n - 1`: a host with `n` CPUs (and as many of every other
/// device with an `id`).
pub(crate) fn bench_with_ids(n: usize) -> Catalog {
    with_ids(
        &Catalog::from_json(include_str!("../tests/fixtures/catalogs/bench.json")),
        n,
    )
}

/// `base` with every series that has an `id` label repeated for ids 0 to
/// `n - 1`.
pub(crate) fn with_ids(bench: &Catalog, n: usize) -> Catalog {
    let widen = |metrics: &BTreeMap<String, Vec<Labels>>| -> BTreeMap<String, Vec<Labels>> {
        metrics
            .iter()
            .map(|(name, series)| {
                let mut out = Vec::new();
                let mut seen = BTreeSet::new();
                for labels in series {
                    let mut base = labels.clone();
                    if base.remove("id").is_none() {
                        out.push(labels.clone());
                        continue;
                    }
                    if seen.insert(base.clone()) {
                        for i in 0..n {
                            let mut one = base.clone();
                            one.insert("id".to_string(), i.to_string());
                            out.push(one);
                        }
                    }
                }
                (name.clone(), out)
            })
            .collect()
    };
    Catalog {
        counters: widen(&bench.counters),
        gauges: widen(&bench.gauges),
        histograms: widen(&bench.histograms),
    }
}

/// `sink` built from the `bench` fixture: the largest catalog.
pub(crate) fn sink_catalog() -> Catalog {
    sink(&Catalog::from_json(include_str!(
        "../tests/fixtures/catalogs/bench.json"
    )))
}

fn legacy(bench: &Catalog) -> Catalog {
    let mut out = bench
        .without("network_host_bytes")
        .without("network_host_packets")
        .without("blockio_queue_latency")
        .without("blockio_total_latency");
    out.rename("blockio_device_latency", "blockio_latency");
    out
}

/// Two NVIDIA GPUs and an Intel iGPU that shares id 0 with one of them.
fn gpus() -> Catalog {
    let mut out = Catalog::default();
    let series = vec![
        labels(&[("id", "0"), ("vendor", "nvidia")]),
        labels(&[("id", "1"), ("vendor", "nvidia")]),
    ];
    for name in [
        "gpu_utilization",
        "gpu_memory",
        "gpu_temperature",
        "gpu_power_usage",
    ] {
        out.gauges.insert(name.into(), series.clone());
    }
    out.counters.insert(
        "gpu_engine_busy_time".into(),
        vec![
            labels(&[("id", "0"), ("vendor", "intel"), ("engine_class", "render")]),
            labels(&[("id", "0"), ("vendor", "intel"), ("engine_class", "video")]),
        ],
    );
    out.counters.insert(
        "gpu_frequency_sample".into(),
        vec![labels(&[
            ("id", "0"),
            ("vendor", "intel"),
            ("frequency", "actual"),
        ])],
    );
    out
}

/// An NVIDIA GPU and an Intel iGPU, both `id="0"`: two GPUs only when
/// counted by `(vendor, id)`.
fn gpus_sharing_an_id() -> Catalog {
    let mut out = Catalog::default();
    out.gauges.insert(
        "gpu_utilization".into(),
        vec![labels(&[("id", "0"), ("vendor", "nvidia")])],
    );
    out.counters.insert(
        "gpu_engine_busy_time".into(),
        vec![labels(&[
            ("id", "0"),
            ("vendor", "intel"),
            ("engine_class", "render"),
        ])],
    );
    out
}

/// Two GPUs from a sampler that sets no `vendor` label (as the Apple GPU
/// sampler does), so the selector lists entries without a vendor.
fn gpus_without_vendor() -> Catalog {
    let mut out = Catalog::default();
    out.gauges.insert(
        "gpu_utilization".into(),
        vec![labels(&[("id", "0")]), labels(&[("id", "1")])],
    );
    out
}

/// One GPU, plus a series whose `id` is not an integer and one with no
/// `id`. Counting either as a GPU would make this a multi-GPU host.
fn gpus_with_unparsed_id() -> Catalog {
    let mut out = Catalog::default();
    out.gauges.insert(
        "gpu_utilization".into(),
        vec![
            labels(&[("id", "0"), ("vendor", "nvidia")]),
            labels(&[("id", "gpu"), ("vendor", "nvidia")]),
        ],
    );
    out.gauges
        .insert("gpu_temperature".into(), vec![labels(&[("vendor", "amd")])]);
    out
}

/// An Intel GPU with an engine of every class the sampler names, plus one it
/// does not (`blitter`), so every class's subgroup is seen present and an
/// unknown class is seen ignored.
fn intel_all_engines() -> Catalog {
    let series: Vec<Labels> = [
        "compute",
        "render",
        "copy",
        "video",
        "video-enhance",
        "other",
        "blitter",
    ]
    .iter()
    .map(|class| labels(&[("id", "0"), ("vendor", "intel"), ("engine_class", class)]))
    .collect();
    let mut out = Catalog::default();
    out.counters.insert("gpu_engine_busy_time".into(), series);
    out.counters.insert(
        "gpu_frequency_sample".into(),
        vec![
            labels(&[("id", "0"), ("vendor", "intel"), ("frequency", "actual")]),
            labels(&[("id", "0"), ("vendor", "intel"), ("frequency", "requested")]),
        ],
    );
    out
}

/// GPUs and CPUs with ids `"01"` and `"1"`: one device where ids are
/// compared as integers, two where they are compared as strings.
fn non_canonical_ids() -> Catalog {
    let series = vec![
        labels(&[("id", "01"), ("vendor", "nvidia"), ("state", "user")]),
        labels(&[("id", "1"), ("vendor", "nvidia"), ("state", "user")]),
    ];
    let mut out = Catalog::default();
    for name in ["gpu_utilization", "gpu_memory", "gpu_power_usage"] {
        out.gauges.insert(name.into(), series.clone());
    }
    for name in [
        "cpu_usage",
        "cpu_cycles",
        "cpu_instructions",
        "scheduler_runqueue_wait",
    ] {
        out.counters.insert(name.into(), series.clone());
    }
    out
}

fn named_catalogs() -> Vec<(&'static str, Catalog)> {
    let bench = Catalog::from_json(include_str!("../tests/fixtures/catalogs/bench.json"));
    let sink = sink(&bench);
    vec![
        ("bench", bench.clone()),
        ("bench-single", bench.single()),
        ("legacy", legacy(&bench)),
        ("sink", sink.clone()),
        ("sink-single", sink.single()),
        ("sink-gauges", sink.as_gauges()),
        ("gpus", gpus()),
        ("gpus-sharing-an-id", gpus_sharing_an_id()),
        ("gpus-without-vendor", gpus_without_vendor()),
        ("gpus-with-unparsed-id", gpus_with_unparsed_id()),
        ("intel-all-engines", intel_all_engines()),
        ("non-canonical-ids", non_canonical_ids()),
        ("empty", Catalog::default()),
    ]
}

fn template() -> SectionTemplate {
    SectionTemplate::parse(&crate::rezolus_template::json()).unwrap()
}

fn render_generator(data: &Catalog, route: &str, ctx: &DashboardContext) -> String {
    let view = generate_section(data, route, ctx).unwrap();
    serde_json::to_string_pretty(&view).unwrap()
}

fn render_template(
    t: &SectionTemplate,
    data: &Catalog,
    route: &str,
    ctx: &DashboardContext,
) -> String {
    let mut params = BTreeMap::new();
    if let Some(tq) = &ctx.throughput_query {
        params.insert("throughput".to_string(), tq.clone());
    }
    let view = t
        .expand(data, route, ctx.sections.clone(), &params)
        .unwrap_or_else(|e| panic!("{route}: {e}"))
        .unwrap_or_else(|| panic!("{route}: not in the template"));
    serde_json::to_string_pretty(&view).unwrap()
}

/// The contexts each section is rendered in: the default, and for the
/// overview one with a throughput KPI.
fn contexts() -> Vec<(&'static str, DashboardContext)> {
    let plain = build_dashboard_context(None, &[], None, &[]);
    let mut throughput = plain.clone();
    throughput.throughput_query = Some("sum(irate(requests[5m]))".to_string());
    vec![("", plain), ("throughput", throughput)]
}

fn routes(t: &SectionTemplate) -> Vec<String> {
    t.sections().into_iter().map(|s| s.route).collect()
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn golden_name(catalog: &str, route: &str, ctx: &str) -> String {
    let route = route.trim_start_matches('/');
    if ctx.is_empty() {
        format!("{catalog}/{route}.json")
    } else {
        format!("{catalog}/{route}.{ctx}.json")
    }
}

#[test]
fn section_template_matches_the_generators() {
    let t = template();
    let mut failures = Vec::new();
    for (ctx_name, ctx) in contexts() {
        for route in routes(&t) {
            if !ctx_name.is_empty() && route != "/overview" {
                continue;
            }
            for (name, data) in named_catalogs() {
                let want = render_generator(&data, &route, &ctx);
                let got = render_template(&t, &data, &route, &ctx);
                if want != got {
                    failures.push(format!("{route} on {name} {ctx_name}"));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "template differs from the generator: {failures:#?}"
    );
}

/// Each named metric removed from `sink` in turn, so every predicate on a
/// metric's presence is seen false while the rest hold.
#[test]
fn section_template_matches_the_generators_with_each_metric_absent() {
    let t = template();
    let bench = Catalog::from_json(include_str!("../tests/fixtures/catalogs/bench.json"));
    let sink = sink(&bench);
    let ctx = build_dashboard_context(None, &[], None, &[]);
    let mut failures = BTreeMap::<String, Vec<String>>::new();
    for metric in sink.names() {
        let data = sink.without(&metric);
        for route in routes(&t) {
            if render_generator(&data, &route, &ctx) != render_template(&t, &data, &route, &ctx) {
                failures.entry(route).or_default().push(metric.clone());
            }
        }
    }
    assert!(
        failures.is_empty(),
        "template differs from the generator without: {failures:#?}"
    );
}

#[test]
fn section_template_matches_the_golden_files() {
    let t = template();
    let bless = std::env::var_os("REZOLUS_BLESS").is_some();
    let mut failures = Vec::new();
    for (ctx_name, ctx) in contexts() {
        for route in routes(&t) {
            if !ctx_name.is_empty() && route != "/overview" {
                continue;
            }
            for (name, data) in named_catalogs() {
                let path = golden_dir().join(golden_name(name, &route, ctx_name));
                if bless {
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, render_generator(&data, &route, &ctx) + "\n").unwrap();
                }
                let want = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                if want.trim_end() != render_template(&t, &data, &route, &ctx) {
                    failures.push(path.display().to_string());
                }
            }
        }
    }
    assert!(failures.is_empty(), "template differs from: {failures:#?}");
}

/// The template lists every section the generators do, in the same order,
/// so the navigation built from it is the navigation built from them.
#[test]
fn section_template_lists_the_generators_sections() {
    let from_template: Vec<(String, String)> = template()
        .sections()
        .into_iter()
        .map(|s| (s.name, s.route))
        .collect();
    let from_generators: Vec<(String, String)> = build_dashboard_context(None, &[], None, &[])
        .sections
        .into_iter()
        .map(|s| (s.name, s.route))
        .collect();
    assert_eq!(from_template, from_generators);
}

/// What the viewers render: the context `build_template_context` makes from
/// the embedded template, rendered with `render_section`, has the navigation
/// and every section's JSON of the generators' context, with and without a
/// service section and a throughput KPI.
#[test]
fn a_template_context_renders_what_the_generators_render() {
    use crate::dashboard::{SourceEntry, build_template_context, render_section};
    use crate::source_templates::embedded_dashboard_template;

    let ext: crate::ServiceExtension = serde_json::from_value(serde_json::json!({
        "service_name": "svc",
        "kpis": [{"role": "throughput", "title": "Requests", "query": "sum(irate(requests[5m]))", "type": "delta_counter", "denominator": true}],
    }))
    .unwrap();
    let services: &[&[(&str, &crate::ServiceExtension)]] = &[&[], &[("svc", &ext)]];
    let sources = [
        vec![],
        vec![SourceEntry {
            name: "rezolus".into(),
            is_rezolus: true,
        }],
        vec![SourceEntry {
            name: "app".into(),
            is_rezolus: false,
        }],
    ];
    let mut failures = Vec::new();
    for svc in services {
        for src in &sources {
            let want_ctx = build_dashboard_context(Some(7), svc, None, src);
            assert_eq!(want_ctx.throughput_query.is_some(), !svc.is_empty());
            let got_ctx =
                build_template_context(embedded_dashboard_template(), Some(7), svc, None, src);
            assert_eq!(
                serde_json::to_string(&want_ctx.sections).unwrap(),
                serde_json::to_string(&got_ctx.sections).unwrap(),
                "navigation, {} services, {} sources",
                svc.len(),
                src.len()
            );
            for (name, data) in named_catalogs() {
                for section in &want_ctx.sections {
                    let want = generate_section(&data, &section.route, &want_ctx)
                        .map(|v| serde_json::to_string(&v).unwrap());
                    let got = render_section(&data, &section.route, &got_ctx).map(|r| {
                        assert!(r.error.is_none(), "{}: {:?}", section.route, r.error);
                        serde_json::to_string(&r.view).unwrap()
                    });
                    if want != got {
                        failures.push(format!("{} on {name}", section.route));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "differs: {failures:#?}");
}

/// A template section that cannot be expanded renders as an empty view
/// carrying the reason, and the other sections render as usual. A route the
/// template does not have is a 404 even when a generator has it, and a
/// template section under `/service/` or `/source/` is neither listed nor
/// rendered.
#[test]
fn a_template_context_renders_the_templates_routes_only() {
    use crate::dashboard::{TEMPLATE_ERROR_KEY, build_template_context, render_section};
    use crate::section_template::TemplateErrorKind;

    let plot = |query: &str| {
        serde_json::json!({"name": "g", "id": "g", "subgroups": [{"plots": [
            {"kind": "gauge", "title": "t", "id": "p", "unit": "count", "query": query}
        ]}]})
    };
    let doc = serde_json::json!({
        "schema": 1,
        "params": ["throughput"],
        "sections": [
            {"name": "Fine", "route": "/fine", "groups": [plot("up")]},
            {"name": "Needs throughput", "route": "/needs", "groups": [plot("${throughput}")]},
            {"name": "Shadow", "route": "/service/svc", "groups": [plot("up")]},
            {"name": "Shadow source", "route": "/source/app", "groups": [plot("up")]},
            {"name": "About", "route": "/about", "groups": [plot("up")]},
            {"name": "Report", "route": "/report", "groups": [plot("up")]},
            {"name": "Deep", "route": "/a/b", "groups": [plot("up")]},
        ],
    });
    let template = std::sync::Arc::new(SectionTemplate::parse(&doc.to_string()).unwrap());
    let ctx = build_template_context(template, None, &[], None, &[]);
    let routes: Vec<&str> = ctx.sections.iter().map(|s| s.route.as_str()).collect();
    assert_eq!(routes, ["/fine", "/needs"]);

    let data = Catalog::default();
    let fine = render_section(&data, "/fine", &ctx).unwrap();
    assert!(fine.error.is_none());
    assert!(!fine.view.metadata.contains_key(TEMPLATE_ERROR_KEY));

    let needs = render_section(&data, "/needs", &ctx).unwrap();
    let error = needs.error.expect("an unset parameter");
    assert_eq!(error.kind(), TemplateErrorKind::UnsetParam);
    let json = serde_json::to_value(&needs.view).unwrap();
    assert_eq!(json["groups"], serde_json::json!([]));
    assert_eq!(json["metadata"][TEMPLATE_ERROR_KEY], error.to_string());
    assert_eq!(json["sections"].as_array().unwrap().len(), 2);

    // With the parameter set, it renders.
    let mut with = ctx.clone();
    with.throughput_query = Some("sum(irate(requests[5m]))".into());
    assert!(
        render_section(&data, "/needs", &with)
            .unwrap()
            .error
            .is_none()
    );

    assert!(render_section(&data, "/cpu", &ctx).is_none());
    assert!(render_section(&data, "/service/svc", &ctx).is_none());
    assert!(render_section(&data, "/source/app", &ctx).is_none());
    assert!(render_section(&data, "/about", &ctx).is_none());
    assert!(render_section(&data, "/report", &ctx).is_none());
    assert!(render_section(&data, "/a/b", &ctx).is_none());
}

#[test]
fn reserved_routes_are_the_frontends_and_deeper_ones() {
    use crate::dashboard::{CLIENT_ROUTES, is_reserved_route};
    for route in CLIENT_ROUTES {
        assert!(is_reserved_route(route), "{route}");
    }
    for route in [
        "/",
        "",
        "cpu",
        "/service/x",
        "/source/x",
        "/cpu/chart/p",
        "/a/b",
    ] {
        assert!(is_reserved_route(route), "{route}");
    }
    // Every section of the Rezolus template is shown.
    for section in template().sections() {
        assert!(!is_reserved_route(&section.route), "{}", section.route);
    }
    assert!(!is_reserved_route("/aboutx"));
}
