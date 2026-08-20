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
    // `&'static str`) — rather than `group_registry()`'s own
    // `"{sampler}/{name}"` joined `String` key, so building this map costs
    // no allocation at all, and neither does a per-metric lookup below
    // (a `(&str, &str)` tuple, not a freshly `format!`ed `String`).
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

        // KEEP IN SYNC with create_v3 — see the doc comment on
        // `metric_metadata` (the shared prefix) and on `create_v3` itself
        // (the per-kind walk/naming/membership rules below, which are NOT
        // shared and are duplicated independently in each function).
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

/// Per-group accumulation while walking the metriken registry: the group's
/// acquisition window (read once, at first touch — see `create_v3`) plus,
/// per kind, the (descriptor, value) pairs in schema order.
///
/// `reader_guard` is only ever `Some` for a
/// [reader-stamped](AcquisitionGroup::is_reader_stamped) group (a
/// `PackedCounters` mmap-direct group): its acquisition IS this walk's read
/// of the group's members, so first touch (the `Entry::Vacant` arm below)
/// acquires the bracket directly instead of reading a window a sampler
/// already stamped, and the group-emit loop `finish()`es it once every
/// member's value has been read — see the doc comment there.
#[derive(Default)]
struct GroupBuilder {
    window: Option<Window>,
    reader_guard: Option<AcquisitionGuard<'static>>,
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
        // Reader-stamped (`PackedCounters` mmap-direct) groups use a THIRD
        // membership mode instead of `member_bound`'s dense prefix: every
        // index with metadata registered (`load_metadata(idx).is_some()`,
        // walked via `metadata_snapshot()`) is a member — see the
        // CounterGroup/GaugeGroup arms below and the doc comment on
        // `create_v3` for the walk-cost grounding.
        let mut reader_stamped = false;
        let group_key = match metric.metadata().get("acq_group") {
            Some(acq_group) => {
                let key = format!("{sampler}/{acq_group}");
                if let Some(ag) = group_registry.get(&key) {
                    declared = true;
                    member_bound = ag.member_bound();
                    reader_stamped = ag.is_reader_stamped();
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
                e.insert(GroupBuilder {
                    window,
                    reader_guard,
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
                if reader_stamped {
                    // Reader-stamped (mmap-direct `PackedCounters`) group:
                    // membership is metadata-presence, not `0..bound` — a
                    // registered index (a live cgroup/task the sampler's
                    // ringbuf handler attached metadata to) is a member,
                    // full stop, regardless of the backing array's capacity
                    // (`MAX_CGROUPS`/`MAX_PID`, sized for the worst case —
                    // see docs/principles.md principle 6). Walking
                    // `0..entries()` here would mean sweeping all 4.2M
                    // `MAX_PID` slots every tick for `task_cpu_usage`
                    // regardless of how many tasks are actually live; see
                    // the walk-cost grounding on `create_v3`'s doc comment.
                    // `metadata_snapshot()`'s cost is O(populated), not
                    // O(entries()) — it walks metriken's own sparse
                    // `HashMap<usize, _>` metadata store, not the dense
                    // value array.
                    //
                    // `metadata_snapshot()`'s order follows that HashMap's
                    // iteration order, which is NOT stable tick-to-tick on
                    // its own (hashbrown gives no ordering guarantee) even
                    // when the populated set is unchanged — sort by index
                    // so a stable member set produces a byte-stable schema
                    // order (and therefore a `SkeletonCache` hit) across
                    // ticks, the same determinism concern the external-
                    // metrics sort below addresses for a different source.
                    let mut members = g.metadata_snapshot();
                    members.sort_by_key(|(idx, _)| *idx);
                    for (idx, m) in members {
                        // Two separate reads, same torn-recycle caveat as
                        // the non-reader-stamped arm below — see its
                        // comment.
                        let v = g.counter_value(idx);

                        let mut entry_metadata = metadata.clone();
                        entry_metadata.insert("id".to_string(), idx.to_string());
                        for (k, v) in m {
                            entry_metadata.insert(k, v);
                        }

                        group.counters.push((
                            MetricDesc {
                                name: format!("{metric_id}x{idx}"),
                                metadata: entry_metadata,
                            },
                            v,
                        ));
                    }
                    // Mark the end HERE — right after this metric's member
                    // values were actually read — not at emit time, when
                    // `finish()` runs below after the rest of the walk
                    // (every other group's schema assembly, hashing, etc.)
                    // has also happened. See `AcquisitionGuard::mark_end`.
                    // A group with several like-entity members (e.g.
                    // `cgroup_syscall`'s 16 op-class maps) touches this arm
                    // once per member; each call moves the mark forward, so
                    // the LAST touch — this group's true last member read —
                    // is what ends up published, exactly like `finish()`'s
                    // original stamp-last derivation, just decoupled from
                    // publish timing.
                    if let Some(guard) = group.reader_guard.as_mut() {
                        guard.mark_end();
                    }
                } else {
                    // Registration membership for a per-CPU (or similar)
                    // group IS the group's real member population —
                    // `possible_cpus()` for a `CpuCounters`-backed group —
                    // not the backing array's `entries()` capacity, which is
                    // a fixed implementation ceiling (`MAX_CPUS`; see
                    // docs/principles.md principle 6, "over-allocates on
                    // small machines") sized for the worst case, not this
                    // host. Walking the full capacity on every declared
                    // group would put an ~18-CPU host's tick at ~19× the
                    // entries it actually populated; walk the bound instead
                    // when one is set (clamped to `entries()` in case a
                    // stale/misconfigured bound somehow exceeds the backing
                    // array).
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
            }
            Value::GaugeGroup(g) => {
                if reader_stamped {
                    // See the identical branch on the CounterGroup arm
                    // above for the full rationale (walk-cost grounding,
                    // and why the sort is required for schema stability).
                    // No `PackedCounters`-style gauge group exists in the
                    // codebase yet, but this keeps the declared-group
                    // membership rule symmetric across both group kinds
                    // rather than leaving a silent gap for the first one
                    // that does.
                    let mut members = g.metadata_snapshot();
                    members.sort_by_key(|(idx, _)| *idx);
                    for (idx, m) in members {
                        let v = g.gauge_value(idx);

                        let mut entry_metadata = metadata.clone();
                        entry_metadata.insert("id".to_string(), idx.to_string());
                        for (k, v) in m {
                            entry_metadata.insert(k, v);
                        }

                        group.gauges.push((
                            MetricDesc {
                                name: format!("{metric_id}x{idx}"),
                                metadata: entry_metadata,
                            },
                            v,
                        ));
                    }
                    // See the CounterGroup arm above: mark the end right
                    // after THIS metric's member values were read, not at
                    // emit time.
                    if let Some(guard) = group.reader_guard.as_mut() {
                        guard.mark_end();
                    }
                } else {
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
        if group.counters.is_empty() && group.gauges.is_empty() && group.histograms.is_empty() {
            continue;
        }

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
            group_registry.get(&group_name).and_then(|ag| ag.window())
        } else {
            // Re-read the window here (after this group's values, above)
            // and reconcile with the first-touch read via
            // `resolve_walk_window` — see its doc comment for why a second
            // read is necessary.
            let latest_window = group_registry.get(&group_name).and_then(|ag| ag.window());
            resolve_walk_window(group.window, latest_window)
        };
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
            Some(cached) if *cached.schema == schema => (cached.schema.clone(), cached.hash),
            _ => {
                let hash = schema.hash();
                let schema = Arc::new(schema);
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
