use crate::agent::config::SnapshotFormat;
use crate::agent::external_metrics::{
    ExternalMetric, ExternalMetricValue, ExternalMetricsStore, MetricKey,
};
use crate::agent::timing::{AcquisitionGroup, AcquisitionGuard};
use crate::agent::*;

use metriken::{Value, Window};
use metriken_exposition::{
    Counter, Gauge, GroupSchema, GroupSnapshot, Histogram, MetricDesc, Snapshot, SnapshotV2,
    SnapshotV3,
};

use bytes::Bytes;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};
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
    /// The encoded bodies for this snapshot, filled on first request for each
    /// format and reused for every later request that hits the same cached
    /// snapshot.
    ///
    /// The TTL cache used to cache only the `Snapshot`, so a cache HIT still
    /// re-encoded the whole body — measured at **3.47 MB per request** on a
    /// 26-sampler host. `rmp_serde::encode::to_vec` starts from an empty `Vec`
    /// and grows by doubling, so that was also a dozen reallocations and a
    /// dozen copies per request, all of it discarded. Encoding once per
    /// snapshot turns every subsequent request into a refcount bump.
    msgpack: OnceLock<Bytes>,
    json: OnceLock<Arc<str>>,
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
            msgpack: OnceLock::new(),
            json: OnceLock::new(),
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

    /// The msgpack body for the current snapshot, encoded at most once per
    /// snapshot. Cloning the returned [`Bytes`] is a refcount bump, not a copy.
    pub async fn build_msgpack(&mut self, now: Instant) -> Bytes {
        let cached = {
            self.build(now).await;
            self.cached.as_ref().expect("build populates the cache")
        };
        cached
            .msgpack
            .get_or_init(|| {
                Bytes::from(
                    rmp_serde::encode::to_vec(&cached.snapshot)
                        .expect("failed to serialize snapshot"),
                )
            })
            .clone()
    }

    /// The JSON body for the current snapshot, encoded at most once per
    /// snapshot. Cloning the returned `Arc<str>` is a refcount bump.
    pub async fn build_json(&mut self, now: Instant) -> Arc<str> {
        let cached = {
            self.build(now).await;
            self.cached.as_ref().expect("build populates the cache")
        };
        cached
            .json
            .get_or_init(|| {
                serde_json::to_string(&cached.snapshot)
                    .expect("failed to serialize snapshot")
                    .into()
            })
            .clone()
    }
}

/// Metadata common to any exposed entry for one metric: `"metric"` (its
/// name) plus everything from the metric's own static metadata, plus
/// `"sampler"` attribution — also returned standalone for callers that want
/// the sampler name without building the whole map. Shared between `create`
/// (V2) and `create_v3` so the two builders can't independently drift on
/// what this prefix does; `create` uses the returned `BTreeMap` directly
/// (its wire format's native form, converted to a `HashMap` for V2's).
/// `create_v3` calls this only on a `SkeletonCache` miss (see
/// `fold_group_identities` and its own doc comment) — group ROUTING there
/// uses the cheaper `attribute_sampler` directly instead, since a cache hit
/// needs no metadata `BTreeMap` at all.
///
/// # KEEP IN SYNC
///
/// This helper covers only the shared prefix. Everything after it — the
/// `log_` skip that gates it (each caller's own, since it's a `continue` at
/// the caller's loop, not something a shared helper can do), and the
/// per-kind (Counter/Gauge/CounterGroup/GaugeGroup/Histogram)
/// walk/naming/membership rules — remains genuinely duplicated between
/// THREE places now, not two: `create` (V2), `create_v3`'s schema-building
/// (miss) arms, and `fold_group_identities`' matching arms (which fold
/// identity over the SAME membership without building any of this
/// metadata). `{id}`/`{id}x{idx}` naming, the histogram
/// `grouping_power`/`max_value_power` metadata keys, and — for
/// `fold_group_identities` specifically — the value-sentinel skip test for
/// default groups (it must decide "is this idx a member" the identical way
/// `create_v3` does, or the two disagree on membership; see the "wider
/// torn-read window" note on `fold_group_identities`). A change to any one
/// of the three must be checked against the other two.
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

/// # V2 output compatibility for reader-stamped (`PackedCounters`) groups
///
/// V2 predates declared acquisition groups and has no registration-
/// membership concept — every `CounterGroup`/`GaugeGroup` entry, declared or
/// not, keeps the transitional value-sentinel skip (`== 0`/`== i64::MIN`)
/// V2 has always used; that is V2's wire-format semantics, not a per-group
/// choice, and reader-stamped groups do not change it. The ONLY thing wave-2
/// Part A adds to V2 output for a migrated packed metric is the acquisition
/// window — previously silently absent (`PackedCounters::refresh()` is a
/// no-op, so there was nothing to stamp a per-metric window from) — pinned
/// by `v2_output_is_unchanged_except_windows_for_a_migrated_packed_metric`.
/// Serialises the snapshot builders under `cargo test`.
///
/// `create`/`create_v3` are the sole writers of reader-stamped group window
/// slots (a reader-stamped group's window IS the builder's own read of it —
/// see `AcquisitionGroup::set_reader_stamped`), and each such slot is a single
/// static shared by the whole process. In production one snapshot builder runs
/// at a time, so the single-writer invariant the seqlock's `debug_assert`
/// guards (see `timing.rs`) holds. The test harness runs many builder tests on
/// parallel threads, and once any test latches a group reader-stamped, every
/// subsequent `create_v3` walk acquires that shared slot — so two overlapping
/// builder calls trip the assert. This lock restores the production invariant
/// (one builder at a time) for tests without touching the hot path: it is
/// `#[cfg(test)]`, so production compiles nothing. Poison is recovered rather
/// than propagated, so a test that panics mid-build does not cascade-fail the
/// rest. (See issue #1130.)
#[cfg(test)]
static BUILDER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn create(
    timestamp: SystemTime,
    duration: Duration,
    external_metrics: Vec<ExternalMetric>,
) -> Snapshot {
    #[cfg(test)]
    let _serialize = BUILDER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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

    // Resolve every declared group's window ONCE, up front, instead of
    // once per tagged metric: 138 metrics carry `acq_group` today (14
    // wave-1 counter groups + 11 histogram-wave groups, 25 distinct groups
    // total — see `group_registry()`'s doc comment for how that registry
    // is built). A per-metric resolve (the original version of this fix)
    // paid a `group_registry()` lookup AND an `AcquisitionGroup::window()`
    // seqlock read 138 times; reading each group's window here, once, cuts
    // the actual seqlock reads to 25 — the per-metric step below becomes a
    // cheap in-memory `HashMap` lookup against this snapshot, not a fresh
    // atomic read.
    //
    // Keyed by `(sampler, name)` — the SAME parts `AcquisitionGroup`
    // itself stores (`group.sampler`/`group.name`, both already
    // `&'static str`, and the same tuple `group_registry()` itself is now
    // keyed by) — so building this map costs no allocation at all, and
    // neither does a per-metric lookup below (a `(&str, &str)` tuple, not
    // a freshly `format!`ed `String`).
    //
    // This also closes an intra-snapshot consistency hole the per-metric
    // version had: every member of a group now sees the SAME window for
    // this V2 snapshot (read once, before any member is visited), matching
    // `create_v3`'s first-touch semantics (see its `Entry::Vacant` arm).
    // Unlike `create_v3`, there is no walk-spanning "latest" re-read at
    // emit time to reconcile against — resolving once, up front, in effect
    // makes EVERY metric first-touch, so a group that gets re-stamped
    // mid-walk by a racing sampler tick is simply not observed by this
    // snapshot. That is the safe direction: `AcquisitionGuard::finish`
    // stamps last (see `AcquisitionGroup::acquire`), so a window can only
    // ever LAG the true acquisition, never lead it — not observing a
    // fresher stamp mid-walk means this snapshot's windows are, at worst, a
    // touch stale, never claiming freshness a value doesn't actually have.
    // Reader-stamped groups are filtered OUT here — see the doc comment
    // below on `reader_stamped_groups`/`ReaderStampedBracket` for why this
    // map cannot serve them (it would return a previous tick's stale
    // bracket, or `None` on the first call, never this tick's actual
    // read). Excluding them means a scalar `Counter`/`Gauge`/`Histogram`
    // that somehow ends up tagged with a reader-stamped group's name (not
    // possible today — see `reader_stamped_group`'s doc comment below —
    // but not structurally prevented either) misses here and falls into
    // the `None =>` arm's mismatch handling below, rather than silently
    // resolving to a stale or absent window as if it were a legitimate
    // sampler-stamped miss.
    let group_windows: HashMap<(&str, &str), Option<Window>> = group_registry()
        .values()
        .filter(|group| !group.is_reader_stamped())
        .map(|group| ((group.sampler, group.name), group.window()))
        .collect();

    // Reader-stamped (`PackedCounters` mmap-direct) groups CANNOT be served
    // by the resolve-once map above: nothing but the walk below ever stamps
    // their window slot (see `AcquisitionGroup::set_reader_stamped`), so
    // reading it up front — before this walk has touched any of the
    // group's members — would return whatever bracket a PREVIOUS `create()`
    // call left behind (or `None`, on the very first call), never this
    // tick's actual read. Track them separately: acquire the bracket at
    // first touch (below, in the CounterGroup/GaugeGroup arms), mark its
    // end immediately after each touching metric's member-value loop
    // (`AcquisitionGuard::mark_end` — the LAST such call before finish()
    // wins, so a group touched by several like-entity members, e.g.
    // `cgroup_syscall`'s 16 op-class maps, still ends up with its true
    // last-member-read as the end), then finish() (publish) once the whole
    // per-metric loop completes (see the finalization loop after it) and
    // patch every entry pushed for that group to the published window.
    // Publish timing ("finish once the loop completes") is coarser than
    // `create_v3`'s per-group emit point — V2's flat, single-pass walk has
    // no per-group deferred-emit stage to hook a tighter boundary into —
    // but `mark_end()` decouples the reported WIDTH from that publish
    // delay entirely: the width is this group's own read span (µs-scale),
    // not walk-scale, regardless of how much unrelated work runs between
    // mark_end() and finish(). Only visibility (when the window appears at
    // all) lags slightly behind the tightest possible, which is the same
    // safe direction `AcquisitionGuard` already guarantees elsewhere —
    // begin_ns is each group's true first touch either way.
    //
    // `counter_positions`/`gauge_positions` are APPEND-ONLY: a position is
    // pushed here exactly once (immediately after the matching
    // `s.counters`/`s.gauges` push, same index), never removed or reused.
    // The finalization loop below patches each one exactly once; the
    // `debug_assert!` there pins that "exactly once" — a position appearing
    // twice (a future bug re-touching the same entry) would silently
    // overwrite an already-patched window otherwise.
    struct ReaderStampedBracket {
        group: &'static AcquisitionGroup,
        guard: AcquisitionGuard<'static>,
        counter_positions: Vec<usize>,
        gauge_positions: Vec<usize>,
    }
    let reader_stamped_groups: HashMap<(&str, &str), &'static AcquisitionGroup> = group_registry()
        .values()
        .filter(|group| group.is_reader_stamped())
        .map(|group| ((group.sampler, group.name), *group))
        .collect();
    let mut reader_stamped_brackets: HashMap<(&str, &str), ReaderStampedBracket> = HashMap::new();

    for (metric_id, metric) in metriken::metrics().iter().enumerate() {
        let (value, stored_window) = metric.value_with_window();

        if value.is_none() {
            continue;
        }

        let name = metric.name();

        if name.starts_with("log_") {
            continue;
        }

        // KEEP IN SYNC with create_v3 AND fold_group_identities — see the
        // doc comment on `metric_metadata` (the shared prefix) and on
        // `create_v3` itself (the per-kind walk/naming/membership rules
        // below, which are NOT shared and are duplicated independently in
        // each of these three places).
        let (metadata, sampler) = metric_metadata(metric, &sampler_mods);
        let mut metadata: HashMap<String, String> = metadata.into_iter().collect();

        // Migrated metrics (wave 1+: plain `LazyCounter`/`CounterGroup`/
        // `RwLockHistogram` stamped by an `AcquisitionGroup` instead of a
        // per-metric `Windowed*` wrapper) carry `acq_group` in their static
        // metadata but no per-metric window of their own —
        // `value_with_window()`/`load_with_window(idx)` fall through to the
        // trait's windowless default (`None`) for them. Look up the
        // pre-resolved group window (`group_windows`, above) instead, so
        // V2 output doesn't silently go window-blind for every metric that
        // migrates. A metric with no `acq_group` keeps its existing
        // per-metric window path untouched below.
        //
        // `group_window` is `Option<Option<Window>>`: outer `None` means
        // "not a group member (or an unresolved/typo'd tag — see the
        // `debug_assert!` below), don't override"; `Some(None)` means "is
        // a member of a group that just hasn't been stamped yet" (still no
        // window, but deliberately, not by falling through to a stale
        // per-metric value). `.unwrap_or(stored_window)` therefore does
        // exactly the right thing in both cases: outer `None` → keep
        // `stored_window`; outer `Some(w)` → replace it with the group's
        // `w` (which may itself be `None`).
        //
        // The window this attaches is a whole-group ACQUISITION window
        // (one stamp covering an entire sweep — a per-CPU read, a map
        // read), not the old per-entry stamp. It is measured ~1.75× wider
        // than the per-entry windows it replaces (see
        // docs/journal/2026-08-17-window-sidecar-cost.md proposal 2) — an
        // accepted, honest widening (an upper bound on the true per-entry
        // acquisition time, never an underestimate), and now the same
        // semantic V3 already uses: V2 and V3 consumers see the identical
        // acquisition-window meaning for a migrated metric.
        let group_window: Option<Option<Window>> = metric.metadata().get("acq_group").map(|g| {
            // `sampler` (above) is an owned `String` — needed for the
            // metadata map, and cloned into every `CounterGroup`/
            // `GaugeGroup` entry below, so it can't be a borrow of
            // `sampler_mods` itself. For the allocation-free tuple lookup
            // we need a `&str` that lives across the WHOLE loop (to match
            // `group_windows`' `&'static str` components), not a fresh
            // per-iteration owned `String` — so resolve it a second time
            // here, only for tagged metrics, via the same
            // `attribute_sampler` call `metric_metadata` already makes
            // internally (cheap: a linear scan over ~25 registered
            // samplers, no allocation) rather than widening the shared
            // `metric_metadata` helper's return type (used identically by
            // `create_v3`).
            let sampler_attr =
                crate::agent::samplers::attribute_sampler(metric.module(), &sampler_mods);

            match group_windows.get(&(sampler_attr, g)) {
                Some(w) => *w,
                None => {
                    // A miss here has two possible causes, and only one is
                    // a bug. (1) The group IS registered but reader-stamped
                    // — deliberately excluded from `group_windows` above,
                    // not a typo; `CounterGroup`/`GaugeGroup` handle this
                    // legitimately via `reader_stamped_group` below, and a
                    // scalar `Counter`/`Gauge`/`Histogram` hitting it is
                    // flagged by THAT arm's own, more specific
                    // `debug_assert!` (clearer than this generic message,
                    // which would otherwise wrongly claim the group isn't
                    // registered at all) — so skip the generic assert here
                    // and let the per-kind check downstream catch it. (2)
                    // The group genuinely isn't registered — a typo or
                    // rename mismatch, a migration bug on EITHER format,
                    // not just V3's (same message shape as create_v3's,
                    // deliberately).
                    if !reader_stamped_groups.contains_key(&(sampler_attr, g)) {
                        debug_assert!(
                            false,
                            "metric `{name}` declares acq_group=\"{g}\" for sampler \
                             `{sampler}`, but no AcquisitionGroup (\"{sampler}\", \"{g}\") is \
                             registered on ACQUISITION_GROUPS; V2 output keeps this metric's \
                             untouched per-metric window path instead of overriding it \
                             (create_v3 has a default group to fall back to; V2 does not)",
                        );
                    }
                    None
                }
            }
        });

        // Reader-stamped groups (see the doc comment above
        // `reader_stamped_groups`): resolved independently of
        // `group_window` above, which cannot serve them. Only
        // `Value::CounterGroup`/`Value::GaugeGroup` consult this to build a
        // `ReaderStampedBracket` today — no scalar `Counter`/`Gauge`/
        // `Histogram` is ever `PackedCounters`-backed, since `PackedCounters`
        // only ever wraps a `CounterGroup`. The scalar arms below instead
        // `debug_assert!` that this is `None` for them: if a future packed
        // SCALAR type is ever added, routing it correctly needs the same
        // acquire-at-first-touch/mark_end/patch-at-finish machinery
        // `ReaderStampedBracket` already gives CounterGroup/GaugeGroup —
        // extend that struct (or give scalars their own single-entry
        // variant of it) rather than silently letting a reader-stamped tag
        // fall through to `group_window`'s `stored_window` fallback, which
        // would silently under-report (a scalar `LazyCounter`'s
        // `value_with_window()` is windowless for a migrated type, same as
        // any other wave-1 metric) rather than carrying the real bracket.
        let reader_stamped_group: Option<&'static AcquisitionGroup> =
            metric.metadata().get("acq_group").and_then(|g| {
                let sampler_attr =
                    crate::agent::samplers::attribute_sampler(metric.module(), &sampler_mods);
                reader_stamped_groups.get(&(sampler_attr, g)).copied()
            });

        // V2 has no group concept — `acq_group` only means something to
        // `create_v3`'s routing. Strip it unconditionally (mirroring
        // `create_v3`'s declared-group strip) so a metric migrating to a
        // declared acquisition group does not grow a new label in V2
        // output; default mode stays byte-stable vs main.
        metadata.remove("acq_group");

        let name = format!("{metric_id}");

        match value {
            Some(Value::Counter(value)) => {
                debug_assert!(
                    reader_stamped_group.is_none(),
                    "metric `{}` is a scalar Counter tagged with a reader-stamped acq_group; \
                     PackedCounters only ever wraps a CounterGroup, so this combination isn't \
                     supported yet — see the doc comment on `reader_stamped_group` for what \
                     routing a packed scalar would need",
                    metric.name()
                );
                s.counters.push(
                    Counter::new(name, value, metadata)
                        .with_window(group_window.unwrap_or(stored_window)),
                )
            }
            Some(Value::Gauge(value)) => {
                debug_assert!(
                    reader_stamped_group.is_none(),
                    "metric `{}` is a scalar Gauge tagged with a reader-stamped acq_group; \
                     PackedCounters only ever wraps a CounterGroup, so this combination isn't \
                     supported yet — see the doc comment on `reader_stamped_group` for what \
                     routing a packed scalar would need",
                    metric.name()
                );
                s.gauges.push(
                    Gauge::new(name, value, metadata)
                        .with_window(group_window.unwrap_or(stored_window)),
                )
            }
            Some(Value::CounterGroup(g)) => {
                // Reader-stamped group (`PackedCounters` mmap-direct): the
                // walk-spanning bracket is acquired at first touch and
                // finished (with every position pushed below patched to
                // its window) after the whole per-metric loop — see the
                // doc comment on `reader_stamped_groups`. V2 OUTPUT
                // COMPATIBILITY: iteration (`0..g.entries()`) and the
                // value-sentinel skip below are otherwise UNCHANGED from
                // the non-reader-stamped path — this only changes which
                // window ends up attached, never membership or value
                // semantics, so a migrated packed metric's V2 output is
                // byte-identical except for windows (pinned by
                // `v2_output_is_unchanged_except_windows_for_a_migrated_packed_metric`).
                //
                // Deliberately NOT switching V2 to `create_v3`'s
                // metadata-presence membership: for `task_cpu_usage` this
                // keeps a real `0..MAX_PID` = 4,194,304-iteration walk
                // every V2 tick (cheap per-iteration — a value load plus a
                // sentinel-skip branch, no allocation for the common
                // unpopulated case — but O(capacity), not O(population),
                // unlike V3's `metadata_snapshot()` walk). The two aren't
                // just a performance tradeoff: they can DISAGREE. If a
                // task's `task_info` ringbuf event is ever dropped (BPF
                // ringbuf momentarily full — see
                // `src/agent/samplers/cpu/linux/usage/mod.bpf.c`'s
                // `handle_task_info`/`handle_task_exit`, and the retry
                // fix in the accompanying `fix(bpf)` commit for the
                // window this leaves even after that fix), the counter
                // VALUE can still be nonzero (BPF increments it
                // unconditionally in the hot path) while `load_metadata`
                // for that index is `None` (the metadata insert that
                // would have registered it never happened). Value-based
                // walk-and-skip still finds and emits that entry (unlabeled
                // beyond `id`, but present, still summable); metadata-
                // presence membership would silently drop it. Keeping V2's
                // existing walk preserves that fallback robustness, not
                // just historical byte-parity.
                if let Some(ag) = reader_stamped_group {
                    let bracket = reader_stamped_brackets
                        .entry((ag.sampler, ag.name))
                        .or_insert_with(|| ReaderStampedBracket {
                            group: ag,
                            guard: ag.acquire(),
                            counter_positions: Vec::new(),
                            gauge_positions: Vec::new(),
                        });
                    for counter_id in 0..g.entries() {
                        let (value, _entry_window) = g.load_with_window(counter_id);
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

                        s.counters.push(Counter::new(
                            format!("{metric_id}x{counter_id}"),
                            value,
                            metadata,
                        ));
                        bracket.counter_positions.push(s.counters.len() - 1);
                    }
                    // Mark the end right after this metric's member values
                    // were read — see the doc comment on `ReaderStampedBracket`.
                    bracket.guard.mark_end();
                } else {
                    for counter_id in 0..g.entries() {
                        // Atomic pair read: value + window under one lock, so a
                        // concurrent writer can never pair a fresh value with a
                        // stale window (drivehealth's async tear surface). For a
                        // migrated (group-stamped) member, `load_with_window`
                        // itself only ever returns `None` for the window half —
                        // `group_window` (resolved once above, outside this
                        // loop, since it's the same for every member) is what
                        // actually carries the group's window.
                        let (value, entry_window) = g.load_with_window(counter_id);
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
                                .with_window(group_window.unwrap_or(entry_window)),
                        )
                    }
                }
            }
            Some(Value::GaugeGroup(g)) => {
                // See the CounterGroup arm above for the reader-stamped
                // rationale; no `PackedCounters`-style gauge group exists
                // today, kept symmetric for the first one that does.
                if let Some(ag) = reader_stamped_group {
                    let bracket = reader_stamped_brackets
                        .entry((ag.sampler, ag.name))
                        .or_insert_with(|| ReaderStampedBracket {
                            group: ag,
                            guard: ag.acquire(),
                            counter_positions: Vec::new(),
                            gauge_positions: Vec::new(),
                        });
                    for gauge_id in 0..g.entries() {
                        let (value, _entry_window) = g.load_with_window(gauge_id);
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

                        s.gauges.push(Gauge::new(
                            format!("{metric_id}x{gauge_id}"),
                            value,
                            metadata,
                        ));
                        bracket.gauge_positions.push(s.gauges.len() - 1);
                    }
                    // See the CounterGroup arm above.
                    bracket.guard.mark_end();
                } else {
                    for gauge_id in 0..g.entries() {
                        // Atomic pair read (see CounterGroup arm above); same
                        // group-window override for a migrated member.
                        let (value, entry_window) = g.load_with_window(gauge_id);
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
                                .with_window(group_window.unwrap_or(entry_window)),
                        )
                    }
                }
            }
            Some(Value::Histogram(h)) => {
                debug_assert!(
                    reader_stamped_group.is_none(),
                    "metric `{}` is a Histogram tagged with a reader-stamped acq_group; \
                     PackedCounters only ever wraps a CounterGroup, so this combination isn't \
                     supported yet — see the doc comment on `reader_stamped_group` for what \
                     routing a packed scalar would need",
                    metric.name()
                );
                if let Some(value) = h.load() {
                    metadata.insert(
                        "grouping_power".to_string(),
                        h.config().grouping_power().to_string(),
                    );
                    metadata.insert(
                        "max_value_power".to_string(),
                        h.config().max_value_power().to_string(),
                    );

                    s.histograms.push(
                        Histogram::new(name, value, metadata)
                            .with_window(group_window.unwrap_or(stored_window)),
                    )
                }
            }
            _ => {}
        }
    }

    // Publish every reader-stamped bracket now that the per-metric loop has
    // read every member's value — each bracket's window content was
    // already decided by its last `mark_end()` call above; `finish()` here
    // only decides when it becomes visible (see the doc comment on
    // `ReaderStampedBracket`) — then patch the published window into every
    // entry recorded for that group above. `Counter`/`Gauge`'s `window`
    // field is public exactly so this post-hoc patch is possible without
    // re-pushing. `bracket.guard` is moved out of `bracket` by value here
    // (a partial move — `bracket.group`/`counter_positions`/
    // `gauge_positions` stay usable below); every `ReaderStampedBracket`
    // was constructed with a real guard (never a placeholder), so there is
    // no `Option` to unwrap.
    for (_, bracket) in reader_stamped_brackets {
        bracket.guard.finish();
        let window = bracket.group.window();
        for idx in bracket.counter_positions {
            debug_assert!(
                s.counters[idx].window.is_none(),
                "counter_positions is append-only and each position is patched exactly \
                 once; a Some(_) here means this index was already patched — a bug, not a \
                 legitimate re-touch"
            );
            s.counters[idx].window = window;
        }
        for idx in bracket.gauge_positions {
            debug_assert!(
                s.gauges[idx].window.is_none(),
                "gauge_positions is append-only and each position is patched exactly once; \
                 a Some(_) here means this index was already patched — a bug, not a \
                 legitimate re-touch"
            );
            s.gauges[idx].window = window;
        }
    }

    for metric in external_metrics.into_iter() {
        // Capture the window before metric fields are consumed by the moves below.
        // Window is Copy so this is free; precedence level 2 (external source stamp).
        let window = metric.window;

        // The entry name is a COLUMN KEY, not a display name — scalars use
        // `{metric_id}`, grouped metrics `{metric_id}x{counter_id}`, and the
        // human name travels in `metadata["metric"]`. External metrics used to
        // pass `String::new()`, so every one of them keyed the same empty
        // column: `.rez` ingest keys columns by this name, so two active
        // external metrics of the same type double-pushed into one column
        // (misaligning values against timestamps from that row on), and two of
        // different types were silently dropped by the shape-mismatch skip.
        //
        // Derive it from (name, labels) via the same hash the store's own
        // `MetricKey` uses for identity, exactly as `create_v3` does. Identity
        // rather than position because `get_active()`'s order churns
        // tick-to-tick: a positional name would silently reattach a metric's
        // values under a different column, which a name-keyed consumer reads as
        // a continuous series that isn't one. Name alone is not enough either —
        // two external metrics may share a name with different labels.
        //
        // `/` and `#` cannot appear in the numeric keys above, so an external
        // metric can never collide with a registry metric's column.
        let labels_hash = MetricKey::new(&metric.name, &metric.labels).labels_hash;
        let name = format!("external/{}#{labels_hash:016x}", metric.name);

        let mut metadata: HashMap<String, String> = [
            ("metric".to_string(), metric.name.clone()),
            ("source".to_string(), "external".to_string()),
        ]
        .into();

        for (k, v) in metric.labels {
            metadata.insert(k, v);
        }

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
/// A cache HIT (this tick's [`GroupSkeleton::identity`] equals last tick's)
/// skips [`GroupSchema`] assembly entirely: no `MetricDesc`, no `BTreeMap`,
/// no formatted member name is built for that group this tick — only its
/// values, which change every tick regardless and must always be read. The
/// cached `Arc<GroupSchema>` is cloned (a refcount bump) and the cached wire
/// `schema_hash` is reused verbatim. A MISS (identity differs, or there is
/// no cached entry yet) assembles the schema exactly as before and pays for
/// a fresh [`GroupSchema::hash`] — the full canonical msgpack encode + fold
/// that scales with schema size.
///
/// # Two hashes, distinct roles
///
/// - **Identity** ([`GroupSkeleton::identity`]) — internal, never
///   transmitted, cheap to fold: per group per tick, for each kind in fixed
///   order (counters, gauges, histograms), for each member in the same
///   order the schema lists them, fold the member's identity (its metric id
///   and, for a group entry, its index — as raw integer bytes, no
///   `format!`) then its metadata as sorted key/value pairs (via the
///   borrowing `with_metadata`/`for_each_metadata` accessors, sorted on a
///   small inline stack buffer — see `identity_fold_metadata` — no
///   allocation for the metadata maps rezolus samplers actually produce).
///   128-bit FNV-1a, same collision reasoning as the wire hash: a collision
///   here would serve a stale schema — the C1 failure class below.
/// - **Wire `schema_hash`** ([`GroupSkeleton::hash`]) — unchanged:
///   [`GroupSchema::hash`], computed only on a miss.
///
/// Deliberately narrower than a full `GroupSchema` fingerprint: identity
/// omits a group's own static metric-level metadata (the `"metric"`/
/// `"sampler"` pair `metric_metadata` derives), which is fixed at
/// registration and never mutates at runtime — `insert_metadata`/
/// `set_metadata` are only ever called on a `CounterGroup`/`GaugeGroup`'s
/// PER-INDEX metadata (a task's `comm`, a cgroup's `name`), never on a
/// `MetricEntry`'s own `metadata()`. That per-index metadata IS what
/// identity folds, via the same borrowing accessors, so the C1 case below
/// still forces a miss.
///
/// # Delivered: a hit allocates a small, member-count-independent constant
///
/// This closes the gap an earlier version of this cache (which compared
/// full `GroupSchema` equality — assembling it unconditionally, cache hit
/// or miss, and skipping only the hash) left open: that cache still
/// allocated MORE per tick than V2 on a hit, measured 1.2–3.4× V2's
/// allocations, with the emit-time `cached.schema.clone()` alone 54% of
/// allocations at 2k members. Two upstream changes made the fix possible —
/// both landed on the pinned metriken rev and both are in active use here:
/// the borrowing `with_metadata`/`for_each_metadata` accessors (no full-map
/// clone to decide "did this member's identity change"), and
/// `GroupSnapshot.schema: Option<Arc<GroupSchema>>` (a hit hands out a
/// refcount bump, not a deep clone). See
/// `v3_hit_tick_allocations_are_a_small_constant_not_o_n` for the measured
/// before/after allocation counts on a 512-member fixture group.
///
/// # Why full-schema equality was unsound with names only (still true here)
///
/// An earlier version of this cache (before the identity hash existed)
/// compared member NAME lists only. That's unsound: metriken metadata
/// mutates in place at a stable index (`insert_metadata`/`set_metadata`,
/// e.g. a task's `comm` or a cgroup's `name`), and the kernel recycles PIDs
/// and cgroup ids — so a slot's metadata can change while its
/// `"{metric_id}x{idx}"` name stays byte-for-byte identical. A names-only
/// cache would call that a hit, keep serving the OLD occupant's metadata
/// under an UNCHANGED `schema_hash`, and a receiver caching parsed schemas
/// by `(name, schema_hash)` would bind new values to dead labels
/// indefinitely. The identity fold covers names AND per-index metadata for
/// exactly this reason — see
/// `declared_group_schema_reflects_metadata_mutated_at_a_stable_index`, the
/// pinned regression test.
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
    /// This tick's cheap membership fingerprint, folded by
    /// [`fold_group_identities`]. Compared against next tick's freshly
    /// folded identity to decide hit vs. miss BEFORE `create_v3`'s
    /// value-collecting walk begins — see the `SkeletonCache` doc comment.
    identity: (u64, u64),
    schema: Arc<GroupSchema>,
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

/// FNV-1a-128 offset basis and prime — the standard constants, same
/// algorithm [`GroupSchema::hash`] uses but a completely separate hash
/// space: this one is an internal cache key that is never transmitted, so
/// nothing requires (or forbids) sharing constants with the wire hash.
const IDENTITY_FNV_OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
const IDENTITY_FNV_PRIME: u128 = 0x0000000001000000000000000000013b;

#[inline]
fn identity_fold(mut acc: u128, bytes: &[u8]) -> u128 {
    for &b in bytes {
        acc ^= b as u128;
        acc = acc.wrapping_mul(IDENTITY_FNV_PRIME);
    }
    acc
}

/// Fold a length-prefixed byte string into `acc`: the byte length as fixed-
/// width `u64` bytes, then the bytes themselves. Plain concatenation of
/// variable-length strings is ambiguous at the boundary — folding `"ab"`
/// then `"cd"` produces the exact same bytes, and therefore the exact same
/// hash, as folding `"a"` then `"bcd"`. Framing every variable-length input
/// this way makes the byte stream self-describing, so two DIFFERENT
/// (key, value, ...) sequences can never fold to the same identity by
/// boundary-shifting into each other — the property `GroupSchema::hash`
/// gets for free from msgpack's own framing, needed here too: an
/// undetected collision here means a stale schema served under a changed
/// identity, the same failure class the C1 regression fixed for the wire
/// hash. Fixed-width fields (`metric_id`/`idx`, always exactly 8 bytes via
/// `to_le_bytes()`) don't need this — their width never varies, so they
/// can't shift.
#[inline]
fn identity_fold_len_prefixed(acc: u128, bytes: &[u8]) -> u128 {
    let acc = identity_fold(acc, &(bytes.len() as u64).to_le_bytes());
    identity_fold(acc, bytes)
}

/// Fold a group member's metadata into `acc` as sorted `(key, value)`
/// pairs, so the fold is deterministic regardless of the source
/// `HashMap`'s iteration order (the same non-determinism the sparse-group
/// arms in `create_v3` already sort around). The pair COUNT is folded
/// first (fixed-width, so it can't be confused with a key/value), then
/// each key and value length-prefixed (see `identity_fold_len_prefixed`),
/// so the whole (count, pairs) sequence is unambiguous regardless of
/// content — including at the boundary with whatever this member's caller
/// folds next (a following member's fixed-width `metric_id`/`idx` prefix
/// can never be mistaken for "one more pair" of this one).
///
/// Sorts on a fixed-size stack array — no heap allocation — for the common
/// case. Every metadata map a rezolus sampler attaches today has at most a
/// handful of entries (`pid`/`tgid`/`comm`/`cgroup` is the largest, at 4);
/// `INLINE` is set well above that. A map that somehow exceeds it falls
/// back to a one-off heap `Vec` rather than silently truncating; that
/// fallback is not expected to ever trigger in practice.
fn identity_fold_metadata(acc: u128, metadata: &HashMap<String, String>) -> u128 {
    const INLINE: usize = 16;
    if metadata.len() <= INLINE {
        let mut buf: [(&str, &str); INLINE] = [("", ""); INLINE];
        let mut n = 0;
        for (k, v) in metadata {
            buf[n] = (k.as_str(), v.as_str());
            n += 1;
        }
        let pairs = &mut buf[..n];
        pairs.sort_unstable();
        let mut h = identity_fold(acc, &(pairs.len() as u64).to_le_bytes());
        for (k, v) in pairs.iter() {
            h = identity_fold_len_prefixed(h, k.as_bytes());
            h = identity_fold_len_prefixed(h, v.as_bytes());
        }
        h
    } else {
        let mut pairs: Vec<(&str, &str)> = metadata
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        pairs.sort_unstable();
        let mut h = identity_fold(acc, &(pairs.len() as u64).to_le_bytes());
        for (k, v) in pairs {
            h = identity_fold_len_prefixed(h, k.as_bytes());
            h = identity_fold_len_prefixed(h, v.as_bytes());
        }
        h
    }
}

/// Fold a member's OPTIONAL metadata (`with_metadata`'s result — `None` for
/// an index with no metadata attached at all, `Some` (possibly an empty
/// map) for one that does) into `acc`, folding a 1-byte presence flag FIRST
/// so "absent" and "present-but-empty" can never alias.
///
/// Without the flag, "absent" contributes zero bytes and "present, zero
/// pairs" contributes exactly the 8-byte zero-count prefix
/// `identity_fold_metadata` folds for an empty map — a real, constructible
/// collision: a member with `m: None` immediately followed by the NEXT
/// member's `metric_id`/`idx` prefix folds the exact same bytes as a member
/// with `m: Some(empty)` (contributing that 8-byte zero count) immediately
/// followed by a DIFFERENT next member whose `metric_id`/`idx` bytes happen
/// to fill the gap the same way — reachable in a synthetic registry where a
/// group's later member's `metric_id` is small enough to overlap. Folding
/// presence (0 = absent, 1 = present) first means "absent" is now exactly
/// 1 byte and "present" is at least 9 (1 + the 8-byte count), so the two
/// can never be confused regardless of what follows.
#[inline]
fn identity_fold_metadata_presence(acc: u128, m: Option<&HashMap<String, String>>) -> u128 {
    match m {
        Some(m) => identity_fold_metadata(identity_fold(acc, &[1u8]), m),
        None => identity_fold(acc, &[0u8]),
    }
}

/// Per-group running identity, one accumulator per kind so the final
/// identity can fold them in the fixed (counters, gauges, histograms) order
/// `GroupSchema` lists members in, regardless of the interleaved order the
/// registry walk actually visits different kinds' registry entries in.
#[derive(Clone, Copy)]
struct GroupIdentityAccum {
    counters: u128,
    gauges: u128,
    histograms: u128,
}

impl Default for GroupIdentityAccum {
    fn default() -> Self {
        Self {
            counters: IDENTITY_FNV_OFFSET,
            gauges: IDENTITY_FNV_OFFSET,
            histograms: IDENTITY_FNV_OFFSET,
        }
    }
}

impl GroupIdentityAccum {
    fn finish(&self) -> (u64, u64) {
        let mut h = IDENTITY_FNV_OFFSET;
        h = identity_fold(h, &self.counters.to_le_bytes());
        h = identity_fold(h, &self.gauges.to_le_bytes());
        h = identity_fold(h, &self.histograms.to_le_bytes());
        ((h >> 64) as u64, h as u64)
    }
}

/// One group's cache decision for this tick, resolved by
/// [`fold_group_identities`] before `create_v3`'s real walk begins.
struct GroupDecision {
    /// `true`: this tick's identity differs from `cache`'s (or there is no
    /// cached entry yet) — `create_v3` must assemble a fresh `GroupSchema`
    /// for this group. `false`: membership (names + per-member metadata) is
    /// byte-identical to last tick's — `create_v3` clones the cached
    /// `Arc<GroupSchema>` instead of touching a single `MetricDesc`.
    ///
    /// The identity value itself isn't carried past this decision: on a
    /// miss, `create_v3` derives the identity it STORES from what its own
    /// walk actually collects (`GroupBuilder::walk_identity`), not from
    /// this pre-pass's read — see the "miss-tick cache poisoning" note on
    /// `fold_group_identities` for why that distinction matters.
    needs_schema: bool,
}

/// First pass over the metriken registry: fold each group's member
/// identity — NOT values, NOT windows, see the `SkeletonCache` doc comment
/// for what "identity" covers and omits — into a cheap running hash without
/// building a single `MetricDesc`, `BTreeMap`, or formatted name. Compares
/// each group's freshly folded identity against `cache`'s last-tick
/// identity to decide, before `create_v3`'s value-collecting walk begins,
/// which groups can skip schema assembly entirely this tick.
///
/// This is a full second walk of `metriken::metrics()` — but the registry
/// itself is small (bounded by declared metrics, not by live cardinality:
/// CPUs/tasks/cgroups are members WITHIN one registry entry, walked here
/// too, but cheaply, with no allocation). What this pass avoids is walking
/// a STABLE group's members while allocating for each one, which is what
/// made the previous full-schema-equality cache cost more per tick than V2
/// even on a hit.
///
/// External metrics are not folded here — see `create_v3`'s external-
/// metrics block, which always assembles a fresh schema for `external/main`
/// (this pass simply never produces a decision for that key, so
/// `create_v3` defaults it to `needs_schema: true`). External metrics are
/// push-ingested and comparatively few, not cardinality-scaled by BPF/task/
/// cgroup population, so they are out of scope for the allocation target
/// this cache exists to hit; always rebuilding keeps their exact prior
/// behavior (and test coverage) untouched.
///
/// # A wider (still narrow, still accepted) torn-read window for default
/// groups
///
/// A DEFAULT (non-declared) group's membership is value-derived (the
/// transitional V2-style sentinel skip — see `create_v3`'s doc comment):
/// whether index `idx` counts as a member depends on reading its CURRENT
/// value, here AND again in `create_v3`'s own walk. Those are two
/// SEPARATE, non-atomic reads of the same counter/gauge, same class as the
/// value/metadata torn-read gap `create_v3`'s CounterGroup arm already
/// documents and accepts (2-3% torn under a deliberate concurrent-recycle
/// hammer, effectively zero in production because sampler writes complete
/// synchronously inside `refresh()` — fully quiesced before `create_v3`
/// ever runs — rather than racing the snapshot builder from another task).
/// This pass widens that window from "within one member's two reads" to
/// "across this whole pre-pass versus the main walk", so a value that
/// crosses the zero/`None` membership boundary in that window can make a
/// default group's identity (this pass) and its actual collected
/// membership (the main walk) disagree for that one tick. The failure mode
/// is bounded and contained at the agent: on a miss the stored identity is
/// folded from what the walk itself collected (so cache entries are always
/// self-consistent), and on a hit the emit site compares per-kind value
/// lengths against the cached schema and, on a mismatch, evicts the entry
/// and skips emitting that group — nothing invalid reaches the wire, and
/// the next tick rebuilds. One residual: the length check is a proxy for
/// identity, so an *equal-arity* membership swap inside the window (one
/// member drops below the sentinel while another crosses above it) would
/// pass it and bind this tick's values to the previous schema's labels.
/// Closing that means re-folding identity on the hit path, which costs the
/// per-member metadata read the hit path exists to avoid; recorded as an
/// accepted trade rather than taken. Not eliminated outright because doing so
/// would mean re-merging the two passes and losing the hit-path allocation
/// win this cache exists for; named here so it's a known, accepted
/// trade-off rather than a surprise a reviewer has to rediscover. Declared
/// groups have no such window — their membership is registration-derived,
/// not value-derived, so nothing about this pass or the main walk needs to
/// agree on a value to agree on membership.
/// The member indices of a declared group-typed metric.
///
/// Two shapes, because two things can be known. A `member_bound` says "the
/// first N indices", which is what a per-CPU sweep over `0..possible_cpus()`
/// has. An explicit set says exactly which indices are populated, which is what
/// a sampler allowed only part of the machine has — and that set is rarely a
/// prefix.
///
/// The distinction is not cosmetic. A registered `CounterGroup` slot that is
/// never written reads as `0`, not as absent, so declaring a prefix that
/// overstates the population publishes zeros for indices nothing measured — a
/// wrong value where the honest answer is no value at all.
enum MemberIter<'a> {
    Prefix(std::ops::Range<usize>),
    Set(std::slice::Iter<'a, usize>),
}

impl Iterator for MemberIter<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        match self {
            MemberIter::Prefix(range) => range.next(),
            MemberIter::Set(iter) => iter.next().copied(),
        }
    }
}

/// Resolve a declared group's members: the explicit set when one was declared,
/// otherwise the dense prefix, clamped to what the backing array actually has.
fn members<'a>(set: Option<&'a [usize]>, bound: Option<usize>, entries: usize) -> MemberIter<'a> {
    match set {
        // An explicit set wins: a caller that knows the exact indices knows
        // strictly more than a bound. Still clamped — a stale set must not walk
        // past the backing array.
        Some(set) => {
            let end = set.partition_point(|idx| *idx < entries);
            MemberIter::Set(set[..end].iter())
        }
        None => MemberIter::Prefix(0..bound.map_or(entries, |b| b.min(entries))),
    }
}

fn fold_group_identities<'a>(
    cache: &SkeletonCache,
    group_registry: &HashMap<(&'static str, &'static str), &'static AcquisitionGroup>,
    sampler_mods: &'a [(&'a str, &'a str)],
) -> HashMap<(&'a str, &'a str), GroupDecision> {
    let mut accums: HashMap<(&'a str, &'a str), GroupIdentityAccum> = HashMap::new();
    let mut idx_scratch: Vec<usize> = Vec::new();

    for (metric_id, metric) in metriken::metrics().iter().enumerate() {
        let Some(value) = metric.value() else {
            continue;
        };

        let name = metric.name();
        if name.starts_with("log_") {
            continue;
        }

        // Routing mirrors create_v3's (KEEP IN SYNC), and mirrors `create`
        // (V2)'s `group_windows` for the SAME reason: `attribute_sampler`
        // returns a borrowed `&str` (tied to `sampler_mods`, which outlives
        // this whole function), and `metric.metadata().get("acq_group")`
        // is a transient borrowed lookup key, used only against
        // `group_registry` below — never stored. Once matched, the group's
        // OWN `&'static str` fields (`ag.sampler`/`ag.name`) become the
        // key that's actually stored/returned, so this pass never
        // `format!`s a `String` at all: not for the metric-level metadata
        // `BTreeMap` (still skipped entirely, as before), and now not for
        // routing either — the ~334-registry-entry `format!("{sampler}/
        // {acq_group}")` this used to pay for every tick is gone.
        let sampler = crate::agent::samplers::attribute_sampler(metric.module(), sampler_mods);
        let mut declared = false;
        let mut member_bound: Option<usize> = None;
        let mut member_set: Option<&[usize]> = None;
        let mut reader_stamped = false;
        let group_key: (&str, &str) = match metric.metadata().get("acq_group") {
            Some(acq_group) => {
                if let Some(ag) = group_registry.get(&(sampler, acq_group)) {
                    declared = true;
                    member_bound = ag.member_bound();
                    member_set = ag.member_set();
                    reader_stamped = ag.is_reader_stamped();
                    (ag.sampler, ag.name)
                } else {
                    (sampler, "main")
                }
            }
            None => (sampler, "main"),
        };

        let accum = accums.entry(group_key).or_default();
        let metric_id = metric_id as u64;

        match value {
            Value::Counter(_) => {
                accum.counters = identity_fold(accum.counters, &metric_id.to_le_bytes());
            }
            Value::Gauge(_) => {
                accum.gauges = identity_fold(accum.gauges, &metric_id.to_le_bytes());
            }
            Value::CounterGroup(g) => {
                if reader_stamped {
                    idx_scratch.clear();
                    g.for_each_metadata(&mut |idx, _| idx_scratch.push(idx));
                    idx_scratch.sort_unstable();
                    for &idx in idx_scratch.iter() {
                        let idx64 = idx as u64;
                        g.with_metadata(idx, &mut |m| {
                            let mut h = identity_fold(accum.counters, &metric_id.to_le_bytes());
                            h = identity_fold(h, &idx64.to_le_bytes());
                            h = identity_fold_metadata_presence(h, m);
                            accum.counters = h;
                        });
                    }
                } else {
                    for idx in members(member_set, member_bound, g.entries()) {
                        if !declared {
                            let Some(v) = g.counter_value(idx) else {
                                continue;
                            };
                            if v == 0 {
                                continue;
                            }
                        }
                        let idx64 = idx as u64;
                        accum.counters = identity_fold(accum.counters, &metric_id.to_le_bytes());
                        accum.counters = identity_fold(accum.counters, &idx64.to_le_bytes());
                        g.with_metadata(idx, &mut |m| {
                            accum.counters = identity_fold_metadata_presence(accum.counters, m);
                        });
                    }
                }
            }
            Value::GaugeGroup(g) => {
                if reader_stamped {
                    idx_scratch.clear();
                    g.for_each_metadata(&mut |idx, _| idx_scratch.push(idx));
                    idx_scratch.sort_unstable();
                    for &idx in idx_scratch.iter() {
                        let idx64 = idx as u64;
                        g.with_metadata(idx, &mut |m| {
                            let mut h = identity_fold(accum.gauges, &metric_id.to_le_bytes());
                            h = identity_fold(h, &idx64.to_le_bytes());
                            h = identity_fold_metadata_presence(h, m);
                            accum.gauges = h;
                        });
                    }
                } else {
                    for idx in members(member_set, member_bound, g.entries()) {
                        if !declared && g.gauge_value(idx).is_none() {
                            continue;
                        }
                        let idx64 = idx as u64;
                        accum.gauges = identity_fold(accum.gauges, &metric_id.to_le_bytes());
                        accum.gauges = identity_fold(accum.gauges, &idx64.to_le_bytes());
                        g.with_metadata(idx, &mut |m| {
                            accum.gauges = identity_fold_metadata_presence(accum.gauges, m);
                        });
                    }
                }
            }
            // Declared: registration membership, always a member (see
            // create_v3's declared Histogram arm). Default: membership by
            // presence — only a member once it has loaded a value. Note:
            // `h.load()` here materializes and allocates the histogram's
            // bucket snapshot just to check `.is_some()` — wasted on the
            // `declared` side (short-circuited by `||`, never called) but
            // NOT on the default side. Every histogram in this codebase is
            // declared today (grep confirms it), so this is a dead cost in
            // practice, not a live one — but a genuinely undeclared
            // histogram falling back to this arm would pay a real
            // allocation per tick here, on both hit and miss ticks (this
            // pass always runs). Left as-is rather than adding a
            // presence-only check metriken doesn't expose, since it isn't
            // costing anything today.
            Value::Histogram(h) if declared || h.load().is_some() => {
                accum.histograms = identity_fold(accum.histograms, &metric_id.to_le_bytes());
            }
            _ => {}
        }
    }

    accums
        .into_iter()
        .map(|(group_key, accum)| {
            let identity = accum.finish();
            // The cache itself is still keyed by the wire-format
            // "{sampler}/{name}" `String` (it persists ACROSS ticks, so it
            // must own its keys regardless) — this `format!` runs once per
            // DISTINCT GROUP here (bounded by declared-group count, ~tens),
            // not once per registry entry, so it's not the cost this pass
            // exists to avoid.
            let cache_key = format!("{}/{}", group_key.0, group_key.1);
            let needs_schema = match cache.entries.get(&cache_key) {
                Some(cached) => cached.identity != identity,
                None => true,
            };
            (group_key, GroupDecision { needs_schema })
        })
        .collect()
}

/// Per-group accumulation while walking the metriken registry: the group's
/// acquisition window (read once, at first touch — see `create_v3`) plus,
/// per kind, this tick's values (always collected) and descriptors (only
/// collected when `needs_schema` — see `GroupDecision`).
///
/// `reader_guard` is only ever `Some` for a
/// [reader-stamped](AcquisitionGroup::is_reader_stamped) group (a
/// `PackedCounters` mmap-direct group): its acquisition IS this walk's read
/// of the group's members, so first touch (the `Entry::Vacant` arm below)
/// acquires the bracket directly instead of reading a window a sampler
/// already stamped, and the group-emit loop `finish()`es it once every
/// member's value has been read — see the doc comment there.
///
/// `walk_identity` is folded ALONGSIDE `counter_descs`/`gauge_descs`/
/// `histogram_descs` on the MISS path only (see `create_v3`'s `if
/// group.needs_schema` arm) — the same `identity_fold`/`identity_fold_metadata`
/// calls `fold_group_identities` makes, applied to what THIS walk actually
/// pushes into the schema rather than to the pre-pass's own read. Finalize
/// stores `walk_identity.finish()`, not the pre-pass's identity, as the
/// cached identity for a rebuilt group — see the "miss-tick cache
/// poisoning" note on `fold_group_identities` for why the two can
/// legitimately differ and why storing the pre-pass's would be wrong. Left
/// at its `Default` (unfolded) on a hit — nothing needs it there, since a
/// hit reuses the cached identity verbatim.
///
/// `Default::default()` sets `needs_schema: true` (a safe "always rebuild"
/// default): the external-metrics block relies on it via
/// `HashMap::or_default`, and every other call site sets `needs_schema`
/// explicitly from `fold_group_identities`'s decision at first touch.
struct GroupBuilder {
    window: Option<Window>,
    reader_guard: Option<AcquisitionGuard<'static>>,
    needs_schema: bool,
    walk_identity: GroupIdentityAccum,
    counter_descs: Vec<MetricDesc>,
    counter_values: Vec<Option<u64>>,
    gauge_descs: Vec<MetricDesc>,
    gauge_values: Vec<Option<i64>>,
    histogram_descs: Vec<MetricDesc>,
    histogram_values: Vec<Option<histogram::Histogram>>,
}

impl Default for GroupBuilder {
    fn default() -> Self {
        Self {
            window: None,
            reader_guard: None,
            needs_schema: true,
            walk_identity: GroupIdentityAccum::default(),
            counter_descs: Vec::new(),
            counter_values: Vec::new(),
            gauge_descs: Vec::new(),
            gauge_values: Vec::new(),
            histogram_descs: Vec::new(),
            histogram_values: Vec::new(),
        }
    }
}

/// The `(sampler, name) -> AcquisitionGroup` registry, built once. Sound to
/// cache for the process lifetime: `ACQUISITION_GROUPS` is a `linkme`
/// distributed slice, populated at link time before `main` runs and never
/// mutated afterward — every entry that will ever exist already does by the
/// time this first initializes.
fn group_registry() -> &'static HashMap<(&'static str, &'static str), &'static AcquisitionGroup> {
    static REGISTRY: OnceLock<HashMap<(&'static str, &'static str), &'static AcquisitionGroup>> =
        OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry: HashMap<(&'static str, &'static str), &'static AcquisitionGroup> =
            HashMap::new();
        for group in crate::agent::samplers::ACQUISITION_GROUPS {
            // Keyed by `(sampler, name)` directly — the SAME parts
            // `AcquisitionGroup` itself stores, both already `&'static
            // str` — rather than a joined `"{sampler}/{name}"` `String`,
            // so building this map costs no allocation, and neither does
            // a lookup against it (a `(&str, &str)` tuple, not a freshly
            // `format!`ed `String`) — see `create`'s `group_windows` for
            // the same pattern, which this now matches instead of being
            // the one per-tick format! holdout.
            let key = (group.sampler, group.name);
            let prev = registry.insert(key, group);
            debug_assert!(
                prev.is_none(),
                "duplicate acquisition-group registry key `{}/{}` — every registered group's \
                 (sampler, name) pair must stay globally unique, including on non-Linux builds, \
                 where `samplers::bpf_sampler_name` collapses every BPF sampler's \
                 `attribute_sampler` resolution to the shared \"unattributed\" bucket (stats.rs \
                 is `include!`d there for metric-identity continuity, with no matching \
                 SamplerEntry). Qualify the group's `name` with its sampler, e.g. \
                 `<sampler>_<shortname>` — see the naming rule documented on \
                 `samplers::ACQUISITION_GROUPS`.",
                group.sampler,
                group.name,
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
///
/// # A third membership mode: reader-stamped (metadata-presence)
///
/// [`AcquisitionGroup::is_reader_stamped`] groups (mmap-direct
/// `PackedCounters` — cgroup and task counters) use neither the unbounded
/// `0..entries()` walk nor `member_bound`'s dense prefix. Membership is
/// `load_metadata(idx).is_some()`: an index the sampler's ringbuf handler
/// has registered metadata for (a live cgroup, a live task) is a member,
/// walked via `metadata_snapshot()` rather than a `0..N` loop — see the
/// CounterGroup/GaugeGroup match arms below.
///
/// **Walk-cost grounding** (docs/superpowers/plans/2026-08-19-stage3c-wave2.md
/// Part A asks this explicitly): does today's declared-group walk already
/// scan `0..entries()` for these groups, and is there a metriken iterator
/// over populated entries rather than backing-array capacity? Checked
/// against the pinned metriken rev
/// (`f601f48cffcfe27d2acc835bf05c90d0e481d1f7`, `metriken/src/group/{counter,metadata}.rs`):
/// yes to both. Before this mode existed, a packed/sparse `CounterGroup`
/// declared metric with no `member_bound` fell through to the unbounded
/// branch above — `0..g.entries()`, i.e. all 4,194,304 `MAX_PID` slots for
/// `task_cpu_usage`, every tick, in the V3 walk (V2's `create()` still does
/// this — see its own doc comment). `metadata_snapshot()`
/// (`CounterGroupMetric`/`GaugeGroupMetric` trait method, implemented by
/// `GroupMetadata::snapshot()`) is backed by a
/// `parking_lot::RwLock<HashMap<usize, HashMap<String, String>>>` holding
/// only populated indices — its cost is O(live population), the same
/// asymptotic class as `member_bound`'s dense-prefix walk, not a regression
/// against it. It IS a regression against the near-zero cost of an empty
/// group (a fresh `RwLock<HashMap>` clone-and-collect even at zero
/// population isn't free), but that trades a bounded, population-scaled
/// cost for what was previously an unconditional 4.2M-iteration sweep — a
/// net win, not a wash.
fn create_v3(
    timestamp: SystemTime,
    duration: Duration,
    external_metrics: Vec<ExternalMetric>,
    cache: &mut SkeletonCache,
) -> Snapshot {
    // See BUILDER_TEST_LOCK: serialise builder calls under `cargo test` so the
    // shared reader-stamped slots keep their single-writer invariant.
    #[cfg(test)]
    let _serialize = BUILDER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let sampler_mods = crate::agent::samplers::sampler_modules();
    let group_registry = group_registry();

    // Pre-pass: decide, per group, hit or miss — see `fold_group_identities`
    // and the `SkeletonCache` doc comment. Everything below this point
    // either assembles a schema (miss) or skips straight to values (hit)
    // based on this map; it never re-derives the decision itself.
    let group_decisions = fold_group_identities(cache, group_registry, &sampler_mods);

    let mut groups: HashMap<(&str, &str), GroupBuilder> = HashMap::new();
    // Reused across every reader-stamped group's sparse membership walk
    // below (both the schema-building and values-only arms) — cleared
    // per group, never reallocated once it reaches its high-water mark, so
    // this one scratch buffer is the only heap cost the sparse-membership
    // walk pays across the whole tick, not one allocation per group.
    let mut idx_scratch: Vec<usize> = Vec::new();

    for (metric_id, metric) in metriken::metrics().iter().enumerate() {
        let Some(value) = metric.value() else {
            continue;
        };

        let name = metric.name();

        if name.starts_with("log_") {
            continue;
        }

        // Route: a declared `acq_group` wins only if it actually resolves
        // against the registry; otherwise fall back to the sampler's
        // default group (and flag the mismatch in debug builds — see the
        // function-level doc comment). Mirrors `fold_group_identities`'
        // routing (KEEP IN SYNC) — deliberately NOT calling `metric_metadata`
        // here: that builds a `BTreeMap` this walk may not need at all (a
        // cache hit needs no metadata whatsoever), so it's deferred below,
        // behind the `needs_schema` check. Also mirrors `create` (V2)'s
        // `group_windows` lookup: `(&str, &str)` tuple keys throughout, not
        // a `format!`ed `String` per registry entry — see
        // `fold_group_identities`'s matching comment for the full
        // rationale.
        let sampler = crate::agent::samplers::attribute_sampler(metric.module(), &sampler_mods);
        let mut declared = false;
        // The member-population bound for a declared, group-typed metric
        // (`CounterGroup`/`GaugeGroup`): `Some(n)` walks `0..n` instead of
        // the full backing-array `entries()`. Resolved alongside routing,
        // before `group_key` is moved into `groups.entry` below.
        let mut member_bound: Option<usize> = None;
        // Members that are not a dense prefix; see `members`.
        let mut member_set: Option<&[usize]> = None;
        // Reader-stamped (`PackedCounters` mmap-direct) groups use a THIRD
        // membership mode instead of `member_bound`'s dense prefix: every
        // index with metadata registered is a member — see the
        // CounterGroup/GaugeGroup arms below and the doc comment on
        // `create_v3` for the walk-cost grounding.
        let mut reader_stamped = false;
        let group_key: (&str, &str) = match metric.metadata().get("acq_group") {
            Some(acq_group) => {
                if let Some(ag) = group_registry.get(&(sampler, acq_group)) {
                    declared = true;
                    member_bound = ag.member_bound();
                    member_set = ag.member_set();
                    reader_stamped = ag.is_reader_stamped();
                    (ag.sampler, ag.name)
                } else {
                    debug_assert!(
                        false,
                        "metric `{name}` declares acq_group=\"{acq_group}\" for sampler \
                         `{sampler}`, but no AcquisitionGroup (\"{sampler}\", \
                         \"{acq_group}\") is registered on ACQUISITION_GROUPS; routing to \
                         the default group instead",
                    );
                    (sampler, "main")
                }
            }
            None => (sampler, "main"),
        };

        let group = match groups.entry(group_key) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                // First touch for this group THIS TICK.
                //
                // Reader-stamped groups (`PackedCounters` mmap-direct — see
                // `AcquisitionGroup::is_reader_stamped`): this walk's read of
                // the group's members IS the acquisition, so acquire the
                // bracket right here, at first touch, instead of reading a
                // window some sampler already stamped. There is nothing to
                // read yet — `window` stays `None` until the group-emit loop
                // below calls `guard.finish()` to PUBLISH. The published
                // WIDTH, though, is decided earlier: the CounterGroup/
                // GaugeGroup arms call `guard.mark_end()` immediately after
                // each touching metric's member-value loop, so the window's
                // end reflects when this group's own values were actually
                // read (µs-scale), not when `finish()` happens to run after
                // the rest of the tick's walk — see
                // `AcquisitionGuard::mark_end`. Unlike the sampler-stamped
                // case, there is no racing writer to guard against: the
                // ONLY writer for a reader-stamped group's slot is this very
                // walk (see `AcquisitionGroup::set_reader_stamped`'s doc
                // comment on the single-writer contract), so this
                // acquire()/mark_end()/finish() sequence is honest, not an
                // approximation.
                //
                // Sampler-stamped groups keep the original discipline: read
                // the window now, before any of this group's members'
                // values accumulate below — not once per member (redundant
                // seqlock loads, and walk-order-dependent) and not deferred
                // solely to emit time (see `resolve_walk_window`) (unsafe:
                // the walk over the FULL registry, across every group, can
                // take long enough that a concurrent sampler tick completes
                // a whole new acquire()/finish() cycle in the meantime,
                // which would pair window(N+1) with the values(N) already
                // read here — a confident, wrong claim that this data is
                // newer than it actually is). Read before values, mirroring
                // timing.rs's stamp-last rule from the read side: a stale
                // window paired with fresh-enough values only under-claims
                // freshness, which is the safe direction — same "can only
                // lag, never lead" guarantee `AcquisitionGuard` gives
                // writers, applied here to the reader.
                let ag = group_registry.get(e.key());
                let (window, reader_guard) = match ag {
                    Some(ag) if ag.is_reader_stamped() => (None, Some(ag.acquire())),
                    Some(ag) => (ag.window(), None),
                    None => (None, None),
                };

                let needs_schema = group_decisions
                    .get(e.key())
                    .map(|d| d.needs_schema)
                    .unwrap_or(true);

                let mut builder = GroupBuilder {
                    window,
                    reader_guard,
                    needs_schema,
                    ..Default::default()
                };

                // Cache hit: pre-size this tick's value vectors from the
                // cached schema's member counts, so pushing values below
                // never reallocates. The only heap allocations a hit group
                // pays for are these — at most three `Vec::with_capacity`
                // calls, one per kind, not one per member. `cache.entries`
                // is String-keyed (it persists across ticks, so it must own
                // its keys) — this `format!` runs once per DISTINCT GROUP
                // at first touch, not once per registry entry, so it isn't
                // the cost `fold_group_identities`'s routing avoids.
                if !needs_schema {
                    let (sampler, name) = *e.key();
                    if let Some(cached) = cache.entries.get(&format!("{sampler}/{name}")) {
                        builder.counter_values = Vec::with_capacity(cached.schema.counters.len());
                        builder.gauge_values = Vec::with_capacity(cached.schema.gauges.len());
                        builder.histogram_values =
                            Vec::with_capacity(cached.schema.histograms.len());
                    }
                }

                e.insert(builder)
            }
        };

        if group.needs_schema {
            // MISS path: assemble the schema exactly as before — full
            // metadata, formatted member names, everything a receiver needs
            // to parse this group's values.
            //
            // KEEP IN SYNC with create AND fold_group_identities — see the
            // doc comment on `metric_metadata` (the shared prefix) and
            // below (the per-kind walk/naming/membership rules, which are
            // NOT shared and are duplicated independently in each of these
            // three places).
            let (mut metadata, _) = metric_metadata(metric, &sampler_mods);

            // Strip unconditionally, not just when `declared`. On the
            // declared path it is redundant with the group's own name
            // (`GroupSnapshot.name`, `"{sampler}/{acq_group}"`) — left in
            // place it would be one more copy of the same key/value pair
            // repeated in every member's `MetricDesc.metadata`, thousands of
            // identical wasted copies for a large declared group (tasks/
            // cgroups/CPUs). On the unmatched-registry fallback path (the
            // `debug_assert!` arm above — a typo'd or renamed group), the
            // metric still carries its stale `acq_group` value even though
            // it just got routed to the DEFAULT group instead; leaving it in
            // would leak that value as a phantom label on release builds,
            // where the `debug_assert!` compiles away and this fallback runs
            // silently instead of panicking. A metric with no `acq_group`
            // tag at all has nothing to remove, so this is a no-op there.
            metadata.remove("acq_group");

            let entry_name = format!("{metric_id}");
            // Reused below wherever a member's identity is folded into
            // `group.walk_identity` — see fix (a) on the `SkeletonCache`
            // doc comment / `fold_group_identities`'s "miss-tick cache
            // poisoning" note for why this walk folds its OWN identity
            // instead of trusting the pre-pass's.
            let metric_id_u64 = metric_id as u64;

            match value {
                Value::Counter(v) => {
                    group.walk_identity.counters =
                        identity_fold(group.walk_identity.counters, &metric_id_u64.to_le_bytes());
                    group.counter_descs.push(MetricDesc {
                        name: entry_name,
                        metadata,
                    });
                    group.counter_values.push(Some(v));
                }
                Value::Gauge(v) => {
                    group.walk_identity.gauges =
                        identity_fold(group.walk_identity.gauges, &metric_id_u64.to_le_bytes());
                    group.gauge_descs.push(MetricDesc {
                        name: entry_name,
                        metadata,
                    });
                    group.gauge_values.push(Some(v));
                }
                Value::CounterGroup(g) => {
                    if reader_stamped {
                        // Reader-stamped (mmap-direct `PackedCounters`)
                        // group: membership is metadata-presence, not
                        // `0..bound` — a registered index (a live cgroup/
                        // task the sampler's ringbuf handler attached
                        // metadata to) is a member, full stop, regardless of
                        // the backing array's capacity (`MAX_CGROUPS`/
                        // `MAX_PID`, sized for the worst case — see
                        // docs/principles.md principle 6). Walking
                        // `0..entries()` here would mean sweeping all 4.2M
                        // `MAX_PID` slots every tick for `task_cpu_usage`
                        // regardless of how many tasks are actually live;
                        // see the walk-cost grounding on `create_v3`'s doc
                        // comment. `for_each_metadata`'s cost is
                        // O(populated), not O(entries()) — it walks
                        // metriken's own sparse `HashMap<usize, _>` metadata
                        // store, not the dense value array, and — unlike
                        // `metadata_snapshot()` — borrows each entry instead
                        // of cloning it.
                        //
                        // Iteration order is NOT stable tick-to-tick on its
                        // own (hashbrown gives no ordering guarantee) even
                        // when the populated set is unchanged — collect
                        // indices only (no metadata read yet) and sort so a
                        // stable member set produces a byte-stable schema
                        // order (and therefore an identity match) across
                        // ticks, the same determinism concern the external-
                        // metrics sort below addresses for a different
                        // source.
                        idx_scratch.clear();
                        g.for_each_metadata(&mut |idx, _| idx_scratch.push(idx));
                        idx_scratch.sort_unstable();
                        for &idx in idx_scratch.iter() {
                            // Same torn-recycle caveat as the non-reader-
                            // stamped arm below — see its comment: the value
                            // read and the metadata read are not atomic.
                            let v = g.counter_value(idx);
                            let idx64 = idx as u64;
                            g.with_metadata(idx, &mut |m| {
                                // Fold this member's identity from what
                                // THIS walk observed — same
                                // identity_fold/identity_fold_metadata
                                // calls, same argument order, as
                                // fold_group_identities' matching arm, so a
                                // later tick's pre-pass can reproduce this
                                // exact value on a genuine hit.
                                group.walk_identity.counters = identity_fold(
                                    group.walk_identity.counters,
                                    &metric_id_u64.to_le_bytes(),
                                );
                                group.walk_identity.counters = identity_fold(
                                    group.walk_identity.counters,
                                    &idx64.to_le_bytes(),
                                );
                                // See identity_fold_metadata_presence's doc
                                // comment for why the presence flag is
                                // folded regardless of Some/None.
                                group.walk_identity.counters = identity_fold_metadata_presence(
                                    group.walk_identity.counters,
                                    m,
                                );

                                let mut entry_metadata = metadata.clone();
                                entry_metadata.insert("id".to_string(), idx.to_string());
                                if let Some(m) = m {
                                    for (k, v) in m {
                                        entry_metadata.insert(k.clone(), v.clone());
                                    }
                                }
                                group.counter_descs.push(MetricDesc {
                                    name: format!("{metric_id}x{idx}"),
                                    metadata: entry_metadata,
                                });
                            });
                            group.counter_values.push(v);
                        }
                        // Mark the end HERE — right after this metric's
                        // member values were actually read — not at emit
                        // time, when `finish()` runs below after the rest of
                        // the walk (every other group's schema assembly,
                        // hashing, etc.) has also happened. See
                        // `AcquisitionGuard::mark_end`. A group with several
                        // like-entity members (e.g. `cgroup_syscall`'s 16
                        // op-class maps) touches this arm once per member;
                        // each call moves the mark forward, so the LAST
                        // touch — this group's true last member read — is
                        // what ends up published, exactly like `finish()`'s
                        // original stamp-last derivation, just decoupled
                        // from publish timing.
                        if let Some(guard) = group.reader_guard.as_mut() {
                            guard.mark_end();
                        }
                    } else {
                        // Registration membership for a per-CPU (or similar)
                        // group IS the group's real member population —
                        // `possible_cpus()` for a `CpuCounters`-backed group
                        // — not the backing array's `entries()` capacity,
                        // which is a fixed implementation ceiling
                        // (`MAX_CPUS`; see docs/principles.md principle 6,
                        // "over-allocates on small machines") sized for the
                        // worst case, not this host. Walking the full
                        // capacity on every declared group would put an
                        // ~18-CPU host's tick at ~19× the entries it
                        // actually populated; walk the bound instead when
                        // one is set (clamped to `entries()` in case a
                        // stale/misconfigured bound somehow exceeds the
                        // backing array).
                        for idx in members(member_set, member_bound, g.entries()) {
                            let v = g.counter_value(idx);

                            // Transitional V2-style sentinel skip — default
                            // groups only. See doc comment.
                            if !declared {
                                let Some(v) = v else { continue };
                                if v == 0 {
                                    continue;
                                }
                            }

                            // `counter_value(idx)` above and the metadata
                            // read below are two SEPARATE reads, not one
                            // atomic pair — unlike `AcquisitionGroup`'s
                            // window (a seqlock), a group entry's value and
                            // its metadata have no shared lock. A slot
                            // recycled by a concurrent writer between these
                            // two reads (e.g. a pid/cgroup id reused
                            // mid-tick) can pair the NEW occupant's value
                            // with the OLD occupant's labels, or vice versa,
                            // for that one tick. This matches V2's
                            // `create()`, which has the identical two-step
                            // read here — not a regression introduced by
                            // V3. Measured under a deliberate
                            // concurrent-recycle hammer: ~2-3% of ticks
                            // torn; in production today it's effectively
                            // zero, because sampler writes complete
                            // synchronously inside `refresh()` rather than
                            // racing the snapshot builder from another task.
                            // Migration note: a sampler that calls
                            // `insert_metadata` more than once per slot per
                            // refresh (cpu usage does 4) should move to a
                            // single atomic metadata update (`set_metadata`,
                            // one call) when it migrates to a declared
                            // group, to close this window rather than just
                            // narrow it.
                            let mut entry_metadata = metadata.clone();
                            entry_metadata.insert("id".to_string(), idx.to_string());
                            let idx64 = idx as u64;
                            g.with_metadata(idx, &mut |m| {
                                // See the reader-stamped arm above for why
                                // this folds the SAME bytes, same order.
                                group.walk_identity.counters = identity_fold(
                                    group.walk_identity.counters,
                                    &metric_id_u64.to_le_bytes(),
                                );
                                group.walk_identity.counters = identity_fold(
                                    group.walk_identity.counters,
                                    &idx64.to_le_bytes(),
                                );
                                group.walk_identity.counters = identity_fold_metadata_presence(
                                    group.walk_identity.counters,
                                    m,
                                );
                                if let Some(m) = m {
                                    for (k, v) in m {
                                        entry_metadata.insert(k.clone(), v.clone());
                                    }
                                }
                            });

                            group.counter_descs.push(MetricDesc {
                                name: format!("{metric_id}x{idx}"),
                                metadata: entry_metadata,
                            });
                            group.counter_values.push(v);
                        }
                    }
                }
                Value::GaugeGroup(g) => {
                    if reader_stamped {
                        // See the identical branch on the CounterGroup arm
                        // above for the full rationale (walk-cost grounding,
                        // and why the sort is required for schema
                        // stability). No `PackedCounters`-style gauge group
                        // exists in the codebase yet, but this keeps the
                        // declared-group membership rule symmetric across
                        // both group kinds rather than leaving a silent gap
                        // for the first one that does.
                        idx_scratch.clear();
                        g.for_each_metadata(&mut |idx, _| idx_scratch.push(idx));
                        idx_scratch.sort_unstable();
                        for &idx in idx_scratch.iter() {
                            let v = g.gauge_value(idx);
                            let idx64 = idx as u64;
                            g.with_metadata(idx, &mut |m| {
                                group.walk_identity.gauges = identity_fold(
                                    group.walk_identity.gauges,
                                    &metric_id_u64.to_le_bytes(),
                                );
                                group.walk_identity.gauges =
                                    identity_fold(group.walk_identity.gauges, &idx64.to_le_bytes());
                                group.walk_identity.gauges =
                                    identity_fold_metadata_presence(group.walk_identity.gauges, m);

                                let mut entry_metadata = metadata.clone();
                                entry_metadata.insert("id".to_string(), idx.to_string());
                                if let Some(m) = m {
                                    for (k, v) in m {
                                        entry_metadata.insert(k.clone(), v.clone());
                                    }
                                }
                                group.gauge_descs.push(MetricDesc {
                                    name: format!("{metric_id}x{idx}"),
                                    metadata: entry_metadata,
                                });
                            });
                            group.gauge_values.push(v);
                        }
                        // See the CounterGroup arm above: mark the end right
                        // after THIS metric's member values were read, not
                        // at emit time.
                        if let Some(guard) = group.reader_guard.as_mut() {
                            guard.mark_end();
                        }
                    } else {
                        // Same member-population bound as the `CounterGroup`
                        // arm above — see its comment.
                        for idx in members(member_set, member_bound, g.entries()) {
                            let v = g.gauge_value(idx);

                            // Transitional V2-style sentinel skip — default
                            // groups only. See doc comment. Unlike
                            // CounterGroup's `== 0` (still live above: 0 is
                            // a legitimate initialized-but-untouched counter
                            // value, indistinguishable from an explicit 0),
                            // there is no `== i64::MIN` check here:
                            // `GaugeGroup::gauge_value` already maps its
                            // internal never-set sentinel to `None` before
                            // this ever sees it (metriken owns that
                            // mapping), so `Some(i64::MIN)` cannot occur —
                            // an explicit re-check here would be dead code.
                            if !declared && v.is_none() {
                                continue;
                            }

                            // Separate value/metadata reads, same
                            // torn-recycle caveat as the CounterGroup arm
                            // above.
                            let mut entry_metadata = metadata.clone();
                            entry_metadata.insert("id".to_string(), idx.to_string());
                            let idx64 = idx as u64;
                            g.with_metadata(idx, &mut |m| {
                                group.walk_identity.gauges = identity_fold(
                                    group.walk_identity.gauges,
                                    &metric_id_u64.to_le_bytes(),
                                );
                                group.walk_identity.gauges =
                                    identity_fold(group.walk_identity.gauges, &idx64.to_le_bytes());
                                group.walk_identity.gauges =
                                    identity_fold_metadata_presence(group.walk_identity.gauges, m);
                                if let Some(m) = m {
                                    for (k, v) in m {
                                        entry_metadata.insert(k.clone(), v.clone());
                                    }
                                }
                            });

                            group.gauge_descs.push(MetricDesc {
                                name: format!("{metric_id}x{idx}"),
                                metadata: entry_metadata,
                            });
                            group.gauge_values.push(v);
                        }
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
                    // Same membership test as fold_group_identities'
                    // matching arm (declared || h.load().is_some()) — fold
                    // only when this member is actually about to be pushed
                    // below.
                    if declared || hv.is_some() {
                        group.walk_identity.histograms = identity_fold(
                            group.walk_identity.histograms,
                            &metric_id_u64.to_le_bytes(),
                        );
                    }
                    if declared {
                        // Registration membership: this metric IS the
                        // member, full stop — `None` means "registered but
                        // no reading yet" (e.g. before its BPF map
                        // attaches), not "not a member". Omitting it here
                        // would make membership value-derived on the
                        // declared path, churning the schema hash on
                        // exactly the transient event (a histogram that
                        // hasn't loaded yet) the design commits to NOT
                        // treating as a membership change.
                        group.histogram_descs.push(desc);
                        group.histogram_values.push(hv);
                    } else if let Some(hv) = hv {
                        // Default path: unchanged V2-style membership-by-
                        // presence — an unloaded histogram isn't a member at
                        // all.
                        group.histogram_descs.push(desc);
                        group.histogram_values.push(Some(hv));
                    }
                }
                _ => {}
            }
        } else {
            // HIT path: identity unchanged since last tick (see
            // `fold_group_identities`) — read this tick's values, in the
            // SAME order and under the SAME membership rules the schema
            // arms above use, but build no `MetricDesc`, no `BTreeMap`, no
            // formatted member name. The cached `Arc<GroupSchema>` is
            // reused verbatim at emit time below.
            match value {
                Value::Counter(v) => group.counter_values.push(Some(v)),
                Value::Gauge(v) => group.gauge_values.push(Some(v)),
                Value::CounterGroup(g) => {
                    if reader_stamped {
                        idx_scratch.clear();
                        g.for_each_metadata(&mut |idx, _| idx_scratch.push(idx));
                        idx_scratch.sort_unstable();
                        for &idx in idx_scratch.iter() {
                            group.counter_values.push(g.counter_value(idx));
                        }
                        if let Some(guard) = group.reader_guard.as_mut() {
                            guard.mark_end();
                        }
                    } else {
                        for idx in members(member_set, member_bound, g.entries()) {
                            let v = g.counter_value(idx);
                            if !declared {
                                let Some(v) = v else { continue };
                                if v == 0 {
                                    continue;
                                }
                            }
                            group.counter_values.push(v);
                        }
                    }
                }
                Value::GaugeGroup(g) => {
                    if reader_stamped {
                        idx_scratch.clear();
                        g.for_each_metadata(&mut |idx, _| idx_scratch.push(idx));
                        idx_scratch.sort_unstable();
                        for &idx in idx_scratch.iter() {
                            group.gauge_values.push(g.gauge_value(idx));
                        }
                        if let Some(guard) = group.reader_guard.as_mut() {
                            guard.mark_end();
                        }
                    } else {
                        for idx in members(member_set, member_bound, g.entries()) {
                            let v = g.gauge_value(idx);
                            if !declared && v.is_none() {
                                continue;
                            }
                            group.gauge_values.push(v);
                        }
                    }
                }
                Value::Histogram(h) => {
                    let hv = h.load();
                    if declared || hv.is_some() {
                        group.histogram_values.push(hv);
                    }
                }
                _ => {}
            }
        }
    }

    // External metrics: one windowless group, own naming scheme (they are
    // not metriken registry entries, so there is no metric_id to key on).
    // Always schema-built — see `fold_group_identities`'s doc comment for
    // why the identity fold skips this group (and so `GroupBuilder::default`
    // always has `needs_schema: true`, matched here unconditionally).
    if !external_metrics.is_empty() {
        let group = groups.entry(("external", "main")).or_default();
        debug_assert!(
            group.needs_schema,
            "\"external/main\" must always be a fresh GroupBuilder::default() (needs_schema: \
             true) — fold_group_identities never produces a decision for it, so if this ever \
             fires, something inserted an entry ahead of this block and left needs_schema \
             false, which would wrongly skip building its schema below",
        );

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
                    group.counter_descs.push(MetricDesc {
                        name: entry_name,
                        metadata,
                    });
                    group.counter_values.push(Some(v));
                }
                ExternalMetricValue::Gauge(v) => {
                    group.gauge_descs.push(MetricDesc {
                        name: entry_name,
                        metadata,
                    });
                    group.gauge_values.push(Some(v));
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
                        group.histogram_descs.push(MetricDesc {
                            name: entry_name,
                            metadata,
                        });
                        group.histogram_values.push(Some(hv));
                    }
                }
            }
        }
    }

    let mut group_snapshots: Vec<GroupSnapshot> = Vec::with_capacity(groups.len());

    for (group_key, group) in groups {
        // A metric routes to (and so creates) a `GroupBuilder` before its
        // `Value` is matched above, so a metric whose value kind isn't one
        // `create_v3` knows how to expose (falls into the `_ => {}` arm —
        // e.g. a `HistogramGroup`-typed metric, a gap V2's `create` shares)
        // can leave a group with nothing ever pushed into it. An
        // empty-schema `GroupSnapshot` carries no information a receiver
        // can use and would otherwise be hashed and transmitted every
        // tick for nothing — and it contradicts this function's own doc
        // comment, which says a group nothing routes to is absent. Skip it
        // entirely rather than emit a zero-member group. Checked against
        // the VALUE vectors, not the desc vectors — a hit group's desc
        // vectors are always empty by design (see the HIT arm above), so
        // checking those here would wrongly drop every hit group.
        //
        // For a reader-stamped group this `continue` also drops
        // `group.reader_guard` WITHOUT calling `finish()` — an explicit
        // guard-discard, not an oversight: the same "no `finish()` on a
        // read that produced nothing" discipline `AcquisitionGuard`
        // documents for an ordinary failed read (see its doc comment).
        // The group's window slot keeps whatever it held before; nothing
        // was actually read this tick, so there is nothing honest to
        // publish. In practice this path is reachable only in a
        // synthetic/test registry — a real reader-stamped group only
        // creates a `GroupBuilder` when some metric routed to it (the
        // `Entry::Vacant` arm above), and that same metric's
        // CounterGroup/GaugeGroup arm always pushes SOMETHING (an entry
        // per registered/populated index) or the metric wasn't a member at
        // all — so an empty reader-stamped group here would mean a routed
        // metric matched no `Value` arm `create_v3` knows how to expose.
        if group.counter_values.is_empty()
            && group.gauge_values.is_empty()
            && group.histogram_values.is_empty()
        {
            continue;
        }

        // Wire-format name, built once per DISTINCT GROUP here (bounded by
        // declared-group count, ~tens) — not once per registry entry (the
        // `format!` this loop used to pay for at ROUTING time, before the
        // `(&str, &str)` tuple-keyed `groups`/`group_registry` change).
        let group_name = format!("{}/{}", group_key.0, group_key.1);

        // Reader-stamped groups (`PackedCounters` mmap-direct): `finish()`
        // PUBLISHES the bracket acquired at first touch — stamp-last, same
        // rule `AcquisitionGuard` enforces for sampler-stamped groups (see
        // its doc comment), applied here to the reader instead of a
        // sampler. The published WIDTH was already decided earlier, by the
        // last `mark_end()` call the CounterGroup/GaugeGroup arms made for
        // this group (immediately after each touching metric's member-
        // value loop, above) — `finish()` here only decides WHEN the slot
        // becomes visible, not what it contains. No `resolve_walk_window`
        // reconciliation is needed: the ONLY writer for a reader-stamped
        // group's slot is this walk itself (see
        // `AcquisitionGroup::set_reader_stamped`'s single-writer note), so
        // this acquire()/mark_end()/finish() sequence is the complete, sole
        // write for the tick — there is no concurrent background sampler
        // that could have re-stamped it mid-walk, unlike the sampler-
        // stamped case `resolve_walk_window` guards against.
        let window = if let Some(guard) = group.reader_guard {
            guard.finish();
            group_registry.get(&group_key).and_then(|ag| ag.window())
        } else {
            // Re-read the window here (after this group's values, above)
            // and reconcile with the first-touch read via
            // `resolve_walk_window` — see its doc comment for why a second
            // read is necessary.
            let latest_window = group_registry.get(&group_key).and_then(|ag| ag.window());
            resolve_walk_window(group.window, latest_window)
        };

        let (schema, hash) = if group.needs_schema {
            let schema = GroupSchema {
                counters: group.counter_descs,
                gauges: group.gauge_descs,
                histograms: group.histogram_descs,
            };
            let hash = schema.hash();
            let schema = Arc::new(schema);
            // Fix for miss-tick cache poisoning: the identity STORED here
            // is folded from what THIS WALK actually collected
            // (`group.walk_identity`, folded alongside every desc pushed
            // above), NOT from `fold_group_identities`' pre-pass read. For
            // a default group, those two can legitimately disagree (its
            // membership is value-derived — see the "wider torn-read
            // window" note on `fold_group_identities`): the pre-pass might
            // trigger this rebuild based on a read that doesn't match what
            // the walk below actually saw. Storing the pre-pass's identity
            // anyway would bind THIS schema to an identity the walk didn't
            // produce — if membership later drifts back to what the
            // pre-pass saw, a future tick would wrongly HIT and ship this
            // schema against a values vector collected under a DIFFERENT
            // membership, forever (not self-correcting). Storing the
            // walk's own identity makes the invariant "stored identity
            // always describes the stored schema" hold unconditionally, so
            // only the bounded, self-correcting hit-tick race (handled
            // below) remains.
            let identity = group.walk_identity.finish();
            cache.entries.insert(
                group_name.clone(),
                GroupSkeleton {
                    identity,
                    schema: schema.clone(),
                    hash,
                },
            );
            cache.rebuilds += 1;
            (schema, hash)
        } else {
            // Hit path. `fold_group_identities`' pre-pass read a default
            // group's value-derived membership separately from this walk
            // (see its doc comment) — rarely, a value can cross the
            // membership boundary in between, so the pre-pass's "hit"
            // call and what THIS walk actually collected can disagree.
            // Verify arity against the cached schema before trusting it:
            // a mismatch means the cached schema does not actually
            // describe this tick's collected values. Evict the entry and
            // skip emitting this group for this tick rather than shipping
            // a payload every conformant `GroupSnapshot::validate()` call
            // would reject anyway — this is the self-correcting half of
            // the same race; the miss path above closes the
            // NON-self-correcting (permanent poisoning) half.
            match cache.entries.get(&group_name) {
                Some(cached)
                    if cached.schema.counters.len() == group.counter_values.len()
                        && cached.schema.gauges.len() == group.gauge_values.len()
                        && cached.schema.histograms.len() == group.histogram_values.len() =>
                {
                    (cached.schema.clone(), cached.hash)
                }
                Some(cached) => {
                    debug!(
                        "SkeletonCache arity mismatch on a hit tick for group `{group_name}` \
                         (cached schema counters={}/gauges={}/histograms={}, this tick's \
                         collected values counters={}/gauges={}/histograms={}) — evicting the \
                         stale entry and skipping this group for this tick",
                        cached.schema.counters.len(),
                        cached.schema.gauges.len(),
                        cached.schema.histograms.len(),
                        group.counter_values.len(),
                        group.gauge_values.len(),
                        group.histogram_values.len(),
                    );
                    // Note: for a reader-stamped group this `continue`
                    // runs after `guard.finish()` above, so the window is
                    // published for a tick whose group we then drop —
                    // unlike the empty-group skip, which deliberately
                    // discards its guard. Unreachable today: reader-stamped
                    // implies declared, whose membership is
                    // registration-derived and therefore identical in both
                    // passes, so this arm cannot be reached with a reader
                    // guard in hand. If a future membership rule breaks
                    // that implication, move the arity check above the
                    // window resolution.
                    cache.entries.remove(&group_name);
                    continue;
                }
                None => {
                    // Shouldn't happen (fold_group_identities only says
                    // needs_schema=false when a cache entry exists for
                    // this exact group), but defensive rather than a
                    // panic: skip this group for this tick.
                    debug!(
                        "SkeletonCache: group `{group_name}` marked needs_schema=false but has \
                         no cache entry — fold_group_identities and this loop disagree on \
                         cache state; skipping this group for this tick"
                    );
                    continue;
                }
            }
        };

        group_snapshots.push(GroupSnapshot {
            name: group_name,
            schema_hash: hash,
            schema: Some(schema),
            window,
            counters: group.counter_values,
            gauges: group.gauge_values,
            histograms: group.histogram_values,
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

    // --- allocation-counting global allocator -----------------------------
    //
    // Wraps `System`, delegating every call unchanged, and additionally
    // counts allocation/reallocation calls made while `ALLOC_ENABLED` is set
    // for the CURRENT thread. `cargo test` runs each test function on its
    // own OS thread by default, so thread-local counting isolates one
    // test's count from whatever every other concurrently running test in
    // this binary allocates — a process-global counter would be hopelessly
    // noisy under `cargo test`'s default parallelism.
    //
    // This has to be the crate's one and only `#[global_allocator]` (Rust
    // permits exactly one per binary); nothing else in this crate declares
    // one, and this module only compiles under `#[cfg(test)]`, so it's the
    // allocator for the whole test binary. Every other test's allocations
    // still go through it — they're just not counted, since none of them
    // ever set `ALLOC_ENABLED`.
    struct CountingAllocator;

    thread_local! {
        static ALLOC_ENABLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static ALLOC_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            if ALLOC_ENABLED.with(|e| e.get()) {
                ALLOC_COUNT.with(|c| c.set(c.get() + 1));
            }
            unsafe { std::alloc::System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
            unsafe { std::alloc::System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(
            &self,
            ptr: *mut u8,
            layout: std::alloc::Layout,
            new_size: usize,
        ) -> *mut u8 {
            if ALLOC_ENABLED.with(|e| e.get()) {
                ALLOC_COUNT.with(|c| c.set(c.get() + 1));
            }
            unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static COUNTING_ALLOCATOR: CountingAllocator = CountingAllocator;

    /// Run `f`, counting every heap allocation/reallocation `f` makes on
    /// THIS thread (nested nested calls included). Deallocations are not
    /// counted — the claim under test is "how much does a hit tick
    /// allocate", not "how much does it free".
    fn count_allocations<T>(f: impl FnOnce() -> T) -> (T, usize) {
        ALLOC_COUNT.with(|c| c.set(0));
        ALLOC_ENABLED.with(|e| e.set(true));
        let result = f();
        ALLOC_ENABLED.with(|e| e.set(false));
        let count = ALLOC_COUNT.with(|c| c.get());
        (result, count)
    }

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

    // --- V2 group-window regression fix ----------------------------------
    //
    // Wave 1 switched migrated metrics from `Windowed*` types to plain
    // `LazyCounter`/`CounterGroup` stamped by an `AcquisitionGroup`. `create`
    // (V2) used to read a per-metric window off `value_with_window`/
    // `load_with_window`, which just returns `None` for those types now —
    // V2 output silently lost acquisition windows for every migrated
    // sampler. These pin the fix: a declared, stamped group's window is
    // read off the group registry instead; a declared-but-never-stamped
    // group stays `None` (not a stale or fabricated window); an unmigrated
    // `Windowed*` metric's own per-metric window path is untouched.

    // Dedicated group + metric, touched by no other test (same rationale as
    // `V3_STABILITY_GROUP` above): a shared group could get stamped by
    // another test's `acquire()`/`finish()` before this one runs.
    static V2_GROUP_WINDOW_STAMPED_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "v2_group_window_stamped_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V2_GROUP_WINDOW_STAMPED_GROUP_ENTRY: &'static AcquisitionGroup =
        &V2_GROUP_WINDOW_STAMPED_GROUP;

    #[metric(
        name = "snapshot_v2_group_window_stamped_probe",
        metadata = { acq_group = "v2_group_window_stamped_probe" }
    )]
    static V2_GROUP_WINDOW_STAMPED_PROBE: metriken::Counter = metriken::Counter::new();

    #[test]
    fn v2_output_carries_the_group_window_for_a_stamped_declared_group_member() {
        V2_GROUP_WINDOW_STAMPED_PROBE.increment();
        let guard = V2_GROUP_WINDOW_STAMPED_GROUP.acquire();
        guard.finish();
        let group_window = V2_GROUP_WINDOW_STAMPED_GROUP
            .window()
            .expect("group was just stamped");

        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![]);
        let Snapshot::V2(s) = snap else {
            panic!("expected V2")
        };
        let c = s
            .counters
            .iter()
            .find(|c| {
                c.metadata.get("metric").map(String::as_str)
                    == Some("snapshot_v2_group_window_stamped_probe")
            })
            .expect("stamped probe counter present in V2 output");
        assert_eq!(
            c.window,
            Some(group_window),
            "V2 output carries the declared group's window, not None"
        );
    }

    // Dedicated group, never acquired/finished by any test.
    static V2_GROUP_WINDOW_UNSTAMPED_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "v2_group_window_unstamped_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V2_GROUP_WINDOW_UNSTAMPED_GROUP_ENTRY: &'static AcquisitionGroup =
        &V2_GROUP_WINDOW_UNSTAMPED_GROUP;

    #[metric(
        name = "snapshot_v2_group_window_unstamped_probe",
        metadata = { acq_group = "v2_group_window_unstamped_probe" }
    )]
    static V2_GROUP_WINDOW_UNSTAMPED_PROBE: metriken::Counter = metriken::Counter::new();

    #[test]
    fn v2_output_is_windowless_for_a_declared_but_unstamped_group() {
        V2_GROUP_WINDOW_UNSTAMPED_PROBE.increment();

        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![]);
        let Snapshot::V2(s) = snap else {
            panic!("expected V2")
        };
        let c = s
            .counters
            .iter()
            .find(|c| {
                c.metadata.get("metric").map(String::as_str)
                    == Some("snapshot_v2_group_window_unstamped_probe")
            })
            .expect("unstamped probe counter present in V2 output");
        assert_eq!(
            c.window, None,
            "a declared but never-stamped group must not fabricate a window"
        );
    }

    // No `acq_group` tag: an ordinary `WindowedLazyCounter`, same as an
    // unmigrated sampler still uses. Its own per-metric window path (via
    // `value_with_window`) must be exactly what V2 output carries — the
    // group-window fix only ever overrides metrics that declare `acq_group`.
    #[metric(name = "snapshot_v2_unmigrated_windowed_probe")]
    static V2_UNMIGRATED_WINDOWED_PROBE: metriken::WindowedLazyCounter =
        metriken::WindowedLazyCounter::new(metriken::Counter::default);

    #[test]
    fn v2_output_keeps_the_per_metric_window_for_an_unmigrated_windowed_metric() {
        let win = Window::new(5_000, 9_000);
        V2_UNMIGRATED_WINDOWED_PROBE.set_with_window(3, win);

        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![]);
        let Snapshot::V2(s) = snap else {
            panic!("expected V2")
        };
        let c = s
            .counters
            .iter()
            .find(|c| {
                c.metadata.get("metric").map(String::as_str)
                    == Some("snapshot_v2_unmigrated_windowed_probe")
            })
            .expect("unmigrated windowed probe counter present in V2 output");
        assert_eq!(
            c.window,
            Some(win),
            "unmigrated per-metric window path is untouched by the group-window fix"
        );
    }

    // --- Part B: refresh-read sampler shape (drivehealth's GaugeGroup +
    // single-sweep-group pattern) --------------------------------------
    //
    // Fixture-level: no real ioctls, no real `DriveHealth` sampler. This
    // mirrors drivehealth's exact metric shape (a plain `GaugeGroup` tagged
    // `acq_group`, stamped by one `AcquisitionGroup` covering the whole
    // sweep, sized to backing capacity `MAX_DRIVES`-style with `set_member_bound`
    // pinning real population) to pin three properties any refresh-read
    // sampler migrated under principle 18 must have: a stamped sweep's
    // window reaches BOTH V2 and V3 output; a failed/discarded sweep leaves
    // the previous window standing rather than publishing a fresh one with
    // no new values; and — the regression pin for the member-bound fix — a
    // declared group's V3 population reflects `set_member_bound`'s real
    // count, not the backing array's full 64-slot capacity (an unbounded
    // walk would otherwise emit 63 always-`None` entries here, exactly the
    // `drivehealth_sweep`-on-a-driveless-host bug this fixture pins).

    static DRIVEHEALTH_SHAPE_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "drivehealth_shape_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static DRIVEHEALTH_SHAPE_GROUP_ENTRY: &'static AcquisitionGroup = &DRIVEHEALTH_SHAPE_GROUP;

    // 64 backing entries (drivehealth's own MAX_DRIVES), only 1 populated —
    // mirrors a single-drive host with drivehealth's real capacity, not a
    // toy 2-entry group.
    #[metric(
        name = "snapshot_drivehealth_shape_temperature",
        metadata = { acq_group = "drivehealth_shape_probe" }
    )]
    static DRIVEHEALTH_SHAPE_TEMPERATURE: metriken::GaugeGroup = metriken::GaugeGroup::new(64);

    #[test]
    fn drivehealth_shape_stamped_sweep_window_reaches_v2_and_v3_output() {
        // The sweep: set the member bound to the real (single-drive)
        // population, acquire, set the one drive's value, finish — exactly
        // `linux/mod.rs`'s `spawn_blocking` task shape (`DriveHealth::new`
        // calls `set_member_bound(drives.len())` once at discovery; this
        // fixture does the equivalent for its one fixture "drive").
        DRIVEHEALTH_SHAPE_GROUP.set_member_bound(1);
        let guard = DRIVEHEALTH_SHAPE_GROUP.acquire();
        let _ = DRIVEHEALTH_SHAPE_TEMPERATURE.set(0, 42);
        guard.finish();
        let group_window = DRIVEHEALTH_SHAPE_GROUP
            .window()
            .expect("group was just stamped");

        let v2 = create(SystemTime::now(), Duration::from_secs(1), vec![]);
        let Snapshot::V2(s) = v2 else {
            panic!("expected V2")
        };
        let g = s
            .gauges
            .iter()
            .find(|g| {
                g.metadata.get("metric").map(String::as_str)
                    == Some("snapshot_drivehealth_shape_temperature")
                    && g.metadata.get("id").map(String::as_str) == Some("0")
            })
            .expect("stamped drive-0 gauge present in V2 output");
        assert_eq!(
            g.window,
            Some(group_window),
            "V2 output carries the sweep group's window for a plain GaugeGroup member"
        );

        let mut cache = SkeletonCache::new();
        let v3 = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s) = v3 else {
            panic!("expected V3")
        };
        let group = s
            .groups
            .iter()
            .find(|g| g.name == "unattributed/drivehealth_shape_probe")
            .expect("declared drivehealth-shape group present in V3 output");
        assert_eq!(
            group.window,
            Some(group_window),
            "V3 output carries the same sweep group window"
        );

        // The member-bound regression pin: exactly 1 schema slot (the real
        // population), not 64 (the backing capacity).
        let schema = group.schema.as_ref().expect("schema present");
        assert_eq!(
            schema.gauges.len(),
            1,
            "set_member_bound(1) on a 64-entry backing array emits exactly 1 schema slot, \
             not the full backing capacity — the fix for the always-None entries a missing \
             bound produces on every device-group sampler"
        );
        assert_eq!(
            group.gauges.len(),
            1,
            "value slots match the bounded schema, not the backing capacity"
        );
    }

    static DRIVEHEALTH_SHAPE_DISCARD_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "drivehealth_shape_discard_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static DRIVEHEALTH_SHAPE_DISCARD_GROUP_ENTRY: &'static AcquisitionGroup =
        &DRIVEHEALTH_SHAPE_DISCARD_GROUP;

    #[metric(
        name = "snapshot_drivehealth_shape_discard_temperature",
        metadata = { acq_group = "drivehealth_shape_discard_probe" }
    )]
    static DRIVEHEALTH_SHAPE_DISCARD_TEMPERATURE: metriken::GaugeGroup =
        metriken::GaugeGroup::new(1);

    #[test]
    fn drivehealth_shape_discard_on_failed_sweep_leaves_previous_window_standing() {
        // First sweep: every drive read ok, `finish()` publishes.
        let guard = DRIVEHEALTH_SHAPE_DISCARD_GROUP.acquire();
        let _ = DRIVEHEALTH_SHAPE_DISCARD_TEMPERATURE.set(0, 55);
        guard.finish();
        let first_window = DRIVEHEALTH_SHAPE_DISCARD_GROUP
            .window()
            .expect("first sweep stamped");

        // Second sweep: every drive's read failed (`ok == 0` in
        // `linux/mod.rs`'s terms) — the sampler calls `discard()` instead of
        // `finish()`, exactly like `AcquisitionGuard::drop`. No new value is
        // set either (a fully-failed sweep touches nothing).
        let guard = DRIVEHEALTH_SHAPE_DISCARD_GROUP.acquire();
        guard.discard();

        assert_eq!(
            DRIVEHEALTH_SHAPE_DISCARD_GROUP.window(),
            Some(first_window),
            "a discarded sweep must not advance the group's window"
        );

        let mut cache = SkeletonCache::new();
        let v3 = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s) = v3 else {
            panic!("expected V3")
        };
        let group = s
            .groups
            .iter()
            .find(|g| g.name == "unattributed/drivehealth_shape_discard_probe")
            .expect("declared group present in V3 output");
        assert_eq!(
            group.window,
            Some(first_window),
            "V3 output still carries the first sweep's window, not a fabricated new one"
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
        let group2 = s2
            .groups
            .iter()
            .find(|g| g.name == "unattributed/stability_probe")
            .expect("group present in tick 2");
        assert_eq!(
            hash1, group2.schema_hash,
            "schema hash is stable across ticks for an unchanged group"
        );
        // A hit tick's schema/values must still satisfy the wire contract
        // (arity matches, schema_hash matches the schema) — this is
        // exactly what the arity check in create_v3's hit path exists to
        // guarantee before a payload ever reaches this point.
        assert_eq!(group2.validate(), Ok(()));
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

    // Dedicated group + metric, backing capacity for 512 members. Declared,
    // dense (`member_bound`, not reader-stamped), with per-member metadata
    // attached — the shape most declared groups use in production
    // (per-CPU/per-core counters). Grown from `ALLOC_TEST_SMALL_N` to
    // `ALLOC_TEST_LARGE_N` members mid-test (see the test below) so hit-tick
    // allocations can be compared AT TWO DIFFERENT MEMBER COUNTS WITHIN ONE
    // TEST RUN — the direct way to prove "not O(N)" without depending on
    // knowing this test binary's total registry size (a `create_v3` call
    // always walks the FULL `metriken` registry, so a hit tick's measured
    // allocation total is dominated by O(distinct groups) bookkeeping
    // shared by every other declared group this binary registers, not just
    // this fixture — an absolute threshold would really be asserting
    // "roughly how many groups exist today", which is the wrong thing to
    // pin. Comparing the SAME registry's hit-tick cost at two member counts
    // for the SAME group cancels that shared baseline out and isolates
    // exactly the thing under test).
    const ALLOC_TEST_SMALL_N: usize = 8;
    const ALLOC_TEST_LARGE_N: usize = 512;

    static V3_ALLOC_GROUP: AcquisitionGroup = AcquisitionGroup::new("unattributed", "alloc_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_ALLOC_GROUP_ENTRY: &'static AcquisitionGroup = &V3_ALLOC_GROUP;

    #[metric(
        name = "snapshot_v3_alloc_counters",
        metadata = { acq_group = "alloc_probe" }
    )]
    static V3_ALLOC_COUNTERS: metriken::CounterGroup =
        metriken::CounterGroup::new(ALLOC_TEST_LARGE_N);

    /// Populate `[from, to)` with a value and per-member metadata, set the
    /// group's member bound to `to`, and re-acquire/finish the bracket so
    /// the next `create_v3` call sees the new population.
    fn grow_alloc_probe(from: usize, to: usize) {
        for idx in from..to {
            V3_ALLOC_COUNTERS.add(idx, idx as u64 + 1);
            V3_ALLOC_COUNTERS.set_metadata(idx, [("cpu".to_string(), idx.to_string())].into());
        }
        V3_ALLOC_GROUP.set_member_bound(to);
        let guard = V3_ALLOC_GROUP.acquire();
        guard.finish();
    }

    /// Run two ticks against `cache` and return `(snapshot, hit_allocs)` for
    /// the second — the first just warms the cache (necessarily a miss for
    /// anything that changed since the LAST call using this `cache`, e.g.
    /// this fixture right after `grow_alloc_probe` moved its member bound)
    /// and is not measured.
    fn warm_then_measure_hit(cache: &mut SkeletonCache) -> (Snapshot, usize) {
        let _ = create_v3(SystemTime::now(), Duration::from_secs(1), vec![], cache);
        count_allocations(|| create_v3(SystemTime::now(), Duration::from_secs(1), vec![], cache))
    }

    fn alloc_probe_group(snap: &Snapshot) -> &GroupSnapshot {
        let Snapshot::V3(s) = snap else {
            panic!("expected V3")
        };
        s.groups
            .iter()
            .find(|g| g.name == "unattributed/alloc_probe")
            .expect("alloc-probe group present")
    }

    #[test]
    fn v3_hit_tick_allocations_are_a_small_constant_not_o_n() {
        let mut cache = SkeletonCache::new();

        // Phase 1: SMALL_N members, hit tick measured.
        grow_alloc_probe(0, ALLOC_TEST_SMALL_N);
        let (snap_small, small_allocs) = warm_then_measure_hit(&mut cache);
        let group_small = alloc_probe_group(&snap_small);
        let schema_small = group_small.schema.as_ref().expect("schema present").clone();
        assert_eq!(schema_small.counters.len(), ALLOC_TEST_SMALL_N);

        // Phase 2: grow to LARGE_N members — the next tick using `cache`
        // must be a miss (real membership change), then the tick after
        // that is a fresh hit at the larger member count.
        grow_alloc_probe(ALLOC_TEST_SMALL_N, ALLOC_TEST_LARGE_N);
        let (snap_large, large_allocs) = warm_then_measure_hit(&mut cache);
        let group_large = alloc_probe_group(&snap_large);
        let schema_large = group_large.schema.as_ref().expect("schema present");
        assert_eq!(schema_large.counters.len(), ALLOC_TEST_LARGE_N);
        assert!(
            !Arc::ptr_eq(&schema_small, schema_large),
            "growing membership must NOT reuse the small fixture's cached Arc<GroupSchema>"
        );

        // The identity-hash claim, pinned directly: a hit tick reuses the
        // cached `Arc<GroupSchema>` allocation (a refcount bump), not a
        // freshly-rebuilt-but-content-equal one — checked by re-measuring
        // the LARGE_N population's hit tick a second time and confirming
        // the schema `Arc` is the SAME allocation as `snap_large`'s, and the
        // wire output (values) is unchanged.
        let (snap_large_again, _) = count_allocations(|| {
            create_v3(
                SystemTime::now(),
                Duration::from_secs(1),
                vec![],
                &mut cache,
            )
        });
        let group_large_again = alloc_probe_group(&snap_large_again);
        assert!(
            Arc::ptr_eq(
                schema_large,
                group_large_again.schema.as_ref().expect("schema present")
            ),
            "an unchanged 512-member group must reuse the cached Arc<GroupSchema> on its next hit"
        );
        assert_eq!(
            group_large.schema_hash, group_large_again.schema_hash,
            "wire schema_hash is stable across a hit"
        );
        assert_eq!(
            group_large.counters, group_large_again.counters,
            "a hit tick's wire output (values) equals the prior tick's for this unchanged fixture"
        );

        // The allocation-parity claim: growing this fixture's OWN member
        // count 64x (SMALL_N=8 -> LARGE_N=512) must not move the hit-tick
        // allocation total by anywhere close to that factor. Both
        // measurements walk the SAME process-wide metriken registry (every
        // other declared group this test binary registers, dozens of them,
        // contributes the SAME O(distinct groups) bookkeeping cost to
        // both), so comparing them cancels that shared baseline and
        // isolates this fixture's own marginal cost.
        //
        // Measured on this run (2026-08-19, this worktree, debug build,
        // `--test-threads=1`): small_allocs and large_allocs came out
        // IDENTICAL — 916 both times (down from 1,407 before the
        // tuple-keyed routing fix below dropped the remaining
        // O(registry-entry-count) `format!` cost) — meaning this fixture's
        // growth from 8 to 512 members added exactly zero additional
        // allocations on a hit. Under `cargo test`'s default parallelism
        // the two
        // measurements are NOT taken back-to-back in isolation — other
        // tests mutate their OWN groups on other threads in between, and
        // `create_v3` walks the full process-wide registry every time, so
        // some of that concurrent churn legitimately lands as real misses
        // (and their real allocations) inside one measurement or the other
        // — observed delta up to ~40 across repeated full-suite runs, the
        // same class of cross-test interference documented on
        // `skeleton_cache_is_stable_across_ticks` and elsewhere in this
        // file. The margin below (300) comfortably covers that noise while
        // staying nowhere near what a real per-member regression would add:
        // before this change, growing 504 more members would have cost a
        // `MetricDesc` (`String` name + `BTreeMap`) AND an `entry_metadata`
        // `HashMap` PER MEMBER, on the order of 1,500+ extra allocations —
        // two orders of magnitude past this margin.
        let delta = large_allocs.abs_diff(small_allocs);
        assert!(
            delta <= 300,
            "growing this fixture from {ALLOC_TEST_SMALL_N} to {ALLOC_TEST_LARGE_N} members \
             changed hit-tick allocations by {delta} ({small_allocs} -> {large_allocs}) — \
             expected ~0 (some slack for concurrent-test noise); a per-member allocation \
             regression would move this by well over a thousand"
        );
    }

    // Dedicated group + metric: reader-stamped (sparse, metadata-presence
    // membership), 64 backing entries, a handful populated. Complements
    // `reader_stamped_sparse_group_emits_only_metadata_populated_indices`
    // (which pins schema_hash stability and real-churn invalidation) by
    // proving the CACHE MECHANISM directly via `Arc::ptr_eq`: an unchanged
    // sparse population is a true hit (same allocation reused), and a
    // member appearing or disappearing is a true miss (a new allocation),
    // not a coincidentally-equal rebuild either way.
    static V3_SPARSE_CHURN_GROUP: AcquisitionGroup =
        AcquisitionGroup::new_reader_stamped("unattributed", "sparse_churn_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_SPARSE_CHURN_GROUP_ENTRY: &'static AcquisitionGroup = &V3_SPARSE_CHURN_GROUP;

    #[metric(
        name = "snapshot_v3_sparse_churn_counters",
        metadata = { acq_group = "sparse_churn_probe" }
    )]
    static V3_SPARSE_CHURN_COUNTERS: metriken::CounterGroup = metriken::CounterGroup::new(64);

    #[test]
    fn sparse_membership_unchanged_hits_changed_misses() {
        V3_SPARSE_CHURN_COUNTERS.set_metadata(3, [("cgroup".to_string(), "/a".to_string())].into());
        V3_SPARSE_CHURN_COUNTERS.add(3, 1);
        V3_SPARSE_CHURN_COUNTERS
            .set_metadata(40, [("cgroup".to_string(), "/b".to_string())].into());
        V3_SPARSE_CHURN_COUNTERS.add(40, 2);

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
            .find(|g| g.name == "unattributed/sparse_churn_probe")
            .expect("sparse churn group present in tick 1");
        let schema1 = group1.schema.as_ref().expect("schema present").clone();
        assert_eq!(schema1.counters.len(), 2);

        // Same population, no churn: a true hit.
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
            .find(|g| g.name == "unattributed/sparse_churn_probe")
            .expect("sparse churn group present in tick 2");
        assert!(
            Arc::ptr_eq(&schema1, group2.schema.as_ref().unwrap()),
            "unchanged sparse population must reuse the cached Arc<GroupSchema>"
        );

        // A member appears: must be a miss (a new allocation, not the old
        // one reused, and a new wire hash).
        V3_SPARSE_CHURN_COUNTERS
            .set_metadata(50, [("cgroup".to_string(), "/c".to_string())].into());
        V3_SPARSE_CHURN_COUNTERS.add(50, 3);
        let snap3 = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s3) = snap3 else {
            panic!("expected V3")
        };
        let group3 = s3
            .groups
            .iter()
            .find(|g| g.name == "unattributed/sparse_churn_probe")
            .expect("sparse churn group present in tick 3");
        let schema3 = group3.schema.as_ref().expect("schema present");
        assert_eq!(schema3.counters.len(), 3, "the new member is present");
        assert!(
            !Arc::ptr_eq(&schema1, schema3),
            "a member appearing must NOT reuse the old cached Arc<GroupSchema>"
        );
        assert_ne!(
            group1.schema_hash, group3.schema_hash,
            "a member appearing must change the wire schema hash"
        );

        // A member disappears: also a miss.
        V3_SPARSE_CHURN_COUNTERS.clear_metadata(50);
        let snap4 = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s4) = snap4 else {
            panic!("expected V3")
        };
        let group4 = s4
            .groups
            .iter()
            .find(|g| g.name == "unattributed/sparse_churn_probe")
            .expect("sparse churn group present in tick 4");
        let schema4 = group4.schema.as_ref().expect("schema present");
        assert_eq!(schema4.counters.len(), 2, "the removed member is gone");
        assert!(
            !Arc::ptr_eq(schema3, schema4),
            "a member disappearing must NOT reuse the previous tick's cached Arc<GroupSchema>"
        );
        assert_ne!(
            group3.schema_hash, group4.schema_hash,
            "a member disappearing must change the wire schema hash"
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

    // Dedicated metric, no `acq_group` — routes to the shared
    // "unattributed/main" default group like every other undeclared metric
    // in this test binary (module-based sampler attribution gives test
    // fixtures no way to land in a group of their own without declaring
    // one). That sharing means this test can't cleanly assert "tick 2 IS a
    // hit" (a concurrently running test could legitimately force a miss on
    // "unattributed/main" for an unrelated reason — see the same caveat on
    // `skeleton_cache_is_stable_across_ticks`), but it CAN assert "tick 2
    // is NOT the same cached schema as tick 1" unconditionally: crossing
    // zero -> nonzero is a real membership change that this test's own
    // member forces regardless of what else is running, so non-equality
    // must hold either way.
    #[metric(name = "snapshot_v3_default_zero_cross_counter")]
    static V3_DEFAULT_ZERO_CROSS_COUNTERS: metriken::CounterGroup = metriken::CounterGroup::new(1);

    #[test]
    fn default_group_member_crossing_zero_to_nonzero_misses_and_validates() {
        // Regression guard for the miss-tick cache-poisoning class of bug:
        // a DEFAULT (non-declared) group's membership is value-derived (the
        // transitional sentinel skip at zero — see `create_v3`'s doc
        // comment), so a member crossing from absent (value 0) to present
        // (nonzero) between two ticks is a REAL membership change. It must
        // always produce a genuine miss (never get served from a cache
        // entry that describes the earlier, absent state), and the
        // resulting snapshot must satisfy `GroupSnapshot::validate()` on
        // BOTH ticks — exactly the invariant a miss-tick identity/schema
        // mismatch would violate.
        V3_DEFAULT_ZERO_CROSS_COUNTERS.set(0, 0);

        let find = |schema: &GroupSchema| {
            schema.counters.iter().position(|d| {
                d.metadata.get("metric").map(String::as_str)
                    == Some("snapshot_v3_default_zero_cross_counter")
            })
        };

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
            .find(|g| g.name == "unattributed/main")
            .expect("default group present on tick 1");
        assert_eq!(group1.validate(), Ok(()));
        let schema1 = group1.schema.as_ref().expect("schema present").clone();
        assert!(
            find(&schema1).is_none(),
            "value-0 member is sentinel-skipped from the default group's schema on tick 1"
        );

        // Cross zero -> nonzero: a real membership change.
        V3_DEFAULT_ZERO_CROSS_COUNTERS.set(0, 1);
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
            .find(|g| g.name == "unattributed/main")
            .expect("default group present on tick 2");
        assert_eq!(group2.validate(), Ok(()));
        let schema2 = group2.schema.as_ref().expect("schema present");
        let idx = find(schema2).expect("newly-nonzero member now present in the schema");
        assert_eq!(group2.counters[idx], Some(1));

        assert!(
            !Arc::ptr_eq(&schema1, schema2),
            "a member crossing zero -> nonzero is a real membership change and must MISS \
             — never reuse the cached Arc<GroupSchema> from before the member existed"
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

    /// V2 gives each external metric its own column key, so two active at once
    /// do not collide.
    ///
    /// The entry name is a column key — `.rez` ingest keys columns by it — and
    /// V2 used to pass `String::new()` for every external metric. Two of the
    /// same type then double-pushed into the one empty column, misaligning
    /// values against timestamps from that row on; two of different types were
    /// silently dropped by the shape-mismatch skip. Both failures are quiet,
    /// which is what makes them worth a test rather than a comment.
    ///
    /// V3 has always keyed these by identity, and is the default since #1076 —
    /// but v2 remains a supported escape hatch, and silent value misalignment
    /// is a bad thing to leave in a supported path.
    #[test]
    fn v2_external_metrics_do_not_collide_on_one_column_key() {
        let labels_a: HashMap<String, String> = [("env".to_string(), "prod".to_string())].into();
        let labels_b: HashMap<String, String> = [("env".to_string(), "dev".to_string())].into();

        let counter = |labels: HashMap<String, String>, value: u64| ExternalMetric {
            name: "ext_shared_name".into(),
            labels,
            value: ExternalMetricValue::Counter(value),
            last_updated: std::time::Instant::now(),
            window: None,
        };
        let gauge = |name: &str| ExternalMetric {
            name: name.into(),
            labels: Default::default(),
            value: ExternalMetricValue::Gauge(5),
            last_updated: std::time::Instant::now(),
            window: None,
        };

        let snap = create(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![
                counter(labels_a.clone(), 1),
                counter(labels_b.clone(), 2),
                gauge("ext_gauge"),
            ],
        );
        let Snapshot::V2(s) = snap else {
            panic!("expected V2")
        };

        // `create` also emits rezolus's own registry metrics; only the
        // external ones are under test here.
        let is_external =
            |m: &HashMap<String, String>| m.get("source").map(String::as_str) == Some("external");
        let ext_counters: Vec<_> = s
            .counters
            .iter()
            .filter(|c| is_external(&c.metadata))
            .collect();
        let ext_gauges: Vec<_> = s
            .gauges
            .iter()
            .filter(|g| is_external(&g.metadata))
            .collect();
        let names: Vec<&str> = ext_counters.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names.len(),
            2,
            "same name, different labels are two distinct series"
        );
        assert_ne!(
            names[0], names[1],
            "two external counters must not share a column key: {names:?}"
        );
        assert!(
            ext_counters.iter().all(|c| !c.name.is_empty())
                && ext_gauges.iter().all(|g| !g.name.is_empty()),
            "an empty key is the collision this guards against"
        );

        // Same identity, fresh snapshot: the key must be reproducible, or a
        // consumer keyed by it sees one series break into several.
        let again = create(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![counter(labels_a.clone(), 9)],
        );
        let Snapshot::V2(again) = again else {
            panic!("expected V2")
        };
        let again_name = again
            .counters
            .iter()
            .find(|c| is_external(&c.metadata))
            .expect("the external counter is present")
            .name
            .clone();
        assert!(
            names.contains(&again_name.as_str()),
            "the same metric must reattach under the same key across ticks"
        );

        // The real metric name still travels in metadata, unchanged — the key
        // is an identity, not a rename.
        assert!(
            ext_counters
                .iter()
                .all(|c| c.metadata.get("metric").map(String::as_str) == Some("ext_shared_name")),
            "metadata[\"metric\"] carries the human name, as before"
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
        assert_eq!(group2.validate(), Ok(()));
    }

    // --- reader-stamped (PackedCounters) groups --------------------------

    // Dedicated group + metric, touched by no other test. 8 backing
    // entries; only a handful get metadata, standing in for a packed
    // cgroup/task map where most of MAX_CGROUPS/MAX_PID is unregistered.
    // 8 has no significance beyond "small and arbitrary" — this test is
    // about window/bracket behavior, not membership-at-scale (that's
    // `reader_stamped_sparse_group_emits_only_metadata_populated_indices`,
    // at 1000 entries, and `reader_stamped_group_at_max_pid_scale_...`,
    // below, at the real `MAX_PID`).
    static V3_READER_STAMPED_GROUP: AcquisitionGroup =
        AcquisitionGroup::new("unattributed", "reader_stamped_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_READER_STAMPED_GROUP_ENTRY: &'static AcquisitionGroup = &V3_READER_STAMPED_GROUP;

    #[metric(
        name = "snapshot_v3_reader_stamped_counters",
        metadata = { acq_group = "reader_stamped_probe" }
    )]
    static V3_READER_STAMPED_COUNTERS: metriken::CounterGroup = metriken::CounterGroup::new(8);

    #[test]
    fn reader_stamped_declared_group_carries_a_walk_spanning_window_in_v3() {
        // Mirrors what `PackedCounters::new` does (mark the group, don't
        // ever call acquire()/finish() from a sampler side) without needing
        // a real BPF map.
        V3_READER_STAMPED_GROUP.set_reader_stamped();
        V3_READER_STAMPED_COUNTERS
            .set_metadata(1, [("cgroup".to_string(), "/a".to_string())].into());
        V3_READER_STAMPED_COUNTERS.add(1, 5);

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
            .find(|g| g.name == "unattributed/reader_stamped_probe")
            .expect("reader-stamped group present");
        assert_eq!(group1.validate(), Ok(()));
        let window1 = group1
            .window
            .expect("reader-stamped group's window is stamped by create_v3 itself, not a sampler");
        assert!(window1.begin_ns > 0, "wall-clock begin");
        assert!(
            window1.end_ns >= window1.begin_ns,
            "finish() lands at or after acquire() — never leads"
        );

        // A second, independent tick re-acquires and re-finishes the
        // bracket from scratch (there is no sampler that could have
        // stamped it in between — see `set_reader_stamped`'s single-writer
        // note) — this walk's window must not be the first walk's stale
        // leftover.
        let snap2 = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s2) = snap2 else {
            panic!("expected V3")
        };
        let window2 = s2
            .groups
            .iter()
            .find(|g| g.name == "unattributed/reader_stamped_probe")
            .expect("reader-stamped group present on tick 2")
            .window
            .expect("stamped again on tick 2");
        assert!(
            window2.begin_ns >= window1.begin_ns,
            "each tick's bracket begins no earlier than the previous tick's — \
             tick 2: {window2:?}, tick 1: {window1:?}"
        );
    }

    // Concurrent snapshot builders over a reader-stamped group must not panic.
    // This is a smoke test of the concurrency contract `BUILDER_TEST_LOCK`
    // upholds (issue #1130), not a deterministic reproduction: the flake is a
    // collision inside the window slot's ~ns write critical section, so it
    // cannot be triggered reliably or cheaply -- 32 barrier-synced threads
    // hammering `create_v3` reproduced the exact "single writer" assert only
    // ~1-in-3 without the lock, and serialise to minutes WITH it. The fix's real
    // guarantee is structural (create/create_v3 are the sole writers of these
    // shared slots and now run one-at-a-time under test, restoring the
    // production single-writer invariant); this keeps the path exercised and
    // catches a gross regression (deadlock, panic) fast.
    #[test]
    fn concurrent_builders_over_a_reader_stamped_group_do_not_panic() {
        V3_READER_STAMPED_GROUP.set_reader_stamped();
        V3_READER_STAMPED_COUNTERS
            .set_metadata(1, [("cgroup".to_string(), "/smoke".to_string())].into());
        V3_READER_STAMPED_COUNTERS.add(1, 1);

        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    let mut cache = SkeletonCache::new();
                    for _ in 0..25 {
                        let _ = create_v3(
                            SystemTime::now(),
                            Duration::from_secs(1),
                            vec![],
                            &mut cache,
                        );
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("a concurrent builder panicked");
        }
    }

    // Regression fixture for a real bug caught while building this wave: a
    // group declared with plain `AcquisitionGroup::new` reads
    // `is_reader_stamped() == false` until SOMETHING calls
    // `set_reader_stamped()` — and in production that only happens inside
    // `PackedCounters::new`, which only runs once a sampler's `init()` has
    // gotten as far as attaching a live BPF map. That never happens in a
    // unit test (no sampler `init()` ever runs), and does not happen for a
    // sampler disabled via config either — yet the `#[metric]` static and
    // its `acq_group` tag are registered unconditionally at compile time,
    // so `create_v3` still routes it to a DECLARED group. Before the fix
    // (`AcquisitionGroup::new_reader_stamped`, set at construction instead
    // of relying solely on the runtime setter), that meant every declared-
    // but-not-yet-runtime-flagged packed group fell through to the
    // sampler-stamped bound-walk (`0..entries()`, no sentinel skip on the
    // declared path — see the CounterGroup arm's doc comment) — for
    // `task_cpu_usage`'s real `MAX_PID` = 4,194,304 backing array, that is
    // 4.2M pushed entries on ANY call to `create_v3` that merely touches
    // the metriken registry, which is every single test in this file.
    // Measured: killed (SIGKILL, OOM) an 8 GB container running the full
    // test suite. `MAX_PID_SCALE_GROUP` below uses the REAL `MAX_PID`
    // constant, declared with `new_reader_stamped` and never touched by
    // `set_reader_stamped` at all, to pin that the fix actually closes the
    // gap rather than merely relying on `PackedCounters::new` having run.
    static MAX_PID_SCALE_GROUP: AcquisitionGroup =
        AcquisitionGroup::new_reader_stamped("unattributed", "max_pid_scale_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static MAX_PID_SCALE_GROUP_ENTRY: &'static AcquisitionGroup = &MAX_PID_SCALE_GROUP;

    #[metric(
        name = "snapshot_v3_max_pid_scale_counters",
        metadata = { acq_group = "max_pid_scale_probe" }
    )]
    static MAX_PID_SCALE_COUNTERS: metriken::CounterGroup =
        metriken::CounterGroup::new(crate::agent::MAX_PID);

    #[test]
    fn reader_stamped_group_at_max_pid_scale_never_walks_entries_without_set_reader_stamped() {
        // The first `.add()` below triggers `CounterGroup::get_or_init()`,
        // which lazily allocates the FULL `MAX_PID`-sized backing array —
        // one `Vec<AtomicU64>` of 4,194,304 elements, ~33MB. That is a
        // one-time, bounded allocation independent of what this test is
        // pinning: metriken's own value storage, not the bug (which was
        // `create_v3` pushing ~4.2M `MetricDesc`/`HashMap` OUTPUT entries).
        // Expect this test to cost ~33MB regardless of pass/fail; the
        // catastrophic case is a SEPARATE, much larger cost this test
        // exists to prove doesn't happen.
        //
        // Populate exactly 2 of MAX_PID entries, and — critically — never
        // call `MAX_PID_SCALE_GROUP.set_reader_stamped()`. If routing ever
        // regresses to depending on that call alone, this test allocates
        // ~4.2M `MetricDesc`/`HashMap` entries and either OOMs the test
        // process or takes long enough to be obviously wrong; passing
        // quickly with exactly 2 members is the pin.
        MAX_PID_SCALE_COUNTERS.set_metadata(7, [("pid".to_string(), "7".to_string())].into());
        MAX_PID_SCALE_COUNTERS.add(7, 1);
        MAX_PID_SCALE_COUNTERS.set_metadata(
            4_000_000,
            [("pid".to_string(), "4000000".to_string())].into(),
        );
        MAX_PID_SCALE_COUNTERS.add(4_000_000, 1);

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
            .find(|g| g.name == "unattributed/max_pid_scale_probe")
            .expect("MAX_PID-scale declared group present");
        let schema = group.schema.as_ref().expect("schema present");
        assert_eq!(
            schema.counters.len(),
            2,
            "exactly the 2 populated indices out of {} entries — never the full capacity",
            crate::agent::MAX_PID
        );
    }

    // Dedicated group + metric: 1000 backing entries (standing in for
    // MAX_CGROUPS/MAX_PID's over-allocation), only a few ever populated.
    static V3_SPARSE_GROUP: AcquisitionGroup =
        AcquisitionGroup::new_reader_stamped("unattributed", "sparse_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_SPARSE_GROUP_ENTRY: &'static AcquisitionGroup = &V3_SPARSE_GROUP;

    #[metric(
        name = "snapshot_v3_sparse_counters",
        metadata = { acq_group = "sparse_probe" }
    )]
    static V3_SPARSE_COUNTERS: metriken::CounterGroup = metriken::CounterGroup::new(1000);

    #[test]
    fn reader_stamped_sparse_group_emits_only_metadata_populated_indices() {
        // Three populated indices, one deliberately left at value 0 to pin
        // that reader-stamped groups use honest-zero registration
        // membership (the "default-group sentinel path gone for migrated
        // metrics" requirement) — value 0 must NOT be skipped the way the
        // default/unmigrated group's transitional sentinel skip would.
        V3_SPARSE_COUNTERS.set_metadata(5, [("cgroup".to_string(), "/a".to_string())].into());
        V3_SPARSE_COUNTERS.add(5, 3);
        V3_SPARSE_COUNTERS.set_metadata(500, [("cgroup".to_string(), "/b".to_string())].into());
        V3_SPARSE_COUNTERS.add(500, 0);
        V3_SPARSE_COUNTERS.set_metadata(999, [("cgroup".to_string(), "/c".to_string())].into());
        V3_SPARSE_COUNTERS.add(999, 7);

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
            .find(|g| g.name == "unattributed/sparse_probe")
            .expect("sparse declared group present");
        assert_eq!(group.validate(), Ok(()));
        let schema = group.schema.as_ref().expect("schema present");
        assert_eq!(
            schema.counters.len(),
            3,
            "exactly the 3 metadata-registered indices, not the 1000-entry backing capacity \
             (never MAX_CGROUPS/MAX_PID's full capacity — see the walk-cost grounding on \
             create_v3)"
        );
        let ids: Vec<&str> = schema
            .counters
            .iter()
            .map(|d| d.metadata.get("id").map(String::as_str).unwrap())
            .collect();
        assert_eq!(
            ids,
            vec!["5", "500", "999"],
            "sorted by index — stable order regardless of metadata_snapshot()'s HashMap order"
        );
        let idx500 = schema
            .counters
            .iter()
            .position(|d| d.metadata.get("id").map(String::as_str) == Some("500"))
            .unwrap();
        assert_eq!(
            group.counters[idx500],
            Some(0),
            "index 500's honest zero is present, not sentinel-skipped"
        );

        // Stability: the same 3 members produce the identical schema hash
        // on a second tick (no churn from metadata_snapshot()'s
        // non-deterministic HashMap order). NOT asserting the global
        // `cache.rebuilds()` counter here — it accumulates across every
        // group any concurrently running test's own `create_v3` call
        // touches, not just this group (see the identical caveat on
        // `skeleton_cache_is_stable_across_ticks`); `schema_hash`, scoped
        // to this test's own group, is what's actually pinned.
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
            .find(|g| g.name == "unattributed/sparse_probe")
            .expect("sparse declared group present on tick 2");
        assert_eq!(
            group.schema_hash, group2.schema_hash,
            "unchanged membership across ticks does not churn the schema hash"
        );
        assert_eq!(group2.validate(), Ok(()));

        // A member exiting (metadata cleared, simulating a cgroup/task
        // going away) drops it from the schema and DOES force a rebuild —
        // schema churn tracks real membership change, not walk noise.
        V3_SPARSE_COUNTERS.clear_metadata(500);
        let snap3 = create_v3(
            SystemTime::now(),
            Duration::from_secs(1),
            vec![],
            &mut cache,
        );
        let Snapshot::V3(s3) = snap3 else {
            panic!("expected V3")
        };
        let group3 = s3
            .groups
            .iter()
            .find(|g| g.name == "unattributed/sparse_probe")
            .expect("sparse declared group present on tick 3");
        let schema3 = group3.schema.as_ref().expect("schema present");
        assert_eq!(schema3.counters.len(), 2, "the exited member is gone");
        assert_ne!(
            group.schema_hash, group3.schema_hash,
            "real membership change rebuilds the schema hash"
        );
    }

    // Dedicated group + metric for the V2 bracket-window test below.
    static V2_READER_STAMPED_GROUP: AcquisitionGroup =
        AcquisitionGroup::new_reader_stamped("unattributed", "v2_reader_stamped_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V2_READER_STAMPED_GROUP_ENTRY: &'static AcquisitionGroup = &V2_READER_STAMPED_GROUP;

    #[metric(
        name = "snapshot_v2_reader_stamped_counters",
        metadata = { acq_group = "v2_reader_stamped_probe" }
    )]
    static V2_READER_STAMPED_COUNTERS: metriken::CounterGroup = metriken::CounterGroup::new(4);

    #[test]
    fn v2_output_carries_the_bracket_window_for_a_reader_stamped_group_member() {
        V2_READER_STAMPED_COUNTERS.add(2, 9);

        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![]);
        let Snapshot::V2(s) = snap else {
            panic!("expected V2")
        };
        let c = s
            .counters
            .iter()
            .find(|c| {
                c.metadata.get("metric").map(String::as_str)
                    == Some("snapshot_v2_reader_stamped_counters")
                    && c.metadata.get("id").map(String::as_str) == Some("2")
            })
            .expect("reader-stamped counter present in V2 output");
        let window = c.window.expect(
            "V2's resolve-once map cannot serve a reader-stamped group — \
                     create() must bracket it itself, not leave it windowless",
        );
        assert!(window.begin_ns > 0, "wall-clock begin");
    }

    // Dedicated pair of groups/metrics: identical shape (4 entries; indices
    // 0 and 3 get nonzero values, index 2 an explicit honest zero, index 1
    // untouched — both end up 0 once the backing array lazily initializes
    // on first write, so V2's sentinel skip drops both 1 and 2 identically
    // regardless of which reason produced the zero), one plain (no
    // acq_group — stands in for an unmigrated packed metric, pre-wave-2),
    // one reader-stamped (post-migration). Pins that migrating a packed
    // metric to a reader-stamped group changes ONLY the window V2 attaches,
    // never which entries are emitted or their values.
    #[metric(name = "snapshot_v2_compat_plain_counters")]
    static V2_COMPAT_PLAIN_COUNTERS: metriken::CounterGroup = metriken::CounterGroup::new(4);

    static V2_COMPAT_READER_STAMPED_GROUP: AcquisitionGroup =
        AcquisitionGroup::new_reader_stamped("unattributed", "v2_compat_reader_stamped");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V2_COMPAT_READER_STAMPED_GROUP_ENTRY: &'static AcquisitionGroup =
        &V2_COMPAT_READER_STAMPED_GROUP;

    #[metric(
        name = "snapshot_v2_compat_reader_stamped_counters",
        metadata = { acq_group = "v2_compat_reader_stamped" }
    )]
    static V2_COMPAT_READER_STAMPED_COUNTERS: metriken::CounterGroup =
        metriken::CounterGroup::new(4);

    #[test]
    fn v2_output_is_unchanged_except_windows_for_a_migrated_packed_metric() {
        for g in [
            &V2_COMPAT_PLAIN_COUNTERS,
            &V2_COMPAT_READER_STAMPED_COUNTERS,
        ] {
            g.add(0, 5);
            // index 1 left untouched: never-written sentinel, skipped by
            // BOTH paths identically.
            g.add(2, 0); // honest zero: V2's transitional sentinel skip drops this on BOTH paths
            g.add(3, 11);
        }

        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![]);
        let Snapshot::V2(s) = snap else {
            panic!("expected V2")
        };

        let plain: Vec<(String, u64, Option<Window>)> = s
            .counters
            .iter()
            .filter(|c| {
                c.metadata.get("metric").map(String::as_str)
                    == Some("snapshot_v2_compat_plain_counters")
            })
            .map(|c| (c.metadata.get("id").cloned().unwrap(), c.value, c.window))
            .collect();
        let migrated: Vec<(String, u64, Option<Window>)> = s
            .counters
            .iter()
            .filter(|c| {
                c.metadata.get("metric").map(String::as_str)
                    == Some("snapshot_v2_compat_reader_stamped_counters")
            })
            .map(|c| (c.metadata.get("id").cloned().unwrap(), c.value, c.window))
            .collect();

        let plain_ids: Vec<&str> = plain.iter().map(|(id, _, _)| id.as_str()).collect();
        let migrated_ids: Vec<&str> = migrated.iter().map(|(id, _, _)| id.as_str()).collect();
        assert_eq!(
            plain_ids, migrated_ids,
            "identical membership (same sentinel skip, same surviving indices)"
        );
        let plain_values: Vec<u64> = plain.iter().map(|(_, v, _)| *v).collect();
        let migrated_values: Vec<u64> = migrated.iter().map(|(_, v, _)| *v).collect();
        assert_eq!(plain_values, migrated_values, "identical values");

        // The only difference: the migrated (reader-stamped) path carries a
        // real bracket window; the plain path — never `set_with_window`,
        // no acq_group — carries none, exactly as it did before wave 2.
        assert!(
            plain.iter().all(|(_, _, w)| w.is_none()),
            "unmigrated packed metric stays windowless in V2, as before"
        );
        assert!(
            migrated.iter().all(|(_, _, w)| w.is_some()),
            "migrated packed metric gains a window in V2 — the only change"
        );
    }

    /// A request that hits the TTL cache reuses the already-encoded body
    /// instead of re-encoding it.
    ///
    /// The cache stored only the `Snapshot`, so every request re-serialized the
    /// whole thing — measured at 3.47 MB per request on a 26-sampler host, and
    /// `to_vec` grows from empty by doubling, so that was also about a dozen
    /// reallocate-and-copy steps per request, all discarded. Identical
    /// allocation identity is the direct evidence that no second encode
    /// happened: a re-encode would necessarily produce a different buffer.
    #[tokio::test]
    async fn a_cache_hit_reuses_the_encoded_body_instead_of_re_encoding() {
        // A long TTL so the second call is unambiguously a cache hit.
        let config: Config = toml::from_str("[general]\nttl = \"60s\"\n").expect("valid config");
        let mut builder = SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );

        let now = Instant::now();
        let first = builder.build_msgpack(now).await;
        let second = builder.build_msgpack(now).await;

        assert_eq!(first, second, "same snapshot must encode to the same bytes");
        assert_eq!(
            first.as_ptr(),
            second.as_ptr(),
            "a cache hit must hand back the SAME buffer — a different pointer \
             means it re-encoded"
        );

        // A refresh past the TTL legitimately produces a new body; the reuse
        // above must not be a stale-forever cache.
        let later = now + Duration::from_secs(120);
        let third = builder.build_msgpack(later).await;
        assert_ne!(
            first.as_ptr(),
            third.as_ptr(),
            "past the TTL the snapshot is rebuilt, so its body must be too"
        );
    }

    /// An explicit member set walks exactly those indices, and a bound still
    /// walks the prefix.
    ///
    /// This is what keeps a partially-allocated sampler honest. A group whose
    /// members are, say, CPUs 16-31 cannot say so with a bound — a bound means
    /// "the first N" — so it would have to declare 0..32 and let the CPUs it
    /// never wrote publish `0`. An unwritten `CounterGroup` slot reads as zero,
    /// not as absent, so that is a wrong value rather than missing data: a
    /// consumer summing across the machine would understate it and see nothing
    /// wrong.
    #[test]
    fn an_explicit_member_set_walks_only_its_own_indices() {
        // A bound: the dense prefix, as before.
        let prefix: Vec<usize> = members(None, Some(4), 32).collect();
        assert_eq!(prefix, vec![0, 1, 2, 3]);

        // No bound and no set: everything the backing array has.
        let all: Vec<usize> = members(None, None, 3).collect();
        assert_eq!(all, vec![0, 1, 2]);

        // A set that is not a prefix — the case a bound cannot express.
        let set = [16usize, 17, 18, 31];
        let sparse: Vec<usize> = members(Some(&set), None, 32).collect();
        assert_eq!(sparse, vec![16, 17, 18, 31]);

        // The set wins over a bound: a caller that knows the exact indices
        // knows strictly more than one that knows a count.
        let both: Vec<usize> = members(Some(&set), Some(2), 32).collect();
        assert_eq!(both, vec![16, 17, 18, 31]);
    }

    /// A member set is clamped to the backing array, like a bound is.
    ///
    /// A set outliving a resize would otherwise walk past the end of the array
    /// it describes.
    #[test]
    fn a_member_set_never_walks_past_the_backing_array() {
        let set = [0usize, 1, 2, 99];
        let walked: Vec<usize> = members(Some(&set), None, 3).collect();
        assert_eq!(walked, vec![0, 1, 2], "index 99 is not in a 3-entry array");

        let empty: Vec<usize> = members(Some(&set), None, 0).collect();
        assert!(empty.is_empty());
    }

    /// `set_member_set` sorts and de-duplicates, so the walk stays in index
    /// order however the caller discovered them.
    #[test]
    fn a_declared_member_set_is_sorted_and_deduplicated() {
        static G: AcquisitionGroup = AcquisitionGroup::new("t_sparse", "t_sparse_group");
        G.set_member_set(&[31, 16, 17, 16]);
        assert_eq!(G.member_set(), Some(&[16usize, 17, 31][..]));

        // Single-init, like the bound: a second call does not race the walk.
        G.set_member_set(&[0]);
        assert_eq!(G.member_set(), Some(&[16usize, 17, 31][..]));
    }

    #[tokio::test]
    async fn snapshot_format_selects_the_builder() {
        // An empty document defaults `general` via `Default::default()`
        // (empty strings), not the field-level `#[serde(default = ...)]`
        // helpers — those only apply when a `[general]` table is present.
        // Supply an explicit (empty) table so `ttl`/`listen` get their real
        // defaults.
        let default_config: Config = toml::from_str("[general]\n").expect("valid config");
        let mut default_builder = SnapshotBuilder::new(
            Arc::new(default_config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );
        let snap = default_builder.build(Instant::now()).await;
        assert!(matches!(snap, Snapshot::V3(_)), "the default format is v3");

        // And the escape hatch still reaches the old builder.
        let v2_config: Config =
            toml::from_str("[general]\nsnapshot_format = \"v2\"\n").expect("valid config");
        let mut v2_builder = SnapshotBuilder::new(
            Arc::new(v2_config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );
        let snap = v2_builder.build(Instant::now()).await;
        assert!(matches!(snap, Snapshot::V2(_)), "v2 is still selectable");

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
