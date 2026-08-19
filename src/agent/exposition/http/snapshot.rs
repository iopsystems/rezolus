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

        // V2 has no group concept — `acq_group` only means something to
        // `create_v3`'s routing. Strip it unconditionally (mirroring
        // `create_v3`'s declared-group strip) so a metric migrating to a
        // declared acquisition group does not grow a new label in V2
        // output; default mode stays byte-stable vs main.
        metadata.remove("acq_group");

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
            let key = format!("{}/{}", group.sampler, group.name);
            let prev = registry.insert(key.clone(), group);
            debug_assert!(
                prev.is_none(),
                "duplicate acquisition-group registry key `{key}` — every registered group's \
                 (sampler, name) pair must stay globally unique, including on non-Linux builds, \
                 where `samplers::bpf_sampler_name` collapses every BPF sampler's \
                 `attribute_sampler` resolution to the shared \"unattributed\" bucket (stats.rs \
                 is `include!`d there for metric-identity continuity, with no matching \
                 SamplerEntry). Qualify the group's `name` with its sampler, e.g. \
                 `<sampler>_<shortname>` — see the naming rule documented on \
                 `samplers::ACQUISITION_GROUPS`.",
            );
        }
        registry
    })
}

/// Reconcile a declared group's window across one `create_v3` walk.
///
/// `first` is read at the group's first touch, before any of its members'
/// values are read (see the `Entry::Vacant` arm in `create_v3`). For a
/// group with few members or a fast walk that's also the window this
/// function emits with. But the walk over the FULL registry — every
/// group, every member — has measured as long as several milliseconds
/// (mean 1.85ms, max 5.7ms observed span), which is long enough for an
/// async sampler write to complete a whole new `acquire()`/`finish()`
/// cycle in the middle of it. `latest` is a second read of the same
/// group's window, taken at emit time after all of that group's values
/// have been read. If `first` and `latest` differ, some of the values
/// just read may actually be newer than `first` claims — the window can
/// only ever LAG the true acquisition time (`AcquisitionGuard` stamps
/// last), never lead it, so `first` alone would UNDER-claim what this
/// walk covers. The honest fix is the union: `first.begin_ns` is still
/// correct (nothing read during the walk is older than that), so keep it,
/// and extend the end to `latest.end_ns` to honestly bracket every value
/// this walk actually read, rather than silently narrowing the claimed
/// window to only the pre-mid-walk-stamp subset.
///
/// `first: None, latest: Some(_)` means the group was stamped for the
/// first time during this very walk (unstamped at first touch, stamped by
/// the time of emit) — there's nothing to union with, so `latest` alone is
/// the walk's window. `first: Some(_), latest: None` is not expected in
/// practice (a group's seqlock only reads `None` before its first-ever
/// stamp, and stamps never revert to unstamped outside the seqlock's
/// documented u64-wraps-to-exactly-0 edge case) — `first` is kept rather
/// than discarding a real reading for a transient artifact.
fn resolve_walk_window(first: Option<Window>, latest: Option<Window>) -> Option<Window> {
    match (first, latest) {
        (Some(f), Some(l)) if f == l => Some(f),
        (Some(f), Some(l)) => Some(Window::new(f.begin_ns, l.end_ns)),
        (None, Some(l)) => Some(l),
        (Some(f), None) => Some(f),
        (None, None) => None,
    }
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
/// `0..group.entries()` is a member, full stop — UNLESS the group has a
/// member-population bound set ([`AcquisitionGroup::set_member_bound`]), in
/// which case membership is `0..bound.min(group.entries())`. Registration
/// membership for a per-CPU group like a `CpuCounters`-backed one IS its
/// possible-CPU population; the backing array's `entries()` is an
/// implementation ceiling sized for the worst case (`MAX_CPUS`; see
/// docs/principles.md principle 6 and
/// docs/superpowers/plans/2026-08-18-stage3c-wave1-sampler-migration.md),
/// not a claim that every one of those slots is a real member on this host.
/// Counter/gauge group entries within the bound send `Some(value)`
/// including an honest zero — no sentinel skip — and a group whose backing
/// store was never written at all reports `None` for every member
/// ("registered but no reading yet"), never fabricating a value or
/// silently dropping the member. A scalar (non-group) declared metric
/// behaves differently: a `LazyCounter`/`LazyGauge` reports no value at all
/// (`metric.value()` is `None`) until its first `set()`/`increment()`, so
/// it is simply ABSENT from the group's schema — not present with a `None`
/// value — until then; that first appearance is one schema rebuild
/// (`SkeletonCache` absorbs it like any other schema change) rather than an
/// ongoing cost. The group's window comes from the
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
        let (mut metadata, sampler) = metric_metadata(metric, &sampler_mods);

        // Route: a declared `acq_group` wins only if it actually resolves
        // against the registry; otherwise fall back to the sampler's
        // default group (and flag the mismatch in debug builds — see the
        // function-level doc comment).
        let mut declared = false;
        // The member-population bound for a declared, group-typed metric
        // (`CounterGroup`/`GaugeGroup`): `Some(n)` walks `0..n` instead of
        // the full backing-array `entries()`. Resolved alongside routing,
        // before `group_key` is moved into `groups.entry` below.
        let mut member_bound: Option<usize> = None;
        let group_key = match metric.metadata().get("acq_group") {
            Some(acq_group) => {
                let key = format!("{sampler}/{acq_group}");
                if let Some(ag) = group_registry.get(&key) {
                    declared = true;
                    member_bound = ag.member_bound();
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
                // walk-order-dependent) and not deferred solely to emit time (see resolve_walk_window)
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

        // Strip unconditionally, not just when `declared`. On the declared
        // path it is redundant with the group's own name (`GroupSnapshot.name`,
        // `"{sampler}/{acq_group}"`) — left in place it would be one more
        // copy of the same key/value pair repeated in every member's
        // `MetricDesc.metadata`, thousands of identical wasted copies for a
        // large declared group (tasks/cgroups/CPUs). On the unmatched-registry
        // fallback path (the `debug_assert!` arm above — a typo'd or
        // renamed group), the metric still carries its stale `acq_group`
        // value even though it just got routed to the DEFAULT group instead;
        // leaving it in would leak that value as a phantom label on release
        // builds, where the `debug_assert!` compiles away and this fallback
        // runs silently instead of panicking. A metric with no `acq_group`
        // tag at all has nothing to remove, so this is a no-op there.
        metadata.remove("acq_group");

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
                // Registration membership for a per-CPU (or similar) group
                // IS the group's real member population — `possible_cpus()`
                // for a `CpuCounters`-backed group — not the backing
                // array's `entries()` capacity, which is a fixed
                // implementation ceiling (`MAX_CPUS`; see docs/principles.md
                // principle 6, "over-allocates on small machines") sized
                // for the worst case, not this host. Walking the full
                // capacity on every declared group would put an ~18-CPU
                // host's tick at ~19× the entries it actually populated;
                // walk the bound instead when one is set (clamped to
                // `entries()` in case a stale/misconfigured bound somehow
                // exceeds the backing array).
                let bound = member_bound.map_or(g.entries(), |b| b.min(g.entries()));
                for idx in 0..bound {
                    let v = g.counter_value(idx);

                    // Transitional V2-style sentinel skip — default groups only. See doc comment.
                    if !declared {
                        let Some(v) = v else { continue };
                        if v == 0 {
                            continue;
                        }
                    }

                    // `counter_value(idx)` above and `load_metadata(idx)`
                    // here are two SEPARATE reads, not one atomic pair —
                    // unlike `AcquisitionGroup`'s window (a seqlock), a
                    // group entry's value and its metadata have no shared
                    // lock. A slot recycled by a concurrent writer between
                    // these two reads (e.g. a pid/cgroup id reused mid-tick)
                    // can pair the NEW occupant's value with the OLD
                    // occupant's labels, or vice versa, for that one tick.
                    // This matches V2's `create()`, which has the identical
                    // two-step read here — not a regression introduced by
                    // V3. Measured under a deliberate concurrent-recycle
                    // hammer: ~2-3% of ticks torn; in production today it's
                    // effectively zero, because sampler writes complete
                    // synchronously inside `refresh()` rather than racing
                    // the snapshot builder from another task. Migration
                    // note: a sampler that calls `insert_metadata` more
                    // than once per slot per refresh (cpu usage does 4)
                    // should move to a single atomic metadata update
                    // (`set_metadata`, one call) when it migrates to a
                    // declared group, to close this window rather than
                    // just narrow it.
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
                // Same member-population bound as the `CounterGroup` arm
                // above — see its comment.
                let bound = member_bound.map_or(g.entries(), |b| b.min(g.entries()));
                for idx in 0..bound {
                    let v = g.gauge_value(idx);

                    // Transitional V2-style sentinel skip — default groups
                    // only. See doc comment. Unlike CounterGroup's `== 0`
                    // (still live below: 0 is a legitimate initialized-
                    // but-untouched counter value, indistinguishable from
                    // an explicit 0), there is no `== i64::MIN` check here:
                    // `GaugeGroup::gauge_value` already maps its internal
                    // never-set sentinel to `None` before this ever sees
                    // it (metriken owns that mapping), so `Some(i64::MIN)`
                    // cannot occur — an explicit re-check here would be
                    // dead code.
                    if !declared && v.is_none() {
                        continue;
                    }

                    // Separate value/metadata reads, same torn-recycle
                    // caveat as the CounterGroup arm above.
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
        // skeleton cache's schema comparison — is deterministic
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
        // A metric routes to (and so creates) a `GroupBuilder` before its
        // `Value` is matched below, so a metric whose value kind isn't one
        // `create_v3` knows how to expose (falls into the `_ => {}` arm —
        // e.g. a `HistogramGroup`-typed metric, a gap V2's `create` shares)
        // can leave a group with nothing ever pushed into it. An
        // empty-schema `GroupSnapshot` carries no information a receiver
        // can use and would otherwise be hashed and transmitted every
        // tick for nothing — and it contradicts this function's own doc
        // comment, which says a group nothing routes to is absent. Skip it
        // entirely rather than emit a zero-member group.
        if group.counters.is_empty() && group.gauges.is_empty() && group.histograms.is_empty() {
            continue;
        }

        // Re-read the window here (after this group's values, above) and
        // reconcile with the first-touch read via `resolve_walk_window` —
        // see its doc comment for why a second read is necessary.
        let latest_window = group_registry.get(&group_name).and_then(|ag| ag.window());
        let window = resolve_walk_window(group.window, latest_window);
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

    // --- resolve_walk_window ---------------------------------------------

    #[test]
    fn resolve_walk_window_unchanged_returns_the_same_window() {
        let w = Window::new(1_000, 2_000);
        assert_eq!(resolve_walk_window(Some(w), Some(w)), Some(w));
    }

    #[test]
    fn resolve_walk_window_changed_returns_the_union() {
        let first = Window::new(1_000, 2_000);
        let latest = Window::new(3_000, 4_000);
        assert_eq!(
            resolve_walk_window(Some(first), Some(latest)),
            Some(Window::new(1_000, 4_000)),
            "keeps first's begin, extends to latest's end"
        );
    }

    #[test]
    fn resolve_walk_window_both_none_is_none() {
        assert_eq!(resolve_walk_window(None, None), None);
    }

    #[test]
    fn resolve_walk_window_stamped_for_the_first_time_mid_walk_uses_latest() {
        let latest = Window::new(5_000, 6_000);
        assert_eq!(
            resolve_walk_window(None, Some(latest)),
            Some(latest),
            "nothing to union with a never-yet-stamped first read"
        );
    }

    #[test]
    fn resolve_walk_window_transient_unstamped_latest_keeps_first() {
        // Not expected in practice (see the doc comment: a group's seqlock
        // only reads None before its first-ever stamp), but a real earlier
        // reading must never be discarded for an artifact read.
        let first = Window::new(1_000, 2_000);
        assert_eq!(resolve_walk_window(Some(first), None), Some(first));
    }

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

    #[test]
    fn v2_snapshot_strips_acq_group_from_a_tagged_metric() {
        // V3_PROBE_COUNTER (declared below) carries `acq_group = "probe"` in
        // its static metadata. V2 has no group concept — that key must not
        // leak into V2 output as a new label (it would otherwise, since
        // `create` copies through every static metadata key unfiltered).
        V3_PROBE_COUNTER.increment();
        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![]);
        let Snapshot::V2(s) = snap else {
            panic!("expected V2")
        };
        let c = s
            .counters
            .iter()
            .find(|c| {
                c.metadata.get("metric").map(String::as_str) == Some("snapshot_v3_probe_counter")
            })
            .expect("tagged probe counter present in V2 output");
        assert!(
            !c.metadata.contains_key("acq_group"),
            "acq_group must be stripped from V2 output, not leaked as a new label"
        );
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
        assert!(
            !schema.counters[idx].metadata.contains_key("acq_group"),
            "acq_group is redundant with the declared group's own name and must be stripped \
             from each member's metadata, not duplicated across every one"
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

    // A `HistogramGroup`-typed metric: its `Value::HistogramGroup` isn't a
    // kind `create_v3` (or V2's `create`) knows how to expose, so it falls
    // into the match's `_ => {}` arm — the group it routes to gets a
    // `GroupBuilder` (created for the window read at first touch) but
    // never has anything pushed into it. Given its own dedicated declared
    // group, it's the only thing that would ever route there.
    static V3_UNHANDLED_VALUE_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "unhandled_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_UNHANDLED_VALUE_GROUP_ENTRY: &'static AcquisitionGroup = &V3_UNHANDLED_VALUE_GROUP;

    #[metric(
        name = "snapshot_v3_unhandled_probe",
        metadata = { acq_group = "unhandled_probe" }
    )]
    static V3_UNHANDLED_HISTOGRAM_GROUP: metriken::HistogramGroup =
        metriken::HistogramGroup::new(2, 7, 32);

    #[test]
    fn group_with_only_unhandled_value_metrics_is_not_emitted() {
        // Before this fix, routing alone (which happens before the Value
        // match) was enough to create the GroupBuilder, so a group whose
        // only routed metric is an unhandled Value kind shipped as an
        // empty-schema GroupSnapshot every tick — hashed and transmitted
        // for nothing, and silently contradicting the "nothing routed here
        // means absent" contract pinned by the test above.
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
                .any(|g| g.name == "unattributed/unhandled_probe"),
            "a group whose only routed metric is an unhandled Value kind must not be emitted"
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

    // Dedicated group + metric: 8 backing entries, member_bound set to 3.
    // Distinct from `V3_DECLARED_COUNTER_GROUP` above (entries=2, no bound
    // — that test is this one's "unbounded behaves as today" control).
    static V3_BOUNDED_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "bounded_counter_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_BOUNDED_GROUP_ENTRY: &'static AcquisitionGroup = &V3_BOUNDED_GROUP;

    #[metric(
        name = "snapshot_v3_bounded_counters",
        metadata = { acq_group = "bounded_counter_probe" }
    )]
    static V3_BOUNDED_COUNTERS: metriken::CounterGroup = metriken::CounterGroup::new(8);

    #[test]
    fn declared_group_member_bound_limits_population_below_entries() {
        // Write every one of the 8 backing slots so an unbounded walk would
        // see all 8 (this is not a zero-skip scenario — see the honest-zero
        // test above for that).
        for idx in 0..8 {
            V3_BOUNDED_COUNTERS.add(idx, 10 + idx as u64);
        }
        V3_BOUNDED_GROUP.set_member_bound(3);
        let guard = V3_BOUNDED_GROUP.acquire();
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

        let group = s
            .groups
            .iter()
            .find(|g| g.name == "unattributed/bounded_counter_probe")
            .expect("bounded declared group present");
        assert_eq!(group.validate(), Ok(()));

        let schema = group.schema.as_ref().expect("schema present");
        assert_eq!(
            schema.counters.len(),
            3,
            "member_bound=3 on an 8-entry backing array emits exactly 3 schema slots, \
             not the full backing capacity"
        );
        assert_eq!(
            group.counters.len(),
            3,
            "value slots match the bounded schema, not the backing capacity"
        );
        for (idx, desc) in schema.counters.iter().enumerate() {
            assert_eq!(
                desc.metadata.get("id").map(String::as_str),
                Some(idx.to_string()).as_deref(),
                "bounded members are the first `bound` indices, in order"
            );
            assert!(
                !desc.metadata.contains_key("acq_group"),
                "acq_group must still be stripped on a bounded declared group's members"
            );
        }
    }

    // Dedicated group + metric: 2 backing entries, member_bound set larger
    // than entries (5). The bound must clamp to entries, not read/emit
    // out-of-bounds slots.
    static V3_OVERBOUND_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "overbound_counter_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_OVERBOUND_GROUP_ENTRY: &'static AcquisitionGroup = &V3_OVERBOUND_GROUP;

    #[metric(
        name = "snapshot_v3_overbound_counters",
        metadata = { acq_group = "overbound_counter_probe" }
    )]
    static V3_OVERBOUND_COUNTERS: metriken::CounterGroup = metriken::CounterGroup::new(2);

    #[test]
    fn declared_group_member_bound_larger_than_entries_clamps_to_entries() {
        V3_OVERBOUND_COUNTERS.add(0, 1);
        V3_OVERBOUND_COUNTERS.add(1, 2);
        V3_OVERBOUND_GROUP.set_member_bound(5);
        let guard = V3_OVERBOUND_GROUP.acquire();
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

        let group = s
            .groups
            .iter()
            .find(|g| g.name == "unattributed/overbound_counter_probe")
            .expect("overbound declared group present");
        assert_eq!(group.validate(), Ok(()));

        let schema = group.schema.as_ref().expect("schema present");
        assert_eq!(
            schema.counters.len(),
            2,
            "a bound larger than entries() clamps to entries(), never reads past the backing array"
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
