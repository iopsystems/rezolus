use crate::agent::config::SnapshotFormat;
use crate::agent::external_metrics::{
    ExternalMetric, ExternalMetricValue, ExternalMetricsStore, MetricKey,
};
use crate::agent::timing::{AcquisitionGroup, AcquisitionGuard};
use crate::agent::*;

use super::router::{group_registry, RezolusRouter};

use metriken::{Value, Window};
use metriken_exposition::group_builder::{ExtraGroup, GroupBuilder, Stamp};
use metriken_exposition::{Counter, Gauge, Histogram, MetricDesc, Snapshot, SnapshotV2};

use bytes::Bytes;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

pub struct SnapshotBuilder {
    cached: Option<CachedSnapshot>,
    samplers: Arc<Box<[Box<dyn Sampler>]>>,
    ttl: Duration,
    external_store: Option<Arc<ExternalMetricsStore>>,
    format: SnapshotFormat,
    v3: V3Builder,
    /// Per group name, the schema hash most recently *emitted in a delta row
    /// body* (`/metrics/rows` without `schemas=all`).
    ///
    /// This is what lets that endpoint omit a schema that has not changed —
    /// see [`build_rows`](Self::build_rows). It tracks what the AGENT has
    /// sent, not what any particular consumer has received, which is why the
    /// endpoint also has to offer `schemas=all`: a recorder that connects
    /// mid-life, or one whose response was dropped in flight, holds no schema
    /// for a hash this map already considers emitted, and asks for a full body
    /// to recover.
    ///
    /// Bounded by the number of acquisition groups (45 on a 25-sampler host),
    /// not by generations: one entry per group name, overwritten on change.
    emitted_schemas: HashMap<String, (u64, u64)>,
    /// Completed sampling passes — see [`samples`](Self::samples).
    samples: u64,
}

struct CachedSnapshot {
    timestamp: Instant,
    snapshot: Snapshot,
    /// When this pass READ the values, on the source's timeline, and the wall
    /// clock's disagreement with it at that moment.
    ///
    /// Stamped once, at the pass, and carried by every body built from it.
    /// That is the whole point: inside the TTL a request is answered from this
    /// cache, so a body stamped when the request arrived would claim values
    /// were read up to a TTL after they actually were. The agent is the only
    /// party that knows the difference — a consumer sees one HTTP response and
    /// cannot tell a fresh pass from a cached one.
    ///
    /// `ts + wall_offset` is the same wall clock `systemtime` carries, by
    /// construction: both come from the one pair of readings taken at the pass.
    sampled_ts: i64,
    sampled_wall_offset: i64,
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
    /// The row-format bodies, in their two flavours: `rows` omits a schema
    /// whose hash the agent has already emitted, `rows_all` carries every
    /// schema. Both are per snapshot for the same reason `msgpack` is — a TTL
    /// hit should be a refcount bump, not a re-encode.
    rows: OnceLock<Bytes>,
    rows_all: OnceLock<Bytes>,
    /// The decoded, fully-schema'd rows for this snapshot, shared by every
    /// stream subscriber so one tick costs one conversion rather than one per
    /// connection. Each subscriber then clears the schemas IT has already
    /// sent, which is per-connection state and cannot be shared.
    rows_full: OnceLock<Option<Arc<crate::recorder::wire::AgentRows>>>,
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
            v3: v3_builder(),
            emitted_schemas: HashMap::new(),
            samples: 0,
        }
    }

    async fn refresh(&mut self) {
        self.samples += 1;
        let last = Instant::now();

        // One pair of readings for the pass: the anchored stamp every body
        // will carry, and the wall clock `systemtime` reports. Taken together
        // so `ts + wall_offset == systemtime` holds exactly rather than
        // approximately.
        let (sampled_ts, sampled_wall_offset) = crate::agent::epoch::anchored_now();
        let timestamp = SystemTime::UNIX_EPOCH
            + Duration::from_nanos((sampled_ts + sampled_wall_offset).max(0) as u64);

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
            SnapshotFormat::V2 => create(
                timestamp,
                duration,
                external_metrics,
                (sampled_ts, sampled_wall_offset),
            ),
            SnapshotFormat::V3 => create_v3(
                duration,
                external_metrics,
                &mut self.v3,
                (sampled_ts, sampled_wall_offset),
            ),
        };

        self.cached = Some(CachedSnapshot {
            snapshot,
            timestamp: last,
            sampled_ts,
            sampled_wall_offset,
            msgpack: OnceLock::new(),
            json: OnceLock::new(),
            rows: OnceLock::new(),
            rows_all: OnceLock::new(),
            rows_full: OnceLock::new(),
        });
    }

    /// How many sampling passes this builder has run, from either cause: a
    /// clock tick or a TTL-expired request.
    ///
    /// Exposed because "the agent did not sample" is the property that keeps
    /// #1226's observer effect away, and a property worth having is a property
    /// worth asserting — see `an_unsubscribed_agent_never_samples`. Only the
    /// tests need the raw count; an operator reads the clock's state off
    /// `/status` instead.
    #[cfg(test)]
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// Whether this agent can serve acquisition-group rows at all.
    ///
    /// False for a V2 agent, which has no groups. Checked BEFORE a stream is
    /// accepted: without it a V2 agent would take the subscription, find
    /// nothing to send on every tick, and hold an open connection emitting
    /// nothing — indistinguishable to the subscriber from an agent whose
    /// samplers are all quiet.
    pub fn serves_rows(&self) -> bool {
        matches!(self.format, SnapshotFormat::V3)
    }

    /// A snapshot for `now` as rows — sampling only if the cached one has
    /// aged past the TTL, exactly as a scrape would.
    ///
    /// **This is where a subscription's tick becomes (or does not become) a
    /// sampling pass.** Two subscriptions whose timers fall within one TTL of
    /// each other share a pass; one asking for less than the TTL gets the
    /// reading it already has rather than being allowed to sample faster than
    /// the operator configured. That makes the TTL the floor on sampling rate
    /// without a second knob to keep in agreement with it.
    ///
    /// Callers are expected to notice a repeated reading (by `wall_ns`) and
    /// act on it — the stream handler sends an EMPTY frame rather than
    /// repeating the readings, which says "your interval elapsed, nothing is
    /// new" without asserting observations that did not happen.
    pub async fn rows_at(&mut self, now: Instant) -> Option<Arc<crate::recorder::wire::AgentRows>> {
        self.build(now).await;
        self.latest_rows()
    }

    /// The most recent completed sampling pass as rows, with every schema
    /// present — or `None` before the first pass, or for a V2 agent, which has
    /// no acquisition groups to stream.
    ///
    /// Does NOT sample. A stream subscriber reads what the clock produced; it
    /// must never be able to drive a sampling pass of its own, or N
    /// subscribers would mean N passes and the whole point of one shared clock
    /// would be lost.
    pub fn latest_rows(&self) -> Option<Arc<crate::recorder::wire::AgentRows>> {
        let cached = self.cached.as_ref()?;
        cached
            .rows_full
            .get_or_init(|| {
                crate::recorder::wire::encode_snapshot(
                    &cached.snapshot,
                    cached.sampled_ts,
                    cached.sampled_wall_offset,
                )
                .ok()
                .map(Arc::new)
            })
            .clone()
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

    /// The row-format body for the current snapshot — acquisition groups as
    /// pre-encoded WAL rows rather than as a snapshot.
    ///
    /// # Why the `all` flavour exists
    ///
    /// The point of this endpoint is that it does NOT resend a schema that
    /// has not changed. On a 25-sampler host (45 groups, 3,560 declared
    /// members) schemas are 87.8% of a scrape's body — and measured end to
    /// end over 60 consecutive scrapes there, this endpoint's bodies are
    /// **2.6x smaller** than `/metrics/binary`'s (212,308 B against
    /// 559,458 B median).
    ///
    /// Not 8x, which is what removing schemas outright would give, because
    /// a default group's membership is value-derived — a counter's first
    /// non-zero tick changes its group's schema hash and forces a resend —
    /// and because each row is framed separately, costing about 1.6x a
    /// snapshot's framing at equal schema policy. Declared member sets are
    /// the lever on the first; see
    /// `docs/journal/2026-09-16-row-endpoint-schema-resend.md`.
    ///
    /// `/metrics/binary` cannot do this and must keep resending. Its body is
    /// contractually self-contained: `record --format raw` writes those bodies
    /// verbatim to a file that `recording convert` decodes offline, much
    /// later, with no access to whatever earlier scrape carried the schema.
    /// Only a consumer that keeps state across scrapes can be told "the same
    /// schema as last time", and a recorder is the one consumer that does.
    ///
    /// Which is also why omission cannot be unconditional. [`emitted_schemas`]
    /// tracks what this AGENT has sent, not what any given consumer holds, so
    /// a recorder that connects mid-life — or one whose response was dropped
    /// in flight — would be handed a reference to a schema it has never seen.
    /// `all = true` is its way back: it asks for a full body, learns every
    /// schema, and resumes taking delta bodies. That makes the failure
    /// recoverable in one tick without the agent having to track sessions.
    ///
    /// [`emitted_schemas`]: Self::emitted_schemas
    pub async fn build_rows(&mut self, now: Instant, all: bool) -> Result<Bytes, String> {
        self.build(now).await;

        // Cache hit: this snapshot's body for this flavour is already encoded.
        {
            let cached = self.cached.as_ref().expect("build populates the cache");
            let slot = if all { &cached.rows_all } else { &cached.rows };
            if let Some(bytes) = slot.get() {
                return Ok(bytes.clone());
            }
        }

        let mut rows = {
            let cached = self.cached.as_ref().expect("build populates the cache");
            crate::recorder::wire::encode_snapshot(
                &cached.snapshot,
                cached.sampled_ts,
                cached.sampled_wall_offset,
            )?
        };

        if !all {
            // Drop a schema the agent has already put on the wire for this
            // group at this hash, and record the ones it has not. Note this
            // advances on BODY CONSTRUCTION rather than on delivery — see the
            // recovery path above for why that is safe rather than merely
            // convenient.
            for row in &mut rows.rows {
                match self.emitted_schemas.get(&row.stream) {
                    Some(hash) if *hash == row.schema_hash => row.schema = None,
                    _ => {
                        self.emitted_schemas
                            .insert(row.stream.clone(), row.schema_hash);
                    }
                }
            }
        }

        let bytes = Bytes::from(crate::recorder::wire::encode(&rows)?);
        let cached = self.cached.as_ref().expect("build populates the cache");
        let slot = if all { &cached.rows_all } else { &cached.rows };
        let _ = slot.set(bytes.clone());
        Ok(bytes)
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

/// V2's metadata for one metric: `"metric"` (its name), the metric's static
/// metadata, then `"sampler"` attribution, which wins over a static
/// `sampler` key. Also returns the sampler name.
///
/// V3 builds the same prefix in metriken's `GroupBuilder` (the metric name,
/// the static metadata, then `RezolusRouter::annotate`), so the two formats
/// label a member identically. A change here must be made there too.
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
    stamp: (i64, i64),
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
            // Rides on every snapshot, not just on `/status`, because it is
            // what lets a consumer notice the agent restarted BETWEEN two
            // scrapes: at that moment every counter in the payload restarted
            // from zero together, and the values alone cannot say so.
            (
                "producer_epoch".to_string(),
                crate::agent::epoch::producer_epoch().to_string(),
            ),
            // The timeline `systemtime` sits on. A consumer that has this can
            // place a reading without trusting its own clock to agree with
            // this host's, and can tell a wall-clock step from elapsed time:
            // `systemtime` moves with a step, the anchor does not.
            //
            // Carried here as well as on `/status` because a snapshot is the
            // whole of what some consumers ever read, and an anchor fetched
            // separately could belong to a different run of the agent.
            (
                "clock_anchor_wall_ns".to_string(),
                crate::agent::epoch::clock_anchor_wall_ns().to_string(),
            ),
            // The pass's own stamp on that timeline, and the wall clock's
            // disagreement with it at the read. `ts + wall_offset ==
            // systemtime`, so a consumer that keeps these keeps the moment the
            // agent READ the values — which is not the moment it answered, and
            // not the moment the answer arrived.
            ("ts".to_string(), stamp.0.to_string()),
            ("wall_offset".to_string(), stamp.1.to_string()),
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

        // The metadata prefix V3 builds too; see `metric_metadata`.
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

/// The V3 builder: metriken's `GroupBuilder` driven by the agent's router
/// ([`RezolusRouter`], which documents routing, membership and windows).
///
/// It owns the skeleton cache, which reuses a group's schema and hash while
/// the group's membership and per-slot metadata version are unchanged, so it
/// lives as long as the `SnapshotBuilder`. See metriken-exposition's
/// `group_builder` for the cache's rules.
pub(crate) type V3Builder = GroupBuilder<RezolusRouter>;

pub(crate) fn v3_builder() -> V3Builder {
    GroupBuilder::new(RezolusRouter::new())
}

/// Build a `SnapshotV3` (acquisition-group snapshot) from the metriken
/// registry and the external metrics store.
///
/// Registry metrics are grouped by `builder`; external metrics become the
/// `external/main` group (see [`external_group`]). `GroupBuilder::snapshot`
/// adds `producer_epoch`, `clock_anchor_wall_ns`, `ts` and `wall_offset` to
/// the agent's `source` and `version`, and sets `systemtime` to
/// `ts + wall_offset`. The epoch and anchor are `metriken::epoch`'s, which
/// is what `crate::agent::epoch` re-exports.
fn create_v3(
    duration: Duration,
    external_metrics: Vec<ExternalMetric>,
    builder: &mut V3Builder,
    stamp: (i64, i64),
) -> Snapshot {
    #[cfg(test)]
    let _serialize = BUILDER_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let extra = external_group(external_metrics).into_iter().collect();
    let metadata = [
        ("source".to_string(), env!("CARGO_BIN_NAME").to_string()),
        ("version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
    ]
    .into();
    let stamp = Stamp {
        ts: stamp.0,
        wall_offset: stamp.1,
    };
    Snapshot::V3(builder.snapshot(stamp, duration, extra, metadata))
}

/// External (pushed) metrics as the windowless `external/main` group, or
/// `None` when there are none.
///
/// Members are named `external/{name}#{labels_hash:016x}` and sorted by that
/// name, so the schema is deterministic whatever order the store returns
/// them in. A histogram whose buckets do not form a valid histogram is left
/// out. Each metric's own window is dropped: a group carries one window, and
/// pushed metrics have no shared acquisition.
fn external_group(external_metrics: Vec<ExternalMetric>) -> Option<ExtraGroup> {
    if external_metrics.is_empty() {
        return None;
    }

    let mut entries: Vec<(String, ExternalMetric)> = external_metrics
        .into_iter()
        .map(|metric| {
            let labels_hash = MetricKey::new(&metric.name, &metric.labels).labels_hash;
            let entry_name = format!("external/{}#{labels_hash:016x}", metric.name);
            (entry_name, metric)
        })
        .collect();
    entries.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut group = ExtraGroup {
        namespace: "external".to_string(),
        name: "main".to_string(),
        window: None,
        counters: Vec::new(),
        gauges: Vec::new(),
        histograms: Vec::new(),
    };

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
                let desc = MetricDesc {
                    name: entry_name,
                    metadata,
                };
                group.counters.push((desc, Some(v)));
            }
            ExternalMetricValue::Gauge(v) => {
                let desc = MetricDesc {
                    name: entry_name,
                    metadata,
                };
                group.gauges.push((desc, Some(v)));
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
                    let desc = MetricDesc {
                        name: entry_name,
                        metadata,
                    };
                    group.histograms.push((desc, Some(hv)));
                }
            }
        }
    }

    Some(group)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::external_metrics::{ExternalMetric, ExternalMetricValue};
    use metriken::metric;
    use metriken::Window;
    use metriken_exposition::{GroupSchema, GroupSnapshot};
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

    #[metric(name = "snapshot_sampler_label_probe")]
    static SAMPLER_LABEL_PROBE: metriken::Counter = metriken::Counter::new();

    #[test]
    fn built_snapshot_metric_carries_a_sampler_label() {
        SAMPLER_LABEL_PROBE.increment();
        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![], (0, 0));
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
        let snap = create(SystemTime::now(), Duration::from_secs(5), vec![ext], (0, 0));
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
        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![], (0, 0));
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

        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![], (0, 0));
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

        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![], (0, 0));
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

        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![], (0, 0));
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

        let v2 = create(SystemTime::now(), Duration::from_secs(1), vec![], (0, 0));
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

        let mut cache = v3_builder();
        let v3 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

        let mut cache = v3_builder();
        let v3 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

        let mut cache = v3_builder();
        let snap = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

        let mut cache = v3_builder();
        let snap = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

        let mut cache = v3_builder();
        let snap1 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

        let snap2 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
        let mut cache = v3_builder();
        let snap = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

    // A reader-stamped group with two metrics, populated at non-contiguous slots — the shape
    // `cpu_usage/cpu_usage_task` has.
    static V3_SLOT_ORDER_GROUP: AcquisitionGroup =
        AcquisitionGroup::new_reader_stamped("unattributed", "slot_order_probe");

    #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
    static V3_SLOT_ORDER_GROUP_ENTRY: &'static AcquisitionGroup = &V3_SLOT_ORDER_GROUP;

    #[metric(
        name = "snapshot_v3_slot_order_a",
        metadata = { acq_group = "slot_order_probe" }
    )]
    static V3_SLOT_ORDER_A: metriken::CounterGroup = metriken::CounterGroup::new(16);

    #[metric(
        name = "snapshot_v3_slot_order_b",
        metadata = { acq_group = "slot_order_probe" }
    )]
    static V3_SLOT_ORDER_B: metriken::CounterGroup = metriken::CounterGroup::new(16);

    /// Two properties a consumer of a group's rows depends on, neither of
    /// which is enforced by a type today.
    ///
    /// **Within a metric, members come out in ascending slot order.** A row's
    /// values vector is positional, so a consumer reading value `i` as
    /// belonging to the `i`-th sorted live slot is right only if the producer
    /// emitted them that way.
    ///
    /// **Every metric of a group agrees on what a slot means**, so one slot
    /// has one label set for the whole group. Samplers
    /// do this by looping over their metric list
    /// (`cpu/linux/usage/mod.rs`'s `handle_task_info` inserts the same
    /// metadata for every metric in `TASK_METRICS`, and `handle_task_exit`
    /// clears them together), but nothing makes them. A sampler that labelled
    /// one metric and not another would give a slot two meanings.
    #[test]
    fn a_groups_metrics_agree_on_slot_order_and_on_what_each_slot_means() {
        // Non-contiguous on purpose: membership here is metadata-presence, so
        // the emitted order is whatever the walk produces, not 0..n.
        for (slot, comm) in [(9usize, "nine"), (2, "two"), (13, "thirteen")] {
            for metric in [&V3_SLOT_ORDER_A, &V3_SLOT_ORDER_B] {
                metric.set(slot, 1);
                metric.set_metadata(
                    slot,
                    [
                        ("comm".to_string(), comm.to_string()),
                        ("cgroup".to_string(), "/system.slice".to_string()),
                    ]
                    .into(),
                );
            }
        }

        let guard = V3_SLOT_ORDER_GROUP.acquire();
        guard.finish();

        let mut cache = v3_builder();
        let Snapshot::V3(snap) = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0))
        else {
            panic!("expected V3")
        };
        let group = snap
            .groups
            .iter()
            .find(|g| g.name == "unattributed/slot_order_probe")
            .expect("probe group present");
        let schema = group.schema.as_ref().expect("schema present");

        // `MetricDesc.name` is `"{metric_id}x{slot}"` for a group member.
        let mut by_metric: std::collections::BTreeMap<&str, Vec<u32>> = Default::default();
        // What one metric says a slot means: the metric's name, and the
        // identity the sampler attached to that slot.
        type SlotClaim<'a> = (&'a str, BTreeMap<&'a str, &'a str>);
        let mut labels_by_slot: std::collections::BTreeMap<u32, Vec<SlotClaim<'_>>> =
            Default::default();
        for desc in &schema.counters {
            let Some(metric) = desc.metadata.get("metric") else {
                continue;
            };
            if !metric.starts_with("snapshot_v3_slot_order_") {
                continue;
            }
            let slot: u32 = desc
                .name
                .split_once('x')
                .expect("a group member's name is `{metric_id}x{slot}`")
                .1
                .parse()
                .expect("the slot half parses");
            by_metric.entry(metric).or_default().push(slot);

            // Everything the sampler attached, which is what identity is. The
            // keys create_v3 adds itself (`metric`, `sampler`, `id`) are not
            // per-slot and do not belong in an index entry.
            let identity: BTreeMap<&str, &str> = desc
                .metadata
                .iter()
                .filter(|(k, _)| !matches!(k.as_str(), "metric" | "sampler" | "id"))
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            labels_by_slot
                .entry(slot)
                .or_default()
                .push((metric, identity));
        }

        assert_eq!(by_metric.len(), 2, "both metrics emitted members");
        for (metric, slots) in &by_metric {
            let mut sorted = slots.clone();
            sorted.sort_unstable();
            assert_eq!(
                slots, &sorted,
                "`{metric}` emitted members out of slot order: {slots:?}"
            );
            assert_eq!(slots, &vec![2, 9, 13], "the populated slots, ascending");
        }

        for (slot, seen) in &labels_by_slot {
            let (first_metric, first) = &seen[0];
            // Without this the comparison below could hold vacuously: if the
            // filter above ever stripped every key, two empty maps are equal.
            let expected = match slot {
                2 => "two",
                9 => "nine",
                13 => "thirteen",
                other => panic!("unexpected slot {other}"),
            };
            assert_eq!(
                first.get("comm").copied(),
                Some(expected),
                "slot {slot} carries the identity the sampler attached"
            );
            for (metric, identity) in &seen[1..] {
                assert_eq!(
                    identity, first,
                    "slot {slot} means one thing to `{first_metric}` and another \
                     to `{metric}`; an index entry has one place to put it"
                );
            }
        }
        assert_eq!(labels_by_slot.len(), 3, "every populated slot was seen");
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
        // unchanged `schema_hash`. Folding each group metric's metadata
        // version into the cache identity (metriken-exposition's
        // `GroupBuilder`) closes that hole: the metadata change must be
        // visible in the emitted `MetricDesc` AND must
        // change the schema hash, or a receiver caching parsed schemas by
        // `(name, schema_hash)` would bind the new values to dead labels
        // indefinitely.
        V3_RECYCLE_COUNTERS.set(0, 1);
        V3_RECYCLE_COUNTERS.set_metadata(0, [("comm".to_string(), "old_task".to_string())].into());

        let guard = V3_RECYCLE_GROUP.acquire();
        guard.finish();

        let mut cache = v3_builder();
        let snap1 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

        let snap2 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

    /// How many hit ticks to measure before taking the minimum. See
    /// [`warm_then_measure_hit`] for why the minimum is the right estimator.
    const HIT_SAMPLES: usize = 5;

    /// Warm `cache`, then measure a hit tick [`HIT_SAMPLES`] times and return
    /// the **smallest** count, with one of the snapshots.
    ///
    /// The warm tick is necessarily a miss for anything that changed since the
    /// last call using this `cache` — this fixture right after
    /// `grow_alloc_probe` moved its member bound, for instance — so it is not
    /// measured.
    ///
    /// # Why the minimum, and not one sample
    ///
    /// `create_v3` walks the WHOLE metriken registry, not just this fixture.
    /// `BUILDER_TEST_LOCK` stops two builder walks overlapping, but it does not
    /// stop other tests MUTATING metric state between walks — registering a
    /// metric, moving a member set, inserting group metadata. Any such mutation
    /// invalidates that group's skeleton-cache entry, so the next measured walk
    /// pays a real miss for a group this test never touched, and a miss
    /// allocates.
    ///
    /// That interference is **one-sided**: it can only ADD allocations. Nothing
    /// another test does can make this walk cheaper than a clean all-hit walk,
    /// because the hit path is already the cheapest path through the builder.
    /// So the minimum over a few samples converges on the clean cost, while a
    /// single sample is whatever the rest of the suite happened to be doing at
    /// that instant.
    ///
    /// This replaces a wide tolerance. The margin was set against locally
    /// observed noise of ~40 and CI produced 382 on a PR that could not reach
    /// this code (#1232) — widening it further would have meant a test that
    /// cannot fail for the reason it exists. Sampling removes the noise instead
    /// of accommodating it, which is what lets the margin below be tight enough
    /// to mean something.
    fn warm_then_measure_hit(cache: &mut V3Builder) -> (Snapshot, usize) {
        let _ = create_v3(Duration::from_secs(1), vec![], cache, (0, 0));

        let mut best = usize::MAX;
        let mut snapshot = None;
        for _ in 0..HIT_SAMPLES {
            let (snap, allocs) =
                count_allocations(|| create_v3(Duration::from_secs(1), vec![], cache, (0, 0)));
            best = best.min(allocs);
            snapshot = Some(snap);
        }
        (
            snapshot.expect("HIT_SAMPLES is non-zero, so a snapshot was taken"),
            best,
        )
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
        let mut cache = v3_builder();

        // Settle the PROCESS before phase 1 measures anything.
        //
        // `create_v3` walks the whole metriken registry, and a good deal of
        // what it touches is lazily initialized on first use — metric storage,
        // group registries, the per-sampler attribution map. Whichever
        // measurement runs first pays for all of it.
        //
        // That is why this test failed on GitHub's runners while passing on a
        // 32-core Linux box and on macOS: the two phases are compared against
        // each other, so anything one-time landing in phase 1 reads as phase 2
        // being *cheaper*, and the margin is two-sided. Measured on CI, phase 1
        // came in ~382 allocations above phase 2 — consistently, not randomly.
        //
        // A throwaway cache, so this does not warm the one under test: phase 1
        // must still see its own first tick as a miss.
        {
            let mut settle = v3_builder();
            for _ in 0..3 {
                let _ = create_v3(Duration::from_secs(1), vec![], &mut settle, (0, 0));
            }
        }

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
        let (snap_large_again, _) =
            count_allocations(|| create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0)));
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
        // Both counts are the MINIMUM over `HIT_SAMPLES` hit ticks, so the
        // one-sided interference described on `warm_then_measure_hit` has
        // been sampled out rather than tolerated. What remains is the real
        // difference between a hit tick over 8 members and one over 512, and
        // the claim under test is that there is none: the hit path reuses the
        // cached `Arc<GroupSchema>` and touches no per-member allocation.
        //
        // The margin is therefore small on purpose. Before the change this
        // test pins, growing 504 more members cost a `MetricDesc` (`String`
        // name + `BTreeMap`) AND an `entry_metadata` `HashMap` per member —
        // on the order of 1,500+ extra allocations. A margin of 100 sits an
        // order of magnitude below that and no longer has to absorb whatever
        // the rest of the suite was doing at that instant.
        let delta = large_allocs.abs_diff(small_allocs);
        assert!(
            delta <= 100,
            "growing this fixture from {ALLOC_TEST_SMALL_N} to {ALLOC_TEST_LARGE_N} members \
             changed hit-tick allocations by {delta} ({small_allocs} -> {large_allocs}), \
             each the minimum over {HIT_SAMPLES} hit ticks — expected ~0. A per-member \
             allocation regression would move this by well over a thousand; a delta in \
             the low hundreds instead means the hit path started doing per-member work \
             that is cheap but not free"
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

        let mut cache = v3_builder();
        let snap1 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
        let snap2 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
        let snap3 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
        let snap4 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
        let mut cache = v3_builder();
        let snap = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
        let mut cache = v3_builder();
        let snap = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
    // one declared, one not.
    //
    // metriken 0.11 gave `CounterGroup` the `u64::MAX` unwritten sentinel that
    // `GaugeGroup` always had, so an untouched index now reads `None` rather
    // than the `Some(0)` the eagerly zero-initialized array used to give. That
    // removes the ambiguity this test used to work around: "a member that is
    // genuinely present and reads zero" and "a phantom slot that merely exists
    // because the backing array got initialized" are now distinguishable at the
    // group API, so the V3 walk can report the second as absent instead of
    // publishing a zero a consumer cannot tell from a measurement.
    //
    // The CONTRACT difference this pins is unchanged in shape: a declared group
    // walks its registered membership and emits index 1 (now honestly `None`),
    // while an otherwise-identical undeclared group suppresses the entry
    // entirely (V2's transitional value-sentinel skip, default groups only).
    // Registration membership is still what decides whether the entry appears
    // at all — the sentinel only decides what it says.
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

        let mut cache = v3_builder();
        let snap = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
            declared.counters[idx1], None,
            "index 1 is included by registration but reads as absent, not as a \
             zero a consumer would take for a measurement (metriken 0.11 sentinel)"
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

        let mut cache = v3_builder();
        let snap1 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
        let snap2 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

        let mut cache = v3_builder();
        let snap = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

        let mut cache = v3_builder();
        let snap = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
            (0, 0),
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
            (0, 0),
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

        let mut cache = v3_builder();
        let snap1 = create_v3(
            Duration::from_secs(1),
            vec![make(labels_a.clone(), 1), make(labels_b.clone(), 2)],
            &mut cache,
            (0, 0),
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
            Duration::from_secs(1),
            vec![make(labels_b.clone(), 2), make(labels_a.clone(), 1)],
            &mut cache,
            (0, 0),
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

        let mut cache = v3_builder();
        let snap1 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
        let snap2 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
                    let mut cache = v3_builder();
                    for _ in 0..25 {
                        let _ = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

        let mut cache = v3_builder();
        let snap = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

        let mut cache = v3_builder();
        let snap = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
        let snap2 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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
        let snap3 = create_v3(Duration::from_secs(1), vec![], &mut cache, (0, 0));
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

        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![], (0, 0));
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

        let snap = create(SystemTime::now(), Duration::from_secs(1), vec![], (0, 0));
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
    /// A body says when the values were READ, not when the request arrived.
    ///
    /// Inside the TTL a request is answered from the cache, and a consumer
    /// sees one HTTP response either way — it cannot tell a fresh pass from a
    /// cached one. So an agent that stamped at request time would date values
    /// up to a TTL later than they were read, and only the agent is in a
    /// position to know the difference. (For a stream it is worse: there is no
    /// consumer request at all, only whenever the frame was sent.)
    ///
    /// The lag is what makes this a real assertion rather than a tautology
    /// about cached bytes: stamping at request time would put `ts` at roughly
    /// now, and this requires it to be behind now by the time that has passed
    /// since the pass.
    #[tokio::test]
    async fn a_body_is_stamped_when_it_was_sampled_not_when_it_was_asked_for() {
        let config: Config = toml::from_str("[general]\nttl = \"60s\"\n").expect("valid config");
        let mut builder = SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );

        let now = Instant::now();
        let _ = builder.build_msgpack(now).await;
        let sampled_ts = builder.cached.as_ref().expect("a pass ran").sampled_ts;

        std::thread::sleep(Duration::from_millis(50));
        let later = crate::agent::epoch::anchored_ts(Instant::now());
        // The same cached pass, requested again well after it ran.
        let _ = builder.build_msgpack(now).await;
        let again = builder.cached.as_ref().expect("still cached").sampled_ts;

        assert_eq!(again, sampled_ts, "one pass, one stamp");
        assert!(
            later - sampled_ts >= 40_000_000,
            "the stamp must lag the request by the age of the pass; it is only \
             {} ns behind",
            later - sampled_ts
        );
    }

    /// The two clocks a snapshot carries agree by construction.
    ///
    /// `systemtime` is the wall clock and `ts` is the anchored timeline, and a
    /// consumer converts between them with `wall_offset`. They come from one
    /// pair of readings taken at the pass, so the identity is exact rather
    /// than approximate — a consumer can use either and get the same answer.
    #[tokio::test]
    async fn the_anchored_stamp_and_the_wall_clock_agree() {
        let config: Config = toml::from_str("[general]\nttl = \"60s\"\nsnapshot_format = \"v3\"\n")
            .expect("valid config");
        let mut builder = SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );

        let snapshot = builder.build(Instant::now()).await.clone();
        let cached = builder.cached.as_ref().expect("a pass ran");
        let Snapshot::V3(v3) = &snapshot else {
            panic!("a v3 snapshot")
        };
        let wall = v3
            .systemtime
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("after the epoch")
            .as_nanos() as i64;
        assert_eq!(
            cached.sampled_ts + cached.sampled_wall_offset,
            wall,
            "ts + wall_offset is systemtime"
        );
        assert_eq!(
            v3.metadata.get("clock_anchor_wall_ns").map(String::as_str),
            Some(
                crate::agent::epoch::clock_anchor_wall_ns()
                    .to_string()
                    .as_str()
            ),
            "and the anchor that timeline is relative to rides along"
        );
    }

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

    /// The row endpoint exists to stop resending schemas that have not
    /// changed. On a 25-sampler host that is 87.8% of the body (45 groups,
    /// 3,560 declared members), so this is the behaviour the whole endpoint is
    /// for — not an optimization on top of it.
    #[tokio::test]
    async fn the_row_body_stops_resending_a_schema_that_has_not_changed() {
        let config: Config = toml::from_str("[general]\nttl = \"0s\"\nsnapshot_format = \"v3\"\n")
            .expect("valid config");
        let mut builder = SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );

        let now = Instant::now();
        let first =
            crate::recorder::wire::decode(&builder.build_rows(now, false).await.expect("v3 agent"))
                .expect("a decodable body");
        assert!(
            !first.rows.is_empty(),
            "the test binary registers metrics, so there are groups to serve"
        );
        assert!(
            first.rows.iter().all(|r| r.schema.is_some()),
            "the first body must teach the consumer every schema"
        );

        // A later scrape of the same agent, past the TTL so it really
        // re-samples rather than returning the cached body.
        let later = now + Duration::from_secs(1);
        let second = crate::recorder::wire::decode(
            &builder.build_rows(later, false).await.expect("v3 agent"),
        )
        .expect("a decodable body");

        for row in &second.rows {
            let before = first.rows.iter().find(|r| r.stream == row.stream);
            if let Some(before) = before {
                if before.schema_hash == row.schema_hash {
                    assert!(
                        row.schema.is_none(),
                        "group {} repeated an unchanged schema",
                        row.stream
                    );
                }
            }
        }
        // ...and the values are still there: omitting the schema must not
        // have omitted the reading it describes.
        assert!(
            second.rows.iter().all(|r| !r.row.is_empty()),
            "every row must still carry its payload"
        );
    }

    /// `schemas=all` is the recovery path: a recorder that connects mid-life
    /// holds no schema for a hash the agent already considers emitted, and
    /// this is how it catches up. So it must carry every schema regardless of
    /// what the delta body has already sent.
    #[tokio::test]
    async fn schemas_all_carries_every_schema_even_after_a_delta_body() {
        let config: Config = toml::from_str("[general]\nttl = \"0s\"\nsnapshot_format = \"v3\"\n")
            .expect("valid config");
        let mut builder = SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );

        let now = Instant::now();
        // Teach the agent that it has emitted every schema...
        let _ = builder.build_rows(now, false).await.expect("v3 agent");
        // ...then make sure a full body is still full.
        let later = now + Duration::from_secs(1);
        let full = crate::recorder::wire::decode(
            &builder.build_rows(later, true).await.expect("v3 agent"),
        )
        .expect("a decodable body");

        assert!(!full.rows.is_empty());
        assert!(
            full.rows.iter().all(|r| r.schema.is_some()),
            "schemas=all must be unconditional, or a late recorder cannot recover"
        );
    }

    /// The two flavours are cached separately per snapshot. Sharing one slot
    /// would hand a delta body to a recorder that asked for a full one — which
    /// is exactly the request it makes when it is already stuck.
    #[tokio::test]
    async fn the_two_row_flavours_do_not_share_a_cache_slot() {
        let config: Config = toml::from_str("[general]\nttl = \"60s\"\nsnapshot_format = \"v3\"\n")
            .expect("valid config");
        let mut builder = SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );

        let now = Instant::now();
        let delta = builder.build_rows(now, false).await.expect("v3 agent");
        let full = builder.build_rows(now, true).await.expect("v3 agent");
        let full_again = builder.build_rows(now, true).await.expect("v3 agent");

        assert_ne!(
            delta.as_ptr(),
            full.as_ptr(),
            "the two flavours are different bodies"
        );
        assert_eq!(
            full.as_ptr(),
            full_again.as_ptr(),
            "a cache hit must hand back the same buffer rather than re-encoding"
        );
    }

    /// **The TTL is the floor on sampling rate.** A subscription ticking
    /// faster than the TTL is handed the reading it already has rather than
    /// being allowed to drive the samplers — which is what makes a separate
    /// "minimum interval" knob unnecessary, and what stops a remote subscriber
    /// having any say over how hard this agent works.
    #[tokio::test]
    async fn a_tick_inside_the_ttl_does_not_sample_again() {
        let config: Config = toml::from_str("[general]\nttl = \"60s\"\nsnapshot_format = \"v3\"\n")
            .expect("valid config");
        let mut builder = SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );

        let now = Instant::now();
        let first = builder.rows_at(now).await.expect("v3 agent");
        assert_eq!(builder.samples(), 1);

        // Nine more ticks well inside the TTL — a subscription asking for far
        // less than the agent will give it.
        for i in 1..10 {
            let rows = builder
                .rows_at(now + Duration::from_millis(i))
                .await
                .expect("v3 agent");
            assert_eq!(
                rows.wall_ns, first.wall_ns,
                "the same reading, not a new one"
            );
        }
        assert_eq!(
            builder.samples(),
            1,
            "ticking faster than the TTL must not cost sampling passes"
        );
    }

    /// ...and past the TTL it does sample, so the floor is a floor rather than
    /// a cache that never expires.
    #[tokio::test]
    async fn a_tick_past_the_ttl_samples_again() {
        let config: Config =
            toml::from_str("[general]\nttl = \"10ms\"\nsnapshot_format = \"v3\"\n")
                .expect("valid config");
        let mut builder = SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );

        let now = Instant::now();
        builder.rows_at(now).await.expect("v3 agent");
        builder
            .rows_at(now + Duration::from_millis(500))
            .await
            .expect("v3 agent");
        assert_eq!(builder.samples(), 2);
    }

    /// Two subscriptions whose ticks fall within one TTL of each other share a
    /// single sampling pass. This is the property that lets hindsight, a
    /// recorder and a live viewer watch one agent without multiplying its
    /// cost — and with per-subscription timers it comes from the TTL rather
    /// than from a shared clock.
    #[tokio::test]
    async fn two_subscriptions_ticking_together_share_one_pass() {
        let config: Config = toml::from_str("[general]\nttl = \"1s\"\nsnapshot_format = \"v3\"\n")
            .expect("valid config");
        let mut builder = SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );

        let now = Instant::now();
        // A 5s subscription and a 97s one whose boundaries happen to coincide.
        let a = builder.rows_at(now).await.expect("v3 agent");
        let b = builder
            .rows_at(now + Duration::from_micros(200))
            .await
            .expect("v3 agent");

        assert_eq!(a.wall_ns, b.wall_ns, "both saw the same reading");
        assert_eq!(builder.samples(), 1, "and it was sampled once");
    }

    /// A subscription asking faster than the TTL gets UPDATES at the TTL's
    /// rate, not at its own. It still gets a frame per interval — an empty one
    /// where there is nothing new — so what the TTL bounds is the readings,
    /// not the cadence.
    ///
    /// Models what the stream handler does: tick, ask, and carry rows only
    /// when the reading is one it has not already sent.
    #[tokio::test]
    async fn asking_faster_than_the_ttl_yields_updates_at_the_ttl_rate() {
        let config: Config =
            toml::from_str("[general]\nttl = \"100ms\"\nsnapshot_format = \"v3\"\n")
                .expect("valid config");
        let mut builder = SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );

        // A second of ticks at 10ms — a subscriber asking ten times faster
        // than this agent will sample.
        let start = Instant::now();
        let mut updates = 0usize;
        let mut last: Option<u64> = None;
        for i in 0..100 {
            let rows = builder
                .rows_at(start + Duration::from_millis(i * 10))
                .await
                .expect("v3 agent");
            if last != Some(rows.wall_ns) {
                last = Some(rows.wall_ns);
                updates += 1;
            }
        }

        // The exact count is deliberately not asserted. `refresh` stamps the
        // cache with the real `Instant::now()`, so how far this loop's
        // synthetic clock gets ahead of it depends on how long a sampling pass
        // takes in a debug build — noise, not behaviour.
        //
        // What IS behaviour: one update per sampling pass, in both directions.
        // No pass goes unreported (data would be lost) and no update is
        // reported without one (that is the duplicate the empty frame
        // replaces).
        assert_eq!(
            builder.samples() as usize,
            updates,
            "expected one update per sampling pass; {updates} updates, {} passes",
            builder.samples()
        );
        assert!(
            updates >= 2,
            "the TTL must expire at least twice over this span"
        );
        assert!(
            updates < 20,
            "100 ticks at a tenth of the TTL must coalesce heavily; {updates} updates"
        );
    }

    /// A V2 agent must be refused a stream rather than handed an open
    /// connection that never produces a frame — which is what the loop would
    /// do, since every tick would find nothing to send.
    #[tokio::test]
    async fn a_v2_agent_cannot_serve_rows_at_all() {
        let v2: Config =
            toml::from_str("[general]\nsnapshot_format = \"v2\"\n").expect("valid config");
        let builder = SnapshotBuilder::new(
            Arc::new(v2),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );
        assert!(!builder.serves_rows());

        let v3: Config =
            toml::from_str("[general]\nsnapshot_format = \"v3\"\n").expect("valid config");
        let builder = SnapshotBuilder::new(
            Arc::new(v3),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );
        assert!(builder.serves_rows());
    }

    /// A V2 agent has no acquisition groups, so it cannot answer this
    /// question at all. It must say so rather than serve an empty body, which
    /// a recorder could not tell from an agent with every sampler disabled.
    #[tokio::test]
    async fn a_v2_agent_refuses_the_row_endpoint() {
        let config: Config =
            toml::from_str("[general]\nsnapshot_format = \"v2\"\n").expect("valid config");
        let mut builder = SnapshotBuilder::new(
            Arc::new(config),
            Arc::new(Vec::<Box<dyn Sampler>>::new().into_boxed_slice()),
            None,
        );
        assert!(builder.build_rows(Instant::now(), false).await.is_err());
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

#[cfg(test)]
#[path = "v3_contract.rs"]
mod v3_contract;
