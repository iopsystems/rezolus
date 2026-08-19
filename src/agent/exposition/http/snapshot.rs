use crate::agent::config::SnapshotFormat;
use crate::agent::external_metrics::{ExternalMetric, ExternalMetricValue, ExternalMetricsStore};
use crate::agent::timing::AcquisitionGroup;
use crate::agent::*;

use metriken::{Value, Window};
use metriken_exposition::{
    Counter, Gauge, GroupSchema, GroupSnapshot, Histogram, MetricDesc, Snapshot, SnapshotV2,
    SnapshotV3,
};

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime};

pub struct SnapshotBuilder {
    cached: Option<CachedSnapshot>,
    samplers: Arc<Box<[Box<dyn Sampler>]>>,
    ttl: Duration,
    external_store: Option<Arc<ExternalMetricsStore>>,
    format: SnapshotFormat,
    skeleton_cache: SkeletonCache,
}

struct CachedSnapshot {
    timestamp: Instant,
    snapshot: Snapshot,
}

impl SnapshotBuilder {
    pub fn new(
        config: Arc<Config>,
        samplers: Arc<Box<[Box<dyn Sampler>]>>,
        external_store: Option<Arc<ExternalMetricsStore>>,
    ) -> Self {
        Self {
            cached: None,
            samplers,
            ttl: config.general().ttl(),
            external_store,
            format: config.general().snapshot_format(),
            skeleton_cache: SkeletonCache::new(),
        }
    }

    async fn refresh(&mut self) {
        let last = Instant::now();

        let timestamp = SystemTime::now();

        let s: Vec<_> = self
            .samplers
            .iter()
            .map(|s| s.refresh_with_logging())
            .collect();

        let start = Instant::now();
        futures::future::join_all(s).await;
        let duration = start.elapsed();
        debug!("sampling latency: {} us", duration.as_micros());

        let external_metrics = if let Some(store) = &self.external_store {
            store.cleanup();
            store.get_active()
        } else {
            Vec::new()
        };

        let snapshot = match self.format {
            SnapshotFormat::V2 => create(timestamp, duration, external_metrics),
            SnapshotFormat::V3 => create_v3(
                timestamp,
                duration,
                external_metrics,
                &mut self.skeleton_cache,
            ),
        };

        self.cached = Some(CachedSnapshot {
            snapshot,
            timestamp: last,
        });
    }

    pub async fn build(&mut self, now: Instant) -> &Snapshot {
        if self.cached.is_none()
            || now.duration_since(self.cached.as_ref().unwrap().timestamp) > self.ttl
        {
            self.refresh().await;
        }

        &self.cached.as_ref().unwrap().snapshot
    }
}

fn create(
    timestamp: SystemTime,
    duration: Duration,
    external_metrics: Vec<ExternalMetric>,
) -> Snapshot {
    let mut s = SnapshotV2 {
        systemtime: timestamp,
        duration,
        metadata: [
            ("source".to_string(), env!("CARGO_BIN_NAME").to_string()),
            ("version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
        ]
        .into(),
        counters: Vec::new(),
        gauges: Vec::new(),
        histograms: Vec::new(),
    };

    let sampler_mods = crate::agent::samplers::sampler_modules();

    for (metric_id, metric) in metriken::metrics().iter().enumerate() {
        let (value, stored_window) = metric.value_with_window();

        if value.is_none() {
            continue;
        }

        let name = metric.name();

        if name.starts_with("log_") {
            continue;
        }

        let mut metadata: HashMap<String, String> =
            [("metric".to_string(), name.to_string())].into();

        for (k, v) in metric.metadata().iter() {
            metadata.insert(k.to_string(), v.to_string());
        }

        let sampler = crate::agent::samplers::attribute_sampler(metric.module(), &sampler_mods);
        metadata.insert("sampler".to_string(), sampler.to_string());

        let name = format!("{metric_id}");

        match value {
            Some(Value::Counter(value)) => s
                .counters
                .push(Counter::new(name, value, metadata).with_window(stored_window)),
            Some(Value::Gauge(value)) => s
                .gauges
                .push(Gauge::new(name, value, metadata).with_window(stored_window)),
            Some(Value::CounterGroup(g)) => {
                for counter_id in 0..g.entries() {
                    // Atomic pair read: value + window under one lock, so a
                    // concurrent writer can never pair a fresh value with a
                    // stale window (drivehealth's async tear surface).
                    let (value, window) = g.load_with_window(counter_id);
                    let Some(value) = value else { continue };
                    if value == 0 {
                        continue;
                    }
                    let mut metadata = metadata.clone();

                    metadata.insert("id".to_string(), counter_id.to_string());

                    if let Some(m) = g.load_metadata(counter_id) {
                        for (k, v) in m {
                            metadata.insert(k, v);
                        }
                    }

                    s.counters.push(
                        Counter::new(format!("{metric_id}x{counter_id}"), value, metadata)
                            .with_window(window),
                    )
                }
            }
            Some(Value::GaugeGroup(g)) => {
                for gauge_id in 0..g.entries() {
                    // Atomic pair read (see CounterGroup arm above).
                    let (value, window) = g.load_with_window(gauge_id);
                    let Some(value) = value else { continue };
                    if value == i64::MIN {
                        continue;
                    }

                    let mut metadata = metadata.clone();

                    metadata.insert("id".to_string(), gauge_id.to_string());

                    if let Some(m) = g.load_metadata(gauge_id) {
                        for (k, v) in m {
                            metadata.insert(k, v);
                        }
                    }

                    s.gauges.push(
                        Gauge::new(format!("{metric_id}x{gauge_id}"), value, metadata)
                            .with_window(window),
                    )
                }
            }
            Some(Value::Histogram(h)) => {
                if let Some(value) = h.load() {
                    metadata.insert(
                        "grouping_power".to_string(),
                        h.config().grouping_power().to_string(),
                    );
                    metadata.insert(
                        "max_value_power".to_string(),
                        h.config().max_value_power().to_string(),
                    );

                    s.histograms
                        .push(Histogram::new(name, value, metadata).with_window(stored_window))
                }
            }
            _ => {}
        }
    }

    for metric in external_metrics.into_iter() {
        // Capture the window before metric fields are consumed by the moves below.
        // Window is Copy so this is free; precedence level 2 (external source stamp).
        let window = metric.window;

        let mut metadata: HashMap<String, String> = [
            ("metric".to_string(), metric.name.clone()),
            ("source".to_string(), "external".to_string()),
        ]
        .into();

        for (k, v) in metric.labels {
            metadata.insert(k, v);
        }

        let name = String::new();

        match metric.value {
            ExternalMetricValue::Counter(value) => {
                s.counters
                    .push(Counter::new(name, value, metadata).with_window(window));
            }
            ExternalMetricValue::Gauge(value) => {
                s.gauges
                    .push(Gauge::new(name, value, metadata).with_window(window));
            }
            ExternalMetricValue::Histogram {
                grouping_power,
                max_value_power,
                buckets,
            } => {
                if let Ok(value) =
                    histogram::Histogram::from_buckets(grouping_power, max_value_power, buckets)
                {
                    metadata.insert("grouping_power".to_string(), grouping_power.to_string());
                    metadata.insert("max_value_power".to_string(), max_value_power.to_string());

                    s.histograms
                        .push(Histogram::new(name, value, metadata).with_window(window));
                }
            }
        }
    }

    Snapshot::V2(s)
}

/// Per-group schema cache for [`create_v3`], keyed by group name
/// (`"<sampler>/<name>"`).
///
/// Each tick, `create_v3` still has to assemble a fresh [`GroupSchema`] for
/// every group to know each member's current metadata (that assembly is no
/// more expensive than what V2's `create` already does every tick — no
/// regression there). What the cache actually buys is skipping
/// [`GroupSchema::hash`]: a full canonical msgpack serialization of the
/// schema followed by an FNV-1a-128 fold, the one per-tick cost that scales
/// with schema size rather than with "did anything change". When a group's
/// member-name list (the cheap part — just the `MetricDesc::name` strings,
/// already produced while building the schema) is byte-identical to the
/// previous tick's, the freshly assembled schema is discarded in favor of
/// the cached one and its hash is reused verbatim; only a genuine membership
/// change (a metric added/removed from the group) pays for a fresh hash.
pub(crate) struct SkeletonCache {
    entries: HashMap<String, GroupSkeleton>,
    rebuilds: u64,
}

struct GroupSkeleton {
    member_names: Vec<String>,
    schema: GroupSchema,
    hash: (u64, u64),
}

impl SkeletonCache {
    pub(crate) fn new() -> Self {
        Self {
            entries: HashMap::new(),
            rebuilds: 0,
        }
    }

    /// Number of times a group's schema has been rebuilt (hash recomputed)
    /// since this cache was created. Exposed for tests.
    #[allow(dead_code)]
    pub(crate) fn rebuilds(&self) -> u64 {
        self.rebuilds
    }
}

/// Per-group accumulation while walking the metriken registry: the group's
/// shared acquisition window plus, per kind, the (descriptor, value) pairs
/// in schema order.
#[derive(Default)]
struct GroupBuilder {
    window: Option<Window>,
    counters: Vec<(MetricDesc, Option<u64>)>,
    gauges: Vec<(MetricDesc, Option<i64>)>,
    histograms: Vec<(MetricDesc, Option<histogram::Histogram>)>,
}

/// Build a `SnapshotV3` (acquisition-group snapshot) from the current
/// metriken registry, mirroring `create`'s walk/naming/metadata rules with
/// one addition: routing each entry into an acquisition group.
///
/// # Routing
///
/// A metric whose own metadata carries `acq_group = "<name>"` routes to the
/// declared group `"<sampler>/<name>"`, provided `(sampler, name)` is
/// actually registered on [`crate::agent::samplers::ACQUISITION_GROUPS`].
/// Everything else — the overwhelming majority of metrics today, since no
/// sampler has migrated yet — falls into that sampler's default group,
/// `"<sampler>/main"`.
///
/// A metric that names an `acq_group` with no matching registry entry is a
/// migration bug (a typo, or a group that was renamed on one side and not
/// the other): it is routed to the default group so the tick still produces
/// a valid snapshot, but `debug_assert!` catches it in tests/debug builds
/// rather than letting it pass silently in release.
///
/// # Default vs. declared group semantics
///
/// Default groups keep V2's transitional membership semantics: a
/// `CounterGroup` entry reading exactly `0`, or a `GaugeGroup` entry reading
/// exactly `i64::MIN`, is treated as "not really there" and skipped, exactly
/// as `create` does today. This is a known compromise carried over
/// unchanged from V2 — real zero/never-set values are indistinguishable —
/// and it is deliberately kept ONLY here, in the pre-migration default
/// groups, so flipping the wire format to V3 does not by itself explode
/// cardinality with a flood of phantom dense slots (every possible CPU,
/// device, or task index some sampler could ever report on, most of them
/// unpopulated). Default groups also carry no acquisition window
/// (`window: None`): V2 attached a window to every individual metric, but a
/// default group has no registered acquisition boundary to report — that
/// returns metric-by-metric once each sampler declares real groups.
///
/// Declared groups use registration membership instead: every entry in
/// `0..group.entries()` is a member, full stop. Counter/gauge group entries
/// send `Some(value)` including an honest zero — no sentinel skip — and a
/// group whose backing store was never written at all reports `None` for
/// every member ("registered but no reading yet"), never fabricating a
/// value or silently dropping the member. The group's window comes from the
/// registered [`AcquisitionGroup`]'s own window slot, not from any
/// per-metric window.
///
/// External metrics land in a single windowless `"external/main"` group;
/// their own per-metric windows are intentionally dropped here (a
/// `GroupSnapshot` carries one window for the whole group) and will return
/// once external sources get real declared groups of their own.
fn create_v3(
    timestamp: SystemTime,
    duration: Duration,
    external_metrics: Vec<ExternalMetric>,
    cache: &mut SkeletonCache,
) -> Snapshot {
    let sampler_mods = crate::agent::samplers::sampler_modules();

    let mut group_registry: HashMap<String, &'static AcquisitionGroup> = HashMap::new();
    for group in crate::agent::samplers::ACQUISITION_GROUPS {
        group_registry.insert(format!("{}/{}", group.sampler, group.name), group);
    }

    let mut groups: HashMap<String, GroupBuilder> = HashMap::new();

    for (metric_id, metric) in metriken::metrics().iter().enumerate() {
        let Some(value) = metric.value() else {
            continue;
        };

        let name = metric.name();

        if name.starts_with("log_") {
            continue;
        }

        let mut metadata: BTreeMap<String, String> =
            [("metric".to_string(), name.to_string())].into();

        for (k, v) in metric.metadata().iter() {
            metadata.insert(k.to_string(), v.to_string());
        }

        let sampler = crate::agent::samplers::attribute_sampler(metric.module(), &sampler_mods);
        metadata.insert("sampler".to_string(), sampler.to_string());

        // Route: a declared `acq_group` wins only if it actually resolves
        // against the registry; otherwise fall back to the sampler's
        // default group (and flag the mismatch in debug builds — see the
        // function-level doc comment).
        let mut declared = false;
        let mut group_window = None;
        let group_key = match metric.metadata().get("acq_group") {
            Some(acq_group) => {
                let key = format!("{sampler}/{acq_group}");
                match group_registry.get(&key) {
                    Some(group) => {
                        declared = true;
                        group_window = group.window();
                        key
                    }
                    None => {
                        debug_assert!(
                            false,
                            "metric `{name}` declares acq_group=\"{acq_group}\" for sampler \
                             `{sampler}`, but no AcquisitionGroup (\"{sampler}\", \
                             \"{acq_group}\") is registered on ACQUISITION_GROUPS; routing to \
                             the default group instead",
                        );
                        format!("{sampler}/main")
                    }
                }
            }
            None => format!("{sampler}/main"),
        };

        let group = groups.entry(group_key).or_insert_with(|| GroupBuilder {
            window: group_window,
            ..Default::default()
        });

        let entry_name = format!("{metric_id}");

        match value {
            Value::Counter(v) => group.counters.push((
                MetricDesc {
                    name: entry_name,
                    metadata,
                },
                Some(v),
            )),
            Value::Gauge(v) => group.gauges.push((
                MetricDesc {
                    name: entry_name,
                    metadata,
                },
                Some(v),
            )),
            Value::CounterGroup(g) => {
                for idx in 0..g.entries() {
                    let v = g.counter_value(idx);

                    // Transitional V2-style sentinel skip — default groups only. See doc comment.
                    if !declared {
                        let Some(v) = v else { continue };
                        if v == 0 {
                            continue;
                        }
                    }

                    let mut entry_metadata = metadata.clone();
                    entry_metadata.insert("id".to_string(), idx.to_string());
                    if let Some(m) = g.load_metadata(idx) {
                        for (k, v) in m {
                            entry_metadata.insert(k, v);
                        }
                    }

                    group.counters.push((
                        MetricDesc {
                            name: format!("{metric_id}x{idx}"),
                            metadata: entry_metadata,
                        },
                        v,
                    ));
                }
            }
            Value::GaugeGroup(g) => {
                for idx in 0..g.entries() {
                    let v = g.gauge_value(idx);

                    // Transitional V2-style sentinel skip — default groups only. See doc comment.
                    if !declared {
                        let Some(v) = v else { continue };
                        if v == i64::MIN {
                            continue;
                        }
                    }

                    let mut entry_metadata = metadata.clone();
                    entry_metadata.insert("id".to_string(), idx.to_string());
                    if let Some(m) = g.load_metadata(idx) {
                        for (k, v) in m {
                            entry_metadata.insert(k, v);
                        }
                    }

                    group.gauges.push((
                        MetricDesc {
                            name: format!("{metric_id}x{idx}"),
                            metadata: entry_metadata,
                        },
                        v,
                    ));
                }
            }
            Value::Histogram(h) => {
                if let Some(hv) = h.load() {
                    let mut entry_metadata = metadata;
                    entry_metadata.insert(
                        "grouping_power".to_string(),
                        h.config().grouping_power().to_string(),
                    );
                    entry_metadata.insert(
                        "max_value_power".to_string(),
                        h.config().max_value_power().to_string(),
                    );

                    group.histograms.push((
                        MetricDesc {
                            name: entry_name,
                            metadata: entry_metadata,
                        },
                        Some(hv),
                    ));
                }
            }
            _ => {}
        }
    }

    // External metrics: one windowless group, own naming scheme (they are
    // not metriken registry entries, so there is no metric_id to key on).
    if !external_metrics.is_empty() {
        let group = groups.entry("external/main".to_string()).or_default();

        for (i, metric) in external_metrics.into_iter().enumerate() {
            let mut metadata: BTreeMap<String, String> = [
                ("metric".to_string(), metric.name.clone()),
                ("source".to_string(), "external".to_string()),
            ]
            .into();

            for (k, v) in metric.labels {
                metadata.insert(k, v);
            }

            let entry_name = format!("external{i}");

            match metric.value {
                ExternalMetricValue::Counter(v) => {
                    group.counters.push((
                        MetricDesc {
                            name: entry_name,
                            metadata,
                        },
                        Some(v),
                    ));
                }
                ExternalMetricValue::Gauge(v) => {
                    group.gauges.push((
                        MetricDesc {
                            name: entry_name,
                            metadata,
                        },
                        Some(v),
                    ));
                }
                ExternalMetricValue::Histogram {
                    grouping_power,
                    max_value_power,
                    buckets,
                } => {
                    if let Ok(hv) =
                        histogram::Histogram::from_buckets(grouping_power, max_value_power, buckets)
                    {
                        metadata.insert("grouping_power".to_string(), grouping_power.to_string());
                        metadata.insert("max_value_power".to_string(), max_value_power.to_string());
                        group.histograms.push((
                            MetricDesc {
                                name: entry_name,
                                metadata,
                            },
                            Some(hv),
                        ));
                    }
                }
            }
        }
    }

    let mut group_snapshots: Vec<GroupSnapshot> = Vec::with_capacity(groups.len());

    for (group_name, group) in groups {
        let (counter_descs, counter_values): (Vec<MetricDesc>, Vec<Option<u64>>) =
            group.counters.into_iter().unzip();
        let (gauge_descs, gauge_values): (Vec<MetricDesc>, Vec<Option<i64>>) =
            group.gauges.into_iter().unzip();
        let (histogram_descs, histogram_values): (
            Vec<MetricDesc>,
            Vec<Option<histogram::Histogram>>,
        ) = group.histograms.into_iter().unzip();

        let schema = GroupSchema {
            counters: counter_descs,
            gauges: gauge_descs,
            histograms: histogram_descs,
        };

        let names_match = cache.entries.get(&group_name).is_some_and(|cached| {
            let total = schema.counters.len() + schema.gauges.len() + schema.histograms.len();
            cached.member_names.len() == total
                && cached.member_names.iter().eq(schema
                    .counters
                    .iter()
                    .chain(schema.gauges.iter())
                    .chain(schema.histograms.iter())
                    .map(|d| &d.name))
        });

        let (schema, hash) = if names_match {
            let cached = cache.entries.get(&group_name).expect("checked above");
            (cached.schema.clone(), cached.hash)
        } else {
            let hash = schema.hash();
            let member_names: Vec<String> = schema
                .counters
                .iter()
                .chain(schema.gauges.iter())
                .chain(schema.histograms.iter())
                .map(|d| d.name.clone())
                .collect();
            cache.entries.insert(
                group_name.clone(),
                GroupSkeleton {
                    member_names,
                    schema: schema.clone(),
                    hash,
                },
            );
            cache.rebuilds += 1;
            (schema, hash)
        };

        group_snapshots.push(GroupSnapshot {
            name: group_name,
            schema_hash: hash,
            schema: Some(schema),
            window: group.window,
            counters: counter_values,
            gauges: gauge_values,
            histograms: histogram_values,
        });
    }

    group_snapshots.sort_by(|a, b| a.name.cmp(&b.name));

    Snapshot::V3(SnapshotV3 {
        systemtime: timestamp,
        duration,
        metadata: [
            ("source".to_string(), env!("CARGO_BIN_NAME").to_string()),
            ("version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
        ]
        .into(),
        groups: group_snapshots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::external_metrics::{ExternalMetric, ExternalMetricValue};
    use metriken::metric;
    use metriken::Window;
    use std::time::{Duration, SystemTime};

    #[metric(name = "snapshot_sampler_label_probe")]
    static SAMPLER_LABEL_PROBE: metriken::Counter = metriken::Counter::new();

    #[test]
    fn built_snapshot_metric_carries_a_sampler_label() {
        SAMPLER_LABEL_PROBE.increment();
        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![]);
        let Snapshot::V2(s) = snap else {
            panic!("expected V2")
        };
        let c = s
            .counters
            .iter()
            .find(|c| {
                c.metadata.get("metric").map(String::as_str) == Some("snapshot_sampler_label_probe")
            })
            .expect("probe counter present");
        assert_eq!(
            c.metadata.get("sampler").map(String::as_str),
            Some("unattributed")
        );
    }

    #[test]
    fn every_registered_sampler_module_self_attributes() {
        let mods = crate::agent::samplers::sampler_modules();
        for (module, name) in &mods {
            assert_eq!(
                crate::agent::samplers::attribute_sampler(module, &mods),
                *name,
                "sampler module {module} should attribute to {name}",
            );
        }
    }

    #[test]
    fn external_metric_carries_its_own_window_not_fleet_time() {
        let win = Window::new(1_000, 2_000);
        let ext = ExternalMetric {
            name: "ext_counter".into(),
            labels: Default::default(),
            value: ExternalMetricValue::Counter(7),
            last_updated: std::time::Instant::now(),
            window: Some(win),
        };
        let snap = create(SystemTime::now(), Duration::from_secs(5), vec![ext]);
        let Snapshot::V2(s) = snap else {
            panic!("expected V2")
        };
        let c = s
            .counters
            .iter()
            .find(|c| c.metadata.get("metric").map(String::as_str) == Some("ext_counter"))
            .expect("external counter present");
        assert_eq!(c.window, Some(win), "external window preserved, not fleet");
    }

    // --- SnapshotV3 -----------------------------------------------------

    // The registered sampler for a metric defined in this test module is
    // "unattributed" (no `SAMPLERS` entry's module path prefixes this file),
    // matching `built_snapshot_metric_carries_a_sampler_label` above. The
    // synthetic acquisition group below uses that same sampler name so its
    // `acq_group` metadata actually resolves against the registry.
    static V3_PROBE_GROUP: AcquisitionGroup = AcquisitionGroup::new("unattributed", "probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_PROBE_GROUP_ENTRY: &'static AcquisitionGroup = &V3_PROBE_GROUP;

    #[metric(name = "snapshot_v3_probe_counter", metadata = { acq_group = "probe" })]
    static V3_PROBE_COUNTER: metriken::Counter = metriken::Counter::new();

    #[test]
    fn v3_snapshot_carries_declared_group_with_window_and_valid_schema() {
        V3_PROBE_COUNTER.increment();
        let guard = V3_PROBE_GROUP.acquire();
        guard.finish();

        let mut cache = SkeletonCache::new();
        let snap = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s) = snap else {
            panic!("expected V3")
        };

        for g in &s.groups {
            assert_eq!(
                g.validate(),
                Ok(()),
                "group `{}` failed to validate",
                g.name
            );
        }

        let group = s
            .groups
            .iter()
            .find(|g| g.name == "unattributed/probe")
            .expect("declared group `unattributed/probe` present");
        assert!(group.window.is_some(), "declared group carries a window");

        let schema = group.schema.as_ref().expect("schema present");
        let idx = schema
            .counters
            .iter()
            .position(|d| {
                d.metadata.get("metric").map(String::as_str) == Some("snapshot_v3_probe_counter")
            })
            .expect("probe counter present in schema");
        assert!(
            group.counters[idx].is_some(),
            "probe counter has a Some(_) value slot"
        );
    }

    #[test]
    fn unmigrated_metrics_land_in_windowless_default_groups() {
        SAMPLER_LABEL_PROBE.increment();

        let mut cache = SkeletonCache::new();
        let snap = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s) = snap else {
            panic!("expected V3")
        };

        let group = s
            .groups
            .iter()
            .find(|g| g.name == "unattributed/main")
            .expect("default group `unattributed/main` present");
        assert_eq!(group.validate(), Ok(()));
        assert!(group.window.is_none(), "default group carries no window");

        let schema = group.schema.as_ref().expect("schema present");
        assert!(
            schema.counters.iter().any(|d| {
                d.metadata.get("metric").map(String::as_str) == Some("snapshot_sampler_label_probe")
            }),
            "unmigrated probe counter present in its sampler's default group"
        );
    }

    #[test]
    fn skeleton_cache_is_stable_across_ticks() {
        SAMPLER_LABEL_PROBE.increment();

        let mut cache = SkeletonCache::new();
        let snap1 = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s1) = snap1 else {
            panic!("expected V3")
        };
        let rebuilds_after_first = cache.rebuilds();
        assert!(
            rebuilds_after_first > 0,
            "first tick builds every observed group's schema at least once"
        );

        let snap2 = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s2) = snap2 else {
            panic!("expected V3")
        };
        assert_eq!(
            cache.rebuilds(),
            rebuilds_after_first,
            "second tick with an unchanged registry rebuilds nothing"
        );

        let mut hashes1: Vec<_> = s1
            .groups
            .iter()
            .map(|g| (g.name.clone(), g.schema_hash))
            .collect();
        let mut hashes2: Vec<_> = s2
            .groups
            .iter()
            .map(|g| (g.name.clone(), g.schema_hash))
            .collect();
        hashes1.sort();
        hashes2.sort();
        assert_eq!(
            hashes1, hashes2,
            "schema hashes are stable across ticks with unchanged membership"
        );
    }

    #[test]
    fn metric_names_unique_across_groups() {
        let mut cache = SkeletonCache::new();
        let snap = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s) = snap else {
            panic!("expected V3")
        };

        let mut seen = std::collections::HashSet::new();
        for g in &s.groups {
            let schema = g.schema.as_ref().expect("schema present");
            for d in schema
                .counters
                .iter()
                .chain(schema.gauges.iter())
                .chain(schema.histograms.iter())
            {
                assert!(
                    seen.insert(d.name.clone()),
                    "duplicate MetricDesc.name `{}` (group `{}`)",
                    d.name,
                    g.name
                );
            }
        }
    }

    #[tokio::test]
    async fn v3_flag_switches_the_builder() {
        // An empty document defaults `general` via `Default::default()`
        // (empty strings), not the field-level `#[serde(default = ...)]`
        // helpers — those only apply when a `[general]` table is present.
        // Supply an explicit (empty) table so `ttl`/`listen` get their real
        // defaults.
        let v2_config: Config = toml::from_str("[general]\n").expect("valid config");
        let mut v2_builder = SnapshotBuilder::new(
            Arc::new(v2_config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );
        let snap = v2_builder.build(Instant::now()).await;
        assert!(matches!(snap, Snapshot::V2(_)), "default format is v2");

        let v3_config: Config =
            toml::from_str("[general]\nsnapshot_format = \"v3\"\n").expect("valid config");
        let mut v3_builder = SnapshotBuilder::new(
            Arc::new(v3_config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );
        let snap = v3_builder.build(Instant::now()).await;
        assert!(
            matches!(snap, Snapshot::V3(_)),
            "snapshot_format = \"v3\" selects the V3 builder"
        );
    }
}
