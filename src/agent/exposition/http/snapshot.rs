use crate::agent::config::SnapshotFormat;
use crate::agent::external_metrics::{
    ExternalMetric, ExternalMetricValue, ExternalMetricsStore, MetricKey,
};
use crate::agent::timing::AcquisitionGroup;
use crate::agent::*;

use metriken::{Value, Window};
use metriken_exposition::{
    Counter, Gauge, GroupSchema, GroupSnapshot, Histogram, MetricDesc, Snapshot, SnapshotV2,
    SnapshotV3,
};

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
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

/// Metadata common to any exposed entry for one metric: `"metric"` (its
/// name) plus everything from the metric's own static metadata, plus
/// `"sampler"` attribution — also returned standalone, since a caller (the
/// V3 builder) needs the sampler name itself for group routing, beyond
/// just having it embedded in the map. Shared between `create` (V2) and
/// `create_v3` so the two builders can't independently drift on what this
/// prefix does; `create_v3` uses the returned `BTreeMap` directly (its wire
/// format's native form) while `create` converts to a `HashMap` (V2's).
///
/// # KEEP IN SYNC
///
/// This helper covers only the shared prefix. Everything after it — the
/// `log_` skip that gates it (each caller's own, since it's a `continue` at
/// the caller's loop, not something a shared helper can do), and the
/// per-kind (Counter/Gauge/CounterGroup/GaugeGroup/Histogram)
/// walk/naming/membership rules — remains genuinely duplicated between
/// `create` and `create_v3`: `{id}`/`{id}x{idx}` naming, and the histogram
/// `grouping_power`/`max_value_power` metadata keys. A change to one must
/// be checked against the other.
fn metric_metadata(
    metric: &metriken::MetricEntry,
    sampler_mods: &[(&str, &str)],
) -> (BTreeMap<String, String>, String) {
    let mut metadata: BTreeMap<String, String> =
        [("metric".to_string(), metric.name().to_string())].into();

    for (k, v) in metric.metadata().iter() {
        metadata.insert(k.to_string(), v.to_string());
    }

    let sampler =
        crate::agent::samplers::attribute_sampler(metric.module(), sampler_mods).to_string();
    metadata.insert("sampler".to_string(), sampler.clone());

    (metadata, sampler)
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

        // KEEP IN SYNC with create_v3 — see the doc comment on
        // `metric_metadata` (the shared prefix) and on `create_v3` itself
        // (the per-kind walk/naming/membership rules below, which are NOT
        // shared and are duplicated independently in each function).
        let (metadata, _sampler) = metric_metadata(metric, &sampler_mods);
        let mut metadata: HashMap<String, String> = metadata.into_iter().collect();

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
/// every group — full metadata included — to know each member's current
/// state; that assembly always runs, cache hit or miss. What the cache
/// skips is [`GroupSchema::hash`]: a full canonical msgpack serialization of
/// the schema followed by an FNV-1a-128 fold, the one per-tick cost that
/// scales with schema size rather than with "did anything change". When the
/// freshly assembled schema is `==` the previous tick's cached schema
/// (`GroupSchema` derives `PartialEq`, comparing every member's name AND
/// metadata, not just names — see the note on why below), it's discarded in
/// favor of the cached one and the cached hash is reused verbatim; only a
/// genuine change pays for a fresh hash.
///
/// # Honest cost accounting (this is NOT a net allocation win yet)
///
/// Despite skipping the hash fold, V3 currently allocates MORE per tick
/// than V2 on the cache-HIT path — measured 1.2–3.4× V2's allocations, with
/// the emit-time `cached.schema.clone()` alone accounting for 54% of
/// allocations at 2k members. The spec's target ("a stable group allocates
/// ~nothing on a hit") is deferred, not delivered by this cache; reaching
/// it needs two follow-up changes this commit does not make: (a) folding
/// member identity + metadata into a rolling hash so the metadata
/// `BTreeMap`/`MetricDesc` assembly itself moves behind the cache-miss
/// branch instead of running unconditionally every tick, and (b) removing
/// the emit-time schema clone, which needs an upstream `metriken-exposition`
/// change (an `Arc`/`Cow`-backed schema, or serializing `GroupSnapshot` by
/// reference instead of by value) since `GroupSnapshot::schema` is an owned
/// `Option<GroupSchema>` today. Both are named follow-up work for the
/// Stage-3d measurement plan, not implied by anything below.
///
/// # Why full-schema equality, not just member names
///
/// An earlier version of this cache compared member NAME lists only. That's
/// unsound: metriken metadata mutates in place at a stable index
/// (`insert_metadata`/`set_metadata`, e.g. a task's `comm` or a cgroup's
/// `name`), and the kernel recycles PIDs and cgroup ids — so a slot's
/// metadata can change while its `"{metric_id}x{idx}"` name stays byte-for-
/// byte identical. A names-only cache would call that a hit, keep serving
/// the OLD occupant's metadata under an UNCHANGED `schema_hash`, and a
/// receiver caching parsed schemas by `(name, schema_hash)` would bind new
/// values to dead labels indefinitely. Comparing the whole `GroupSchema`
/// (names AND metadata) closes that hole at the cost of a full struct
/// comparison instead of a name-list comparison — cheap next to the hash
/// fold it's still avoiding.
///
/// # No eviction
///
/// Entries are never removed. Acceptable because the key space is bounded
/// by the number of acquisition groups a build can ever produce (samplers'
/// declared groups plus one default group per sampler plus `external/main`)
/// — a small, essentially fixed set, not something that grows with runtime
/// cardinality (CPUs, tasks, cgroups... those are members WITHIN a group's
/// schema, not distinct group keys).
pub(crate) struct SkeletonCache {
    entries: HashMap<String, GroupSkeleton>,
    rebuilds: u64,
}

struct GroupSkeleton {
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
    /// since this cache was created.
    #[cfg(test)]
    pub(crate) fn rebuilds(&self) -> u64 {
        self.rebuilds
    }
}

/// Per-group accumulation while walking the metriken registry: the group's
/// acquisition window (read once, at first touch — see `create_v3`) plus,
/// per kind, the (descriptor, value) pairs in schema order.
#[derive(Default)]
struct GroupBuilder {
    window: Option<Window>,
    counters: Vec<(MetricDesc, Option<u64>)>,
    gauges: Vec<(MetricDesc, Option<i64>)>,
    histograms: Vec<(MetricDesc, Option<histogram::Histogram>)>,
}

/// The `(sampler, name) -> AcquisitionGroup` registry, built once. Sound to
/// cache for the process lifetime: `ACQUISITION_GROUPS` is a `linkme`
/// distributed slice, populated at link time before `main` runs and never
/// mutated afterward — every entry that will ever exist already does by the
/// time this first initializes.
fn group_registry() -> &'static HashMap<String, &'static AcquisitionGroup> {
    static REGISTRY: OnceLock<HashMap<String, &'static AcquisitionGroup>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry: HashMap<String, &'static AcquisitionGroup> = HashMap::new();
        for group in crate::agent::samplers::ACQUISITION_GROUPS {
            registry.insert(format!("{}/{}", group.sampler, group.name), group);
        }
        registry
    })
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
/// rather than letting it pass silently in release. Note what that means
/// operationally: on a debug build this panics the scrape task for that one
/// tick (`/metrics/binary`'s handler task, not the sampler tasks) — the
/// `tokio::sync::Mutex` guarding `SnapshotBuilder` does not poison on a
/// panicked holder, so the next scrape simply retries and calls `refresh()`
/// again rather than the agent wedging.
///
/// The mirror case — an `AcquisitionGroup` IS registered but no metric ever
/// names it via `acq_group` — is not an error at all (e.g. a group declared
/// ahead of the sampler code that will use it). `create_v3` only creates a
/// `GroupBuilder` when some metric actually routes to a group, so a
/// registered-but-unused group is silently absent from the emitted
/// snapshot's `groups` list entirely, rather than appearing as an empty
/// `GroupSnapshot`. Pinned by
/// `registered_group_with_no_routed_metrics_is_absent_from_the_snapshot`.
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
    let group_registry = group_registry();

    let mut groups: HashMap<String, GroupBuilder> = HashMap::new();

    for (metric_id, metric) in metriken::metrics().iter().enumerate() {
        let Some(value) = metric.value() else {
            continue;
        };

        let name = metric.name();

        if name.starts_with("log_") {
            continue;
        }

        // KEEP IN SYNC with create — see the doc comment on
        // `metric_metadata` (the shared prefix) and below (the per-kind
        // walk/naming/membership rules, which are NOT shared and are
        // duplicated independently in each function).
        let (metadata, sampler) = metric_metadata(metric, &sampler_mods);

        // Route: a declared `acq_group` wins only if it actually resolves
        // against the registry; otherwise fall back to the sampler's
        // default group (and flag the mismatch in debug builds — see the
        // function-level doc comment).
        let mut declared = false;
        let group_key = match metric.metadata().get("acq_group") {
            Some(acq_group) => {
                let key = format!("{sampler}/{acq_group}");
                if group_registry.contains_key(&key) {
                    declared = true;
                    key
                } else {
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
            None => format!("{sampler}/main"),
        };

        let group = match groups.entry(group_key) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                // First touch for this group THIS TICK: read its window
                // now, before any of its members' values accumulate below
                // — not once per member (redundant seqlock loads, and
                // walk-order-dependent) and not deferred to emit time
                // (unsafe: the walk over the FULL registry, across every
                // group, can take long enough that a concurrent sampler
                // tick completes a whole new acquire()/finish() cycle in
                // the meantime, which would pair window(N+1) with the
                // values(N) already read here — a confident, wrong claim
                // that this data is newer than it actually is). Read
                // before values, mirroring timing.rs's stamp-last rule
                // from the read side: a stale window paired with
                // fresh-enough values only under-claims freshness, which
                // is the safe direction — same "can only lag, never lead"
                // guarantee `AcquisitionGuard` gives writers, applied here
                // to the reader.
                let window = group_registry.get(e.key()).and_then(|ag| ag.window());
                e.insert(GroupBuilder {
                    window,
                    ..Default::default()
                })
            }
        };

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
                // `config()` doesn't require a loaded value, so this is
                // always available regardless of what `load()` returns
                // below.
                let mut entry_metadata = metadata;
                entry_metadata.insert(
                    "grouping_power".to_string(),
                    h.config().grouping_power().to_string(),
                );
                entry_metadata.insert(
                    "max_value_power".to_string(),
                    h.config().max_value_power().to_string(),
                );

                let hv = h.load();
                let desc = MetricDesc {
                    name: entry_name,
                    metadata: entry_metadata,
                };
                if declared {
                    // Registration membership: this metric IS the member,
                    // full stop — `None` means "registered but no reading
                    // yet" (e.g. before its BPF map attaches), not "not a
                    // member". Omitting it here would make membership
                    // value-derived on the declared path, churning the
                    // schema hash on exactly the transient event (a
                    // histogram that hasn't loaded yet) the design commits
                    // to NOT treating as a membership change.
                    group.histograms.push((desc, hv));
                } else if let Some(hv) = hv {
                    // Default path: unchanged V2-style membership-by-
                    // presence — an unloaded histogram isn't a member at
                    // all.
                    group.histograms.push((desc, Some(hv)));
                }
            }
            _ => {}
        }
    }

    // External metrics: one windowless group, own naming scheme (they are
    // not metriken registry entries, so there is no metric_id to key on).
    if !external_metrics.is_empty() {
        let group = groups.entry("external/main".to_string()).or_default();

        // `get_active()`'s Vec order follows the store's `HashMap<MetricKey,
        // _>` iteration order, which is not a stable contract tick-to-tick
        // (hashbrown makes no ordering guarantee, independent of any TTL
        // eviction/insertion churn). Sort by the identity-derived name
        // assigned below so the group's member order — and therefore the
        // skeleton cache's name-list comparison — is deterministic
        // regardless of store iteration order; otherwise the cache would
        // spuriously "miss" (and rebuild/rehash) on every tick whenever the
        // store happened to reorder with no real membership change.
        let mut entries: Vec<(String, ExternalMetric)> = external_metrics
            .into_iter()
            .map(|metric| {
                // A positional name (e.g. `external{i}`) is not a stable
                // identity: both membership and Vec order can churn
                // tick-to-tick, so the same metric would silently reattach
                // under a different name — a name-keyed consumer would read
                // that as a valid continuous series when it isn't one. Name
                // alone isn't sufficient either, since two external metrics
                // may share a name with different labels. Derive the entry
                // name from (name, labels) via the same hash the store's own
                // `MetricKey` uses for identity (`hash_labels`: sorted-key
                // `DefaultHasher`, not process-randomized — verified: three
                // separate process runs over the same label set produced the
                // identical hash), so a metric's values reattach under the
                // same name every tick regardless of where it lands in the
                // store.
                let labels_hash = MetricKey::new(&metric.name, &metric.labels).labels_hash;
                let entry_name = format!("external/{}#{labels_hash:016x}", metric.name);
                (entry_name, metric)
            })
            .collect();
        entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        for (entry_name, metric) in entries {
            let mut metadata: BTreeMap<String, String> = [
                ("metric".to_string(), metric.name.clone()),
                ("source".to_string(), "external".to_string()),
            ]
            .into();

            for (k, v) in metric.labels {
                metadata.insert(k, v);
            }

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
        let window = group.window;
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

        // Full-schema equality (names AND metadata), not just a name-list
        // comparison — see the `SkeletonCache` doc comment for why a
        // names-only cache is unsound (metadata mutates in place at a
        // stable index when the kernel recycles a pid/cgroup id).
        let (schema, hash) = match cache.entries.get(&group_name) {
            Some(cached) if cached.schema == schema => (cached.schema.clone(), cached.hash),
            _ => {
                let hash = schema.hash();
                cache.entries.insert(
                    group_name.clone(),
                    GroupSkeleton {
                        schema: schema.clone(),
                        hash,
                    },
                );
                cache.rebuilds += 1;
                (schema, hash)
            }
        };

        group_snapshots.push(GroupSnapshot {
            name: group_name,
            schema_hash: hash,
            schema: Some(schema),
            window,
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

    // Dedicated group + metric, touched by no other test. `rustc test`
    // threads tests in parallel within one process, sharing the whole
    // metriken registry: a test that instead asserted on the shared
    // `unattributed/main`/`external/main` buckets (several other tests
    // write into those) would be coupled to what else happens to be
    // running concurrently, not to anything this test itself does.
    static V3_STABILITY_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "stability_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_STABILITY_GROUP_ENTRY: &'static AcquisitionGroup = &V3_STABILITY_GROUP;

    #[metric(
        name = "snapshot_v3_stability_probe",
        metadata = { acq_group = "stability_probe" }
    )]
    static V3_STABILITY_PROBE: metriken::Counter = metriken::Counter::new();

    #[test]
    fn skeleton_cache_is_stable_across_ticks() {
        V3_STABILITY_PROBE.increment();
        let guard = V3_STABILITY_GROUP.acquire();
        guard.finish();

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
        // Safe to assert unconditionally: `cache` was just created, so 0 is
        // a known baseline (not a comparison across two ticks that could be
        // perturbed by a concurrently running test's unrelated group).
        assert!(
            cache.rebuilds() > 0,
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

        // NOT asserting `cache.rebuilds()` unchanged between the two ticks
        // here: it's a global counter across every group this tick
        // produces (the full metriken registry, not just this test's own
        // group), so a concurrently running test mutating ITS OWN group's
        // membership between these two calls would legitimately bump it —
        // that's real concurrent-test interference, not a property of the
        // cache. What's actually pinned, scoped to the one group this test
        // owns exclusively, is that its schema hash doesn't change when
        // nothing about it does.
        let hash1 = s1
            .groups
            .iter()
            .find(|g| g.name == "unattributed/stability_probe")
            .expect("group present in tick 1")
            .schema_hash;
        let hash2 = s2
            .groups
            .iter()
            .find(|g| g.name == "unattributed/stability_probe")
            .expect("group present in tick 2")
            .schema_hash;
        assert_eq!(
            hash1, hash2,
            "schema hash is stable across ticks for an unchanged group"
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

    // C1 regression fixture: a declared CounterGroup whose metadata gets
    // mutated at a stable index between ticks, simulating what happens when
    // the kernel recycles a pid/cgroup id — the value slot stays written
    // (metriken's backing array never moves an occupied slot), but the
    // metadata attached to that index changes to describe the new
    // occupant.
    static V3_RECYCLE_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "recycle_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_RECYCLE_GROUP_ENTRY: &'static AcquisitionGroup = &V3_RECYCLE_GROUP;

    #[metric(
        name = "snapshot_v3_recycle_probe",
        metadata = { acq_group = "recycle_probe" }
    )]
    static V3_RECYCLE_COUNTERS: metriken::CounterGroup = metriken::CounterGroup::new(1);

    #[test]
    fn declared_group_schema_reflects_metadata_mutated_at_a_stable_index() {
        // C1 regression: the skeleton cache used to key on member NAMES
        // only (`"{metric_id}x{idx}"`, which does NOT change across a
        // recycle — the index is the same, only what's attached to it
        // changed). A names-only cache would call the second tick below a
        // hit and keep serving the FIRST tick's metadata under an
        // unchanged `schema_hash`. Comparing full schema equality (see the
        // `SkeletonCache` doc comment) closes that hole: the metadata
        // change must be visible in the emitted `MetricDesc` AND must
        // change the schema hash, or a receiver caching parsed schemas by
        // `(name, schema_hash)` would bind the new values to dead labels
        // indefinitely.
        V3_RECYCLE_COUNTERS.set(0, 1);
        V3_RECYCLE_COUNTERS.set_metadata(0, [("comm".to_string(), "old_task".to_string())].into());

        let guard = V3_RECYCLE_GROUP.acquire();
        guard.finish();

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
        let group1 = s1
            .groups
            .iter()
            .find(|g| g.name == "unattributed/recycle_probe")
            .expect("declared group present in tick 1");
        let schema1 = group1.schema.as_ref().expect("schema present");
        let desc1 = schema1
            .counters
            .iter()
            .find(|d| {
                d.metadata.get("metric").map(String::as_str) == Some("snapshot_v3_recycle_probe")
            })
            .expect("member present in tick 1");
        assert_eq!(
            desc1.metadata.get("comm").map(String::as_str),
            Some("old_task")
        );

        // Recycle: same index, same value, DIFFERENT occupant's metadata.
        V3_RECYCLE_COUNTERS.set(0, 1);
        V3_RECYCLE_COUNTERS.set_metadata(0, [("comm".to_string(), "new_task".to_string())].into());

        let guard = V3_RECYCLE_GROUP.acquire();
        guard.finish();

        let snap2 = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s2) = snap2 else {
            panic!("expected V3")
        };
        let group2 = s2
            .groups
            .iter()
            .find(|g| g.name == "unattributed/recycle_probe")
            .expect("declared group present in tick 2");
        let schema2 = group2.schema.as_ref().expect("schema present");
        let desc2 = schema2
            .counters
            .iter()
            .find(|d| {
                d.metadata.get("metric").map(String::as_str) == Some("snapshot_v3_recycle_probe")
            })
            .expect("member present in tick 2");

        assert_eq!(
            desc2.metadata.get("comm").map(String::as_str),
            Some("new_task"),
            "the new occupant's metadata is served, not the stale one's"
        );
        assert_ne!(
            group1.schema_hash, group2.schema_hash,
            "a metadata-only change at a stable index must change the schema hash \
             (the cache must not reuse the stale schema)"
        );
    }

    // Mirror of the unmatched-`acq_group` debug_assert case: here the
    // registry entry is real and correctly named, but no metric ever
    // routes to it via `acq_group`.
    static V3_UNUSED_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "never_routed");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_UNUSED_GROUP_ENTRY: &'static AcquisitionGroup = &V3_UNUSED_GROUP;

    #[test]
    fn registered_group_with_no_routed_metrics_is_absent_from_the_snapshot() {
        // Pinning the behavior actually implemented: `create_v3` only
        // creates a `GroupBuilder` when a metric routes to a group, so a
        // registered-but-unused group does not appear in the snapshot at
        // all — no empty `GroupSnapshot` is synthesized for it. If this
        // ever needs to change (e.g. so a discovery UI can see every
        // declared group, even empty ones), this test is the marker to
        // update alongside the function doc comment on `create_v3`.
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
        assert!(
            !s.groups
                .iter()
                .any(|g| g.name == "unattributed/never_routed"),
            "a registered group nothing routes to should not appear in the snapshot"
        );
    }

    // Same shape (2 entries), same write pattern (only index 0 touched) —
    // one declared, one not. `CounterGroup`'s backing array zero-initializes
    // eagerly across ALL entries the moment any one index is written, so
    // index 1 reads `Some(0)` in both fixtures either way: the metriken
    // group API gives no way to independently distinguish "a member that is
    // genuinely present and reads zero" from "a phantom slot that merely
    // exists because the backing array got initialized" once that's
    // happened. What this test honestly pins is the observable CONTRACT
    // difference the V3 design layers on top of that identical reading: a
    // declared group reports index 1 as `Some(0)` (registration membership,
    // no sentinel skip) while an otherwise-identical undeclared group
    // suppresses it entirely (V2's transitional value-sentinel skip,
    // default groups only).
    static V3_DECLARED_COUNTER_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "counter_group_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_DECLARED_COUNTER_GROUP_ENTRY: &'static AcquisitionGroup = &V3_DECLARED_COUNTER_GROUP;

    #[metric(
        name = "snapshot_v3_declared_counter_group",
        metadata = { acq_group = "counter_group_probe" }
    )]
    static V3_DECLARED_COUNTERS: metriken::CounterGroup = metriken::CounterGroup::new(2);

    #[metric(name = "snapshot_v3_default_counter_group")]
    static V3_DEFAULT_COUNTERS: metriken::CounterGroup = metriken::CounterGroup::new(2);

    #[test]
    fn declared_group_includes_zero_counter_entries_default_group_skips_them() {
        V3_DECLARED_COUNTERS.increment(0);
        V3_DEFAULT_COUNTERS.increment(0);

        let guard = V3_DECLARED_COUNTER_GROUP.acquire();
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

        let declared = s
            .groups
            .iter()
            .find(|g| g.name == "unattributed/counter_group_probe")
            .expect("declared counter group present");
        let declared_schema = declared.schema.as_ref().expect("schema present");
        let find_declared = |id: &str| {
            declared_schema.counters.iter().position(|d| {
                d.metadata.get("metric").map(String::as_str)
                    == Some("snapshot_v3_declared_counter_group")
                    && d.metadata.get("id").map(String::as_str) == Some(id)
            })
        };
        let idx0 = find_declared("0").expect("index 0 present in declared group");
        let idx1 = find_declared("1").expect("index 1 present in declared group (no zero-skip)");
        assert_eq!(
            declared.counters[idx0],
            Some(1),
            "index 0 nonzero as written"
        );
        assert_eq!(
            declared.counters[idx1],
            Some(0),
            "index 1 included as an honest Some(0), not suppressed"
        );

        let default_group = s
            .groups
            .iter()
            .find(|g| g.name == "unattributed/main")
            .expect("default group present");
        let default_schema = default_group.schema.as_ref().expect("schema present");
        let find_default = |id: &str| {
            default_schema.counters.iter().position(|d| {
                d.metadata.get("metric").map(String::as_str)
                    == Some("snapshot_v3_default_counter_group")
                    && d.metadata.get("id").map(String::as_str) == Some(id)
            })
        };
        assert!(
            find_default("0").is_some(),
            "default group keeps the nonzero entry"
        );
        assert!(
            find_default("1").is_none(),
            "default group's transitional sentinel skip drops the zero entry"
        );
    }

    #[test]
    fn external_metrics_get_stable_identity_names_and_deterministic_schema() {
        let labels_a: HashMap<String, String> = [("env".to_string(), "prod".to_string())].into();
        let labels_b: HashMap<String, String> = [("env".to_string(), "dev".to_string())].into();

        let make = |labels: HashMap<String, String>, value: u64| ExternalMetric {
            name: "ext_shared_name".into(),
            labels,
            value: ExternalMetricValue::Counter(value),
            last_updated: std::time::Instant::now(),
            window: None,
        };

        let mut cache = SkeletonCache::new();
        let snap1 = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![make(labels_a.clone(), 1), make(labels_b.clone(), 2)],
            &mut cache,
        );
        let Snapshot::V3(s1) = snap1 else {
            panic!("expected V3")
        };
        let group1 = s1
            .groups
            .iter()
            .find(|g| g.name == "external/main")
            .expect("external group present");
        let schema1 = group1.schema.as_ref().expect("schema present");
        let names1: Vec<&str> = schema1.counters.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names1.len(),
            2,
            "two distinct entries for same-name, different-label metrics"
        );
        assert_ne!(
            names1[0], names1[1],
            "distinct entry names for distinct label sets"
        );

        // Second tick: same two metrics, but handed to create_v3 in the
        // OPPOSITE order — standing in for the store's HashMap iteration
        // reordering tick-to-tick with no real membership change. The
        // group's member order, schema, and hash must all be unaffected.
        // (Not asserting the global `cache.rebuilds()` counter unchanged
        // here — it accumulates across every group this tick produces, not
        // just `external/main`, so a concurrently running test's own
        // group would legitimately bump it. `external/main`'s schema hash
        // below is what's actually scoped to this test: nothing else in
        // the suite ever passes non-empty external metrics, so this group
        // is exclusively this test's to perturb or not.)
        let snap2 = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![make(labels_b.clone(), 2), make(labels_a.clone(), 1)],
            &mut cache,
        );
        let Snapshot::V3(s2) = snap2 else {
            panic!("expected V3")
        };

        let group2 = s2
            .groups
            .iter()
            .find(|g| g.name == "external/main")
            .expect("external group present");
        assert_eq!(
            group1.schema_hash, group2.schema_hash,
            "schema hash stable despite input-order reordering"
        );

        let schema2 = group2.schema.as_ref().expect("schema present");
        let names2: Vec<&str> = schema2.counters.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names1, names2,
            "member order is deterministic (sorted), not input-order-dependent"
        );
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
        let Snapshot::V3(s) = snap else {
            panic!("snapshot_format = \"v3\" selects the V3 builder")
        };
        for g in &s.groups {
            assert_eq!(
                g.validate(),
                Ok(()),
                "group `{}` failed to validate",
                g.name
            );
        }
    }
}
