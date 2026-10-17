//! The section template against the Rust section generators.
//!
//! Every section the template defines must render the same JSON as the
//! generator for that route, on every fixture catalog. The catalogs are a
//! real recording's metric set (`bench`), every metric a generator names
//! with two series each (`sink`), the same with one series each and with
//! every metric a gauge, a
//! recording from before the renames (`legacy`), a host with three GPUs
//! from two vendors, two vendors' GPUs sharing `id="0"`, GPUs with no
//! `vendor` label or a non-integer `id`, an empty recording, and `sink`
//! with each named metric removed in turn, so every presence predicate is
//! seen both ways.
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
struct Catalog {
    counters: BTreeMap<String, Vec<Labels>>,
    gauges: BTreeMap<String, Vec<Labels>>,
    histograms: BTreeMap<String, Vec<Labels>>,
}

impl Catalog {
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
