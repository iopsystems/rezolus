//! The V3 snapshot contract as golden expectations, and a cost benchmark.
//!
//! Metrics here are registered dynamically, for the length of one test, so
//! they do not appear in any other test's snapshot outside it.

use super::*;
use crate::agent::external_metrics::{ExternalMetric, ExternalMetricValue};
use linkme::distributed_slice;
use metriken::{
    AtomicHistogram, Counter, CounterGroup, DynBoxedMetric, Gauge, GaugeGroup, MetricBuilder,
};
use metriken_exposition::{GroupSchema, GroupSnapshot, SnapshotV3};

fn md(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn in_group(name: &str, group: &str) -> MetricBuilder {
    MetricBuilder::new(name.to_string()).metadata("acq_group", group)
}

// ---------------------------------------------------------------------------
// The V3 contract as golden expectations.

static CONTRACT_DECLARED: AcquisitionGroup =
    AcquisitionGroup::new("unattributed", "v3_contract_declared");
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CONTRACT_DECLARED_ENTRY: &'static AcquisitionGroup = &CONTRACT_DECLARED;

static CONTRACT_BOUNDED: AcquisitionGroup =
    AcquisitionGroup::new("unattributed", "v3_contract_bounded");
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CONTRACT_BOUNDED_ENTRY: &'static AcquisitionGroup = &CONTRACT_BOUNDED;

static CONTRACT_SET: AcquisitionGroup = AcquisitionGroup::new("unattributed", "v3_contract_set");
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CONTRACT_SET_ENTRY: &'static AcquisitionGroup = &CONTRACT_SET;

static CONTRACT_READER: AcquisitionGroup =
    AcquisitionGroup::new_reader_stamped("unattributed", "v3_contract_reader");
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CONTRACT_READER_ENTRY: &'static AcquisitionGroup = &CONTRACT_READER;

static CONTRACT_UNSTAMPED: AcquisitionGroup =
    AcquisitionGroup::new("unattributed", "v3_contract_unstamped");
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CONTRACT_UNSTAMPED_ENTRY: &'static AcquisitionGroup = &CONTRACT_UNSTAMPED;

/// One expected member: the metric it belongs to, its slot (for a group
/// metric), the metadata it carries beyond `metric` and `sampler`, and its
/// value.
struct Want {
    metric: &'static str,
    idx: Option<usize>,
    md: Vec<(String, String)>,
    value: WantValue,
}

enum WantValue {
    C(Option<u64>),
    G(Option<i64>),
    H(Option<histogram::Histogram>),
}

fn want(metric: &'static str, idx: Option<usize>, md: &[(&str, &str)], value: WantValue) -> Want {
    let mut md: Vec<(String, String)> = md
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    if let Some(idx) = idx {
        if !md.iter().any(|(k, _)| k == "id") {
            md.push(("id".to_string(), idx.to_string()));
        }
    }
    Want {
        metric,
        idx,
        md,
        value,
    }
}

type Values = (
    Vec<Option<u64>>,
    Vec<Option<i64>>,
    Vec<Option<histogram::Histogram>>,
);

/// The schema and values a group of these members must have: members named
/// `{position}` or `{position}x{idx}` by registry position, ordered by kind
/// (counters, gauges, histograms), then position, then slot; each member's
/// metadata is `metric`, `sampler` and its own.
fn expected(members: Vec<Want>) -> (GroupSchema, Values) {
    let positions: HashMap<String, usize> = metriken::metrics()
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name().to_string(), i))
        .collect();
    let mut members: Vec<(usize, Want)> = members
        .into_iter()
        .map(|w| (positions[w.metric], w))
        .collect();
    members.sort_by_key(|(pos, w)| (*pos, w.idx));

    let mut schema = GroupSchema {
        counters: Vec::new(),
        gauges: Vec::new(),
        histograms: Vec::new(),
    };
    let mut values: Values = (Vec::new(), Vec::new(), Vec::new());
    for (pos, w) in members {
        let name = match w.idx {
            Some(idx) => format!("{pos}x{idx}"),
            None => format!("{pos}"),
        };
        let mut metadata: BTreeMap<String, String> = [
            ("metric".to_string(), w.metric.to_string()),
            ("sampler".to_string(), "unattributed".to_string()),
        ]
        .into();
        metadata.extend(w.md);
        let desc = MetricDesc { name, metadata };
        match w.value {
            WantValue::C(v) => {
                schema.counters.push(desc);
                values.0.push(v);
            }
            WantValue::G(v) => {
                schema.gauges.push(desc);
                values.1.push(v);
            }
            WantValue::H(v) => {
                schema.histograms.push(desc);
                values.2.push(v);
            }
        }
    }
    (schema, values)
}

fn assert_group(tick: &str, g: &GroupSnapshot, members: Vec<Want>) {
    let (schema, values) = expected(members);
    assert_eq!(
        g.schema.as_deref(),
        Some(&schema),
        "{tick} {}: schema",
        g.name
    );
    assert_eq!(g.schema_hash, schema.hash(), "{tick} {}: hash", g.name);
    assert_eq!(g.counters, values.0, "{tick} {}: counters", g.name);
    assert_eq!(g.gauges, values.1, "{tick} {}: gauges", g.name);
    assert_eq!(g.histograms, values.2, "{tick} {}: histograms", g.name);
    assert_eq!(g.validate(), Ok(()), "{tick} {}", g.name);
}

/// This test's members of a group other tests also write into, with their
/// values, in the group's order.
fn my_members(g: &GroupSnapshot, prefix: &str) -> (Vec<MetricDesc>, Values) {
    let schema = g.schema.as_ref().expect("schema");
    let mine = |d: &MetricDesc| {
        d.metadata
            .get("metric")
            .is_some_and(|m| m.starts_with(prefix))
    };
    let mut descs = Vec::new();
    let mut values: Values = (Vec::new(), Vec::new(), Vec::new());
    for (d, v) in schema.counters.iter().zip(&g.counters) {
        if mine(d) {
            descs.push(d.clone());
            values.0.push(*v);
        }
    }
    for (d, v) in schema.gauges.iter().zip(&g.gauges) {
        if mine(d) {
            descs.push(d.clone());
            values.1.push(*v);
        }
    }
    for (d, v) in schema.histograms.iter().zip(&g.histograms) {
        if mine(d) {
            descs.push(d.clone());
            values.2.push(v.clone());
        }
    }
    (descs, values)
}

fn contract_external(generation: u64) -> Vec<ExternalMetric> {
    let metric = |name: &str, labels: &[(&str, &str)], value| ExternalMetric {
        name: name.to_string(),
        labels: md(labels),
        value,
        last_updated: std::time::Instant::now(),
        window: Some(Window::new(1, 2)),
    };
    let mut v = vec![
        metric(
            "ext_requests",
            &[("path", "/b")],
            ExternalMetricValue::Counter(10 + generation),
        ),
        metric(
            "ext_requests",
            &[("path", "/a")],
            ExternalMetricValue::Counter(20 + generation),
        ),
        metric(
            "ext_depth",
            &[],
            ExternalMetricValue::Gauge(-(generation as i64)),
        ),
        metric(
            "ext_latency",
            &[("op", "get")],
            ExternalMetricValue::Histogram {
                grouping_power: 2,
                max_value_power: 4,
                buckets: vec![generation; 12],
            },
        ),
        // 3 buckets is not a 2/4 histogram: left out.
        metric(
            "ext_broken",
            &[],
            ExternalMetricValue::Histogram {
                grouping_power: 2,
                max_value_power: 4,
                buckets: vec![1; 3],
            },
        ),
    ];
    if generation >= 6 {
        v.push(metric(
            "ext_new",
            &[("k", "v")],
            ExternalMetricValue::Counter(1),
        ));
    }
    v
}

/// `external/main` as the external metrics of `generation` must produce it.
fn assert_external(tick: &str, g: &GroupSnapshot, generation: u64) {
    let named = |name: &str, labels: &[(&str, &str)]| {
        let hash = MetricKey::new(name, &md(labels)).labels_hash;
        format!("external/{name}#{hash:016x}")
    };
    let meta = |name: &str, labels: &[(&str, &str)], extra: &[(&str, &str)]| {
        let mut m: BTreeMap<String, String> = [
            ("metric".to_string(), name.to_string()),
            ("source".to_string(), "external".to_string()),
        ]
        .into();
        for (k, v) in labels.iter().chain(extra) {
            m.insert(k.to_string(), v.to_string());
        }
        m
    };
    let mut counters = vec![
        (
            MetricDesc {
                name: named("ext_requests", &[("path", "/b")]),
                metadata: meta("ext_requests", &[("path", "/b")], &[]),
            },
            Some(10 + generation),
        ),
        (
            MetricDesc {
                name: named("ext_requests", &[("path", "/a")]),
                metadata: meta("ext_requests", &[("path", "/a")], &[]),
            },
            Some(20 + generation),
        ),
    ];
    if generation >= 6 {
        counters.push((
            MetricDesc {
                name: named("ext_new", &[("k", "v")]),
                metadata: meta("ext_new", &[("k", "v")], &[]),
            },
            Some(1),
        ));
    }
    counters.sort_by(|a, b| a.0.name.cmp(&b.0.name));
    let schema = GroupSchema {
        counters: counters.iter().map(|(d, _)| d.clone()).collect(),
        gauges: vec![MetricDesc {
            name: named("ext_depth", &[]),
            metadata: meta("ext_depth", &[], &[]),
        }],
        histograms: vec![MetricDesc {
            name: named("ext_latency", &[("op", "get")]),
            metadata: meta(
                "ext_latency",
                &[("op", "get")],
                &[("grouping_power", "2"), ("max_value_power", "4")],
            ),
        }],
    };
    assert_eq!(
        g.schema.as_deref(),
        Some(&schema),
        "{tick} external: schema"
    );
    assert_eq!(g.schema_hash, schema.hash(), "{tick} external: hash");
    assert_eq!(
        g.counters,
        counters.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        "{tick} external: counters"
    );
    assert_eq!(g.gauges, vec![Some(-(generation as i64))]);
    assert_eq!(
        g.histograms,
        vec![Some(
            histogram::Histogram::from_buckets(2, 4, vec![generation; 12]).unwrap()
        )]
    );
    assert_eq!(
        g.window, None,
        "{tick} external: a pushed metric's window is dropped"
    );
}

/// What `create_v3` produces from a known registry state, over nine ticks:
/// every membership rule (declared, bounded, member set, reader-stamped,
/// value-derived), each window source, `external/main`, the `log_` filter,
/// the `sampler` label, and when a schema is rebuilt (a slot assigned or
/// released, metadata replaced in place, a value-derived member appearing or
/// leaving) and when it is reused.
///
/// These expectations were run against the agent's own builder before
/// metriken's `GroupBuilder` replaced it, and it met every one of them.
fn run_v3_contract(build: &mut dyn FnMut(Vec<ExternalMetric>) -> Snapshot) {
    use WantValue::{C, G, H};

    let d_counter: DynBoxedMetric<Counter> = in_group("v3c_d_counter", "v3_contract_declared")
        .metadata("sampler", "bogus")
        .build(Counter::new());
    let d_gauge: DynBoxedMetric<Gauge> = in_group("v3c_d_gauge", "v3_contract_declared")
        .metadata("unit", "bytes")
        .build(Gauge::new());
    let d_cgroup: DynBoxedMetric<CounterGroup> =
        in_group("v3c_d_cgroup", "v3_contract_declared").build(CounterGroup::new(6));
    let d_ggroup: DynBoxedMetric<GaugeGroup> =
        in_group("v3c_d_ggroup", "v3_contract_declared").build(GaugeGroup::new(4));
    let d_hist: DynBoxedMetric<AtomicHistogram> =
        in_group("v3c_d_hist", "v3_contract_declared").build(AtomicHistogram::new(4, 10));
    CONTRACT_BOUNDED.set_member_bound(3);
    let b_cgroup: DynBoxedMetric<CounterGroup> =
        in_group("v3c_b_cgroup", "v3_contract_bounded").build(CounterGroup::new(8));
    CONTRACT_SET.set_member_set(&[9, 1, 5, 5]);
    let s_ggroup: DynBoxedMetric<GaugeGroup> =
        in_group("v3c_s_ggroup", "v3_contract_set").build(GaugeGroup::new(8));
    let r_cgroup: DynBoxedMetric<CounterGroup> =
        in_group("v3c_r_cgroup", "v3_contract_reader").build(CounterGroup::new(64));
    let r_ggroup: DynBoxedMetric<GaugeGroup> =
        in_group("v3c_r_ggroup", "v3_contract_reader").build(GaugeGroup::new(64));
    let u_counter: DynBoxedMetric<Counter> =
        in_group("v3c_u_counter", "v3_contract_unstamped").build(Counter::new());
    let x_cgroup: DynBoxedMetric<CounterGroup> =
        MetricBuilder::new("v3c_x_cgroup").build(CounterGroup::new(4));
    let x_ggroup: DynBoxedMetric<GaugeGroup> =
        MetricBuilder::new("v3c_x_ggroup").build(GaugeGroup::new(4));
    let x_hist: DynBoxedMetric<AtomicHistogram> =
        MetricBuilder::new("v3c_x_hist").build(AtomicHistogram::new(4, 10));
    let x_counter: DynBoxedMetric<Counter> =
        MetricBuilder::new("v3c_x_counter").build(Counter::new());
    let log_counter: DynBoxedMetric<Counter> =
        MetricBuilder::new("log_v3c_counter").build(Counter::new());

    d_counter.add(7);
    d_gauge.set(-3);
    d_cgroup.set_metadata(0, md(&[("cpu", "0")]));
    d_cgroup.set_metadata(2, md(&[("cpu", "2"), ("id", "override")]));
    d_cgroup.add(0, 5);
    d_ggroup.set(1, 11);
    b_cgroup.add(0, 1);
    b_cgroup.add(5, 1);
    s_ggroup.set(5, 55);
    for idx in [3usize, 17] {
        r_cgroup.set_metadata(idx, md(&[("name", &format!("task{idx}"))]));
        r_ggroup.set_metadata(idx, md(&[("name", &format!("task{idx}"))]));
        r_cgroup.add(idx, idx as u64);
        r_ggroup.set(idx, idx as i64);
    }
    u_counter.add(1);
    x_cgroup.add(0, 9);
    x_ggroup.set(2, 4);
    x_counter.add(2);
    log_counter.add(1);
    CONTRACT_DECLARED.acquire().finish();

    let find = |s: &SnapshotV3, name: &str| -> GroupSnapshot {
        s.groups
            .iter()
            .find(|g| g.name == name)
            .unwrap_or_else(|| panic!("group `{name}` present"))
            .clone()
    };
    let task = |i: usize| format!("task{i}");

    // Per tick: the snapshot, and the declared and reader groups' schemas
    // for the hit/miss checks against the tick before.
    let mut prev: Option<SnapshotV3> = None;
    let mut tick = |label: &str,
                    generation: u64,
                    reader_slots: &[(usize, Option<u64>, Option<i64>)],
                    declared_md0: &[(&str, &str)],
                    default_members: Vec<Want>,
                    declared_rebuilt: bool,
                    reader_rebuilt: bool| {
        let Snapshot::V3(s) = build(contract_external(generation)) else {
            panic!("expected V3")
        };

        // Snapshot metadata.
        let keys: Vec<&str> = {
            let mut k: Vec<&str> = s.metadata.keys().map(String::as_str).collect();
            k.sort();
            k
        };
        assert_eq!(
            keys,
            [
                "clock_anchor_wall_ns",
                "producer_epoch",
                "source",
                "ts",
                "version",
                "wall_offset"
            ],
            "{label}: snapshot metadata keys"
        );
        assert_eq!(
            s.metadata["producer_epoch"],
            crate::agent::epoch::producer_epoch().to_string()
        );
        assert_eq!(
            s.metadata["clock_anchor_wall_ns"],
            crate::agent::epoch::clock_anchor_wall_ns().to_string()
        );
        assert_eq!(s.metadata["source"], env!("CARGO_BIN_NAME"));
        assert_eq!(s.metadata["version"], env!("CARGO_PKG_VERSION"));

        // Sorted by name, and a `log_` metric is nowhere.
        assert!(s.groups.windows(2).all(|w| w[0].name < w[1].name));
        for g in &s.groups {
            let schema = g.schema.as_ref().unwrap();
            for d in schema
                .counters
                .iter()
                .chain(&schema.gauges)
                .chain(&schema.histograms)
            {
                assert_ne!(
                    d.metadata.get("metric").map(String::as_str),
                    Some("log_v3c_counter")
                );
                assert!(!d.metadata.contains_key("acq_group"));
            }
        }

        let declared = find(&s, "unattributed/v3_contract_declared");
        let mut cgroup_md0: Vec<(&str, &str)> = vec![("id", "0")];
        cgroup_md0.extend_from_slice(declared_md0);
        assert_group(
            label,
            &declared,
            vec![
                want("v3c_d_counter", None, &[], C(Some(d_counter.value()))),
                want(
                    "v3c_d_gauge",
                    None,
                    &[("unit", "bytes")],
                    G(Some(d_gauge.value())),
                ),
                want("v3c_d_cgroup", Some(0), &cgroup_md0, C(Some(5))),
                want("v3c_d_cgroup", Some(1), &[], C(None)),
                want(
                    "v3c_d_cgroup",
                    Some(2),
                    &[("cpu", "2"), ("id", "override")],
                    C(None),
                ),
                want("v3c_d_cgroup", Some(3), &[], C(None)),
                want("v3c_d_cgroup", Some(4), &[], C(None)),
                want("v3c_d_cgroup", Some(5), &[], C(None)),
                want("v3c_d_ggroup", Some(0), &[], G(None)),
                want("v3c_d_ggroup", Some(1), &[], G(Some(11))),
                want("v3c_d_ggroup", Some(2), &[], G(None)),
                want("v3c_d_ggroup", Some(3), &[], G(None)),
                want(
                    "v3c_d_hist",
                    None,
                    &[("grouping_power", "4"), ("max_value_power", "10")],
                    H(d_hist.load()),
                ),
            ],
        );
        assert_eq!(
            declared.window,
            CONTRACT_DECLARED.window(),
            "{label}: a declared group carries its sampler's window"
        );

        let bounded = find(&s, "unattributed/v3_contract_bounded");
        assert_group(
            label,
            &bounded,
            vec![
                want("v3c_b_cgroup", Some(0), &[], C(Some(1))),
                want("v3c_b_cgroup", Some(1), &[], C(None)),
                want("v3c_b_cgroup", Some(2), &[], C(None)),
            ],
        );
        assert_eq!(bounded.window, None, "{label}: never stamped");

        let set = find(&s, "unattributed/v3_contract_set");
        assert_group(
            label,
            &set,
            vec![
                want("v3c_s_ggroup", Some(1), &[], G(None)),
                want("v3c_s_ggroup", Some(5), &[], G(Some(55))),
            ],
        );

        let reader = find(&s, "unattributed/v3_contract_reader");
        let mut members = Vec::new();
        for &(idx, c, g) in reader_slots {
            let name = task(idx);
            members.push(want("v3c_r_cgroup", Some(idx), &[("name", &name)], C(c)));
            members.push(want("v3c_r_ggroup", Some(idx), &[("name", &name)], G(g)));
        }
        assert_group(label, &reader, members);
        // Not compared with `CONTRACT_READER.window()`: every builder in the
        // process reads this group while it exists, so another test's build
        // can restamp it between this build and the comparison.
        let window = reader.window.expect("the build's own read is the window");
        assert!(window.end_ns >= window.begin_ns);

        let unstamped = find(&s, "unattributed/v3_contract_unstamped");
        assert_group(
            label,
            &unstamped,
            vec![want("v3c_u_counter", None, &[], C(Some(1)))],
        );
        assert_eq!(unstamped.window, None);

        let (descs, values) = my_members(&find(&s, "unattributed/main"), "v3c_x_");
        let (schema, want_values) = expected(default_members);
        let want_descs: Vec<MetricDesc> = schema
            .counters
            .into_iter()
            .chain(schema.gauges)
            .chain(schema.histograms)
            .collect();
        assert_eq!(descs, want_descs, "{label}: default group members");
        assert_eq!(values, want_values, "{label}: default group values");
        assert_eq!(find(&s, "unattributed/main").window, None);

        assert_external(label, &find(&s, "external/main"), generation);

        if let Some(prev) = &prev {
            let reused = |name: &str| {
                Arc::ptr_eq(
                    find(prev, name).schema.as_ref().unwrap(),
                    find(&s, name).schema.as_ref().unwrap(),
                )
            };
            assert_eq!(
                reused("unattributed/v3_contract_declared"),
                !declared_rebuilt,
                "{label}: declared group reused its schema"
            );
            assert_eq!(
                reused("unattributed/v3_contract_reader"),
                !reader_rebuilt,
                "{label}: reader group reused its schema"
            );
            assert!(reused("unattributed/v3_contract_bounded"));
            assert!(reused("unattributed/v3_contract_set"));
            assert!(reused("unattributed/v3_contract_unstamped"));
            let prev_reader = find(prev, "unattributed/v3_contract_reader")
                .window
                .unwrap();
            assert!(
                window.begin_ns >= prev_reader.end_ns,
                "{label}: each build brackets its own read"
            );
        }
        prev = Some(s);
    };

    let default = |c0: u64, c1: Option<u64>, g2: Option<i64>, hist: bool| {
        let mut m = vec![
            want("v3c_x_cgroup", Some(0), &[], C(Some(c0))),
            want("v3c_x_counter", None, &[], C(Some(2))),
        ];
        if let Some(c1) = c1 {
            m.push(want("v3c_x_cgroup", Some(1), &[], C(Some(c1))));
        }
        if let Some(g2) = g2 {
            m.push(want("v3c_x_ggroup", Some(2), &[], G(Some(g2))));
        }
        if hist {
            m.push(want(
                "v3c_x_hist",
                None,
                &[("grouping_power", "4"), ("max_value_power", "10")],
                H(x_hist.load()),
            ));
        }
        m
    };
    let cpu0 = [("cpu", "0")];

    tick(
        "t1 cold",
        1,
        &[(3, Some(3), Some(3)), (17, Some(17), Some(17))],
        &cpu0,
        default(9, None, Some(4), false),
        true,
        true,
    );

    d_counter.add(1);
    r_cgroup.add(3, 1);
    x_cgroup.add(0, 1);
    tick(
        "t2 values only",
        2,
        &[(3, Some(4), Some(3)), (17, Some(17), Some(17))],
        &cpu0,
        default(10, None, Some(4), false),
        false,
        false,
    );

    // A reader slot assigned (its gauge never written: a member with no
    // reading), and a default member crossing zero.
    r_cgroup.set_metadata(40, md(&[("name", "task40")]));
    r_ggroup.set_metadata(40, md(&[("name", "task40")]));
    r_cgroup.add(40, 4);
    x_cgroup.add(1, 1);
    let three = [
        (3, Some(4), Some(3)),
        (17, Some(17), Some(17)),
        (40, Some(4), None),
    ];
    tick(
        "t3 slot assigned",
        3,
        &three,
        &cpu0,
        default(10, Some(1), Some(4), false),
        false,
        true,
    );
    tick(
        "t4 unchanged",
        4,
        &three,
        &cpu0,
        default(10, Some(1), Some(4), false),
        false,
        false,
    );

    // A reader slot released, and metadata replaced at a stable index.
    r_cgroup.clear_metadata(17);
    r_ggroup.clear_metadata(17);
    d_cgroup.set_metadata(0, md(&[("cpu", "0"), ("node", "1")]));
    let two = [(3, Some(4), Some(3)), (40, Some(4), None)];
    let cpu0_node1 = [("cpu", "0"), ("node", "1")];
    tick(
        "t5 slot released, metadata replaced",
        5,
        &two,
        &cpu0_node1,
        default(10, Some(1), Some(4), false),
        true,
        true,
    );

    // The declared window restamped; both histograms load. The declared
    // histogram was already a member, so its group keeps its schema; the
    // default one joins its group.
    CONTRACT_DECLARED.acquire().finish();
    let _ = d_hist.increment(5);
    let _ = x_hist.increment(9);
    tick(
        "t6 restamped, histograms load",
        6,
        &two,
        &cpu0_node1,
        default(10, Some(1), Some(4), true),
        false,
        false,
    );
    tick(
        "t7 unchanged",
        7,
        &two,
        &cpu0_node1,
        default(10, Some(1), Some(4), true),
        false,
        false,
    );

    // A default gauge member returns to unwritten and leaves.
    x_ggroup.set(2, i64::MIN);
    tick(
        "t8 default member leaves",
        8,
        &two,
        &cpu0_node1,
        default(10, Some(1), None, true),
        false,
        false,
    );
    tick(
        "t9 unchanged",
        9,
        &two,
        &cpu0_node1,
        default(10, Some(1), None, true),
        false,
        false,
    );

    drop((
        d_counter,
        d_gauge,
        d_cgroup,
        d_ggroup,
        d_hist,
        b_cgroup,
        s_ggroup,
        r_cgroup,
        r_ggroup,
        u_counter,
        x_cgroup,
        x_ggroup,
        x_hist,
        x_counter,
        log_counter,
    ));
}

fn contract_build(builder: &mut V3Builder) -> impl FnMut(Vec<ExternalMetric>) -> Snapshot + '_ {
    move |external| {
        let stamp = crate::agent::epoch::anchored_now();
        create_v3(Duration::from_millis(3), external, builder, stamp)
    }
}

#[test]
fn v3_snapshot_contract() {
    let mut builder = v3_builder();
    run_v3_contract(&mut contract_build(&mut builder));
}

// ---------------------------------------------------------------------------
// Cost: per-tick build time of a representative registry.

static BENCH_TASK: AcquisitionGroup =
    AcquisitionGroup::new_reader_stamped("unattributed", "bench_task");
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static BENCH_TASK_ENTRY: &'static AcquisitionGroup = &BENCH_TASK;

static BENCH_CGROUP: AcquisitionGroup =
    AcquisitionGroup::new_reader_stamped("unattributed", "bench_cgroup");
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static BENCH_CGROUP_ENTRY: &'static AcquisitionGroup = &BENCH_CGROUP;

static BENCH_CPU_A: AcquisitionGroup = AcquisitionGroup::new("unattributed", "bench_cpu_a");
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static BENCH_CPU_A_ENTRY: &'static AcquisitionGroup = &BENCH_CPU_A;
static BENCH_CPU_B: AcquisitionGroup = AcquisitionGroup::new("unattributed", "bench_cpu_b");
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static BENCH_CPU_B_ENTRY: &'static AcquisitionGroup = &BENCH_CPU_B;
static BENCH_CPU_C: AcquisitionGroup = AcquisitionGroup::new("unattributed", "bench_cpu_c");
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static BENCH_CPU_C_ENTRY: &'static AcquisitionGroup = &BENCH_CPU_C;

static BENCH_SCALARS: AcquisitionGroup = AcquisitionGroup::new("unattributed", "bench_scalars");
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static BENCH_SCALARS_ENTRY: &'static AcquisitionGroup = &BENCH_SCALARS;

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() as f64 * p) as usize).min(sorted.len() - 1)]
}

/// Per-tick `create_v3` cost on a registry shaped like a busy host: a
/// 4096-slot per-task group (4 counter groups, 2,500 live tasks), a 512-slot
/// per-cgroup group (3 counter groups, 300 live cgroups), three bounded
/// per-CPU groups (64 CPUs), and 200 scalars, on top of every metric this
/// test binary registers. Reports cache-hit ticks and membership-change
/// ticks.
///
/// `cargo test --release --bin rezolus v3_contract::v3_build_cost -- --ignored --nocapture --test-threads=1`
#[test]
#[ignore]
fn v3_build_cost() {
    const HIT_TICKS: usize = 3000;
    const MISS_TICKS: usize = 1000;

    // Ask for a performance core: on Apple Silicon an unqualified thread
    // migrates between core types, which moves a whole run by 10-15%.
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
    }
    const CPUS: usize = 64;

    // Per-task: 4 counter groups over one 4096-slot space, 2,500 live tasks.
    let task: Vec<DynBoxedMetric<CounterGroup>> = ["user", "system", "wait", "switches"]
        .iter()
        .map(|m| in_group(&format!("bench_task_{m}"), "bench_task").build(CounterGroup::new(4096)))
        .collect();
    for idx in 0..2_500usize {
        let labels = md(&[
            ("name", &format!("task-{idx}")),
            ("pid", &idx.to_string()),
            ("__uid__", &format!("{:016x}", idx * 7919)),
        ]);
        for g in &task {
            g.set_metadata(idx, labels.clone());
            g.add(idx, idx as u64 + 1);
        }
    }

    // Per-cgroup: 3 counter groups over 512 slots, 300 live cgroups.
    let cgroup: Vec<DynBoxedMetric<CounterGroup>> = ["cycles", "instructions", "throttled"]
        .iter()
        .map(|m| {
            in_group(&format!("bench_cgroup_{m}"), "bench_cgroup").build(CounterGroup::new(512))
        })
        .collect();
    for idx in 0..300usize {
        let labels = md(&[("name", &format!("/system.slice/unit-{idx}.service"))]);
        for g in &cgroup {
            g.set_metadata(idx, labels.clone());
            g.add(idx, 1);
        }
    }

    // Per-CPU: three stamped groups of 64-entry counter groups, bounded.
    let mut percpu: Vec<DynBoxedMetric<CounterGroup>> = Vec::new();
    for (group, ag, metrics) in [
        ("bench_cpu_a", &BENCH_CPU_A, 4usize),
        ("bench_cpu_b", &BENCH_CPU_B, 3),
        ("bench_cpu_c", &BENCH_CPU_C, 2),
    ] {
        ag.set_member_bound(CPUS);
        for m in 0..metrics {
            let g = in_group(&format!("{group}_{m}"), group).build(CounterGroup::new(1024));
            for cpu in 0..CPUS {
                g.set_metadata(cpu, md(&[("cpu", &cpu.to_string())]));
                g.add(cpu, 1);
            }
            percpu.push(g);
        }
        let guard = ag.acquire();
        guard.finish();
    }

    // ~200 scalars: 100 declared, 100 in the default group.
    let mut counters: Vec<DynBoxedMetric<Counter>> = Vec::new();
    let mut gauges: Vec<DynBoxedMetric<Gauge>> = Vec::new();
    for i in 0..50 {
        counters.push(in_group(&format!("bench_sc_{i}"), "bench_scalars").build(Counter::new()));
        gauges.push(in_group(&format!("bench_sg_{i}"), "bench_scalars").build(Gauge::new()));
        counters.push(MetricBuilder::new(format!("bench_dc_{i}")).build(Counter::new()));
        gauges.push(MetricBuilder::new(format!("bench_dg_{i}")).build(Gauge::new()));
    }
    for c in &counters {
        c.add(1);
    }
    for g in &gauges {
        g.set(1);
    }

    let mut builder = v3_builder();
    let mut build = || {
        let start = Instant::now();
        let s = create_v3(Duration::ZERO, Vec::new(), &mut builder, (0, 0));
        let elapsed = start.elapsed();
        std::hint::black_box(s);
        elapsed
    };

    for _ in 0..5 {
        build();
    }

    let mut hit = Vec::with_capacity(HIT_TICKS);
    for i in 0..HIT_TICKS {
        for g in &task {
            g.add(i % 2_500, 1);
        }
        hit.push(build());
    }

    // A membership-change tick: one task slot assigned and the previous one
    // released, so the task group misses and every other group hits. The
    // live population stays at 2,501.
    let mut miss = Vec::with_capacity(MISS_TICKS);
    for i in 0..MISS_TICKS {
        let idx = 2_500 + (i % 1_500);
        let prev = 2_500 + ((i + 1_499) % 1_500);
        for g in &task {
            g.set_metadata(idx, md(&[("name", &format!("task-{idx}"))]));
            g.add(idx, 1);
            if i > 0 {
                g.clear_metadata(prev);
            }
        }
        miss.push(build());
    }

    let report = |label: &str, v: &mut Vec<Duration>| {
        v.sort();
        println!(
            "{label:<24} p10 {:>6.0} us  p50 {:>6.0} us  p99 {:>6.0} us",
            us(percentile(v, 0.1)),
            us(percentile(v, 0.5)),
            us(percentile(v, 0.99)),
        );
    };
    println!(
        "\nregistry: {} entries; {HIT_TICKS} hit ticks, {MISS_TICKS} membership-change ticks",
        metriken::metrics().iter().count()
    );
    report("cache-hit tick", &mut hit);
    report("membership-change tick", &mut miss);

    drop((task, cgroup, percpu, counters, gauges));
}
