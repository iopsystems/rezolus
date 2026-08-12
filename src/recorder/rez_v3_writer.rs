//! The `.rez` v3 writer thread. See docs/journal/2026-08-12-rez-sqlite-container.md.
//!
//! Same shape as the v2 writer (`rez_stream.rs`): a dedicated thread behind a
//! bounded channel, so encoding a large segment cannot skew the scrape cadence
//! and a disk that cannot keep up backpressures the loop instead of growing
//! memory — with one bounded exception, unlike v2: a seal batch is encoded
//! whole before its transaction opens, so its segments' bytes are all resident
//! at once (see `seal_batch`). Everything the tar container needed to fake
//! transactions is gone —
//! no `.partial`, no rename, no rename-aside, no checkpoint manifests, no
//! two-sync ordering protocol. **A seal batch is one transaction**, and the
//! file at `path` is a valid, openable `.rez` from the moment `create` returns.
//!
//! Contract: PANIC-FREE — every fallible op returns `Err`. The global panic
//! hook (`src/main.rs`) prints and calls `process::exit(101)` BEFORE
//! unwinding, so a panic here never reaches the send-error path, skips
//! finalize, and in wrapped mode orphans the child.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread::JoinHandle;

use metriken_exposition::Snapshot;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::rez::{dedup_key, group_by_sampler, write_table_parquet, Entry};
use super::rez_sqlite::{RecordingMeta, RezDb, SegmentMeta, WalRow};
use super::rez_stream::{drain_due, BuilderState, SealPolicy};

/// One sealed segment handed to the writer thread. Shared with the v2 writer:
/// it is a plain `(sampler, RezTable)` carrier with no container-specific
/// content, and a second definition would only be a second thing to keep in
/// step with `TableBuilder::finish`.
pub(crate) use super::rez_stream::SealJob;

/// Everything known when the recording starts. v3 has no manifest and no
/// per-recording tar directory — a recording IS a row in `recordings` — so the
/// v2 writer's `ManifestSeed` is exactly `RecordingMeta` here, minus `dir`.
/// The name is kept so the two writers read alike at their call sites.
///
/// **`dir` was also a display name, and v3 owes its consumers a substitute.**
/// Besides naming the tar directory it was the user-visible recording name in
/// two places: `parquet metadata`'s `recording {dir} [labels]` line
/// (`src/parquet_tools/metadata.rs`) and the viewer's per-capture display
/// filename (`src/rez_reader.rs` → `capture_registry.rs`). Both should derive
/// one from what v3 stores instead — `rez::recording_dir_slug(&labels)`, which
/// is what produced `dir` in the first place, or `recording {id}` — rather
/// than reintroduce the field. A/B aliasing is NOT affected: both viewer paths
/// alias baseline/experiment on `arm`/`host` labels only, and `labels` survives
/// verbatim in the `recordings` row.
pub(crate) type ManifestSeed = RecordingMeta;

enum Msg {
    /// One tick's WAL rows, across all samplers — one transaction.
    Wal(Vec<WalRow>),
    /// One seal batch = one transaction.
    Seal(Vec<SealJob>),
    /// The loop's last clock observation; marks the recording complete.
    Finalize { clock_offset: (u64, i64) },
}

/// Handle to the writer thread. Every fallible hand-off reports the writer's
/// stored error, in the required order: send-failure → join → report.
pub(crate) struct RezV3Writer {
    tx: Option<SyncSender<Msg>>,
    thread: Option<JoinHandle<Result<(), String>>>,
    path: PathBuf,
    recording_id: i64,
}

impl RezV3Writer {
    /// Create the `.rez` at `path`, insert its recording row, and spawn the
    /// writer thread.
    ///
    /// The file is a valid, openable `.rez` from the moment this returns:
    /// there is no `.partial`, no rename at the end, and nothing to move
    /// aside at the start (`RezDb::create` refuses an existing file
    /// atomically). That property is what retires the whole staging dance —
    /// an early-killed recording is just a recording whose `complete` is 0.
    pub(crate) fn create(path: &Path, seed: ManifestSeed) -> Result<Self, String> {
        let db = RezDb::create(path)?;
        let recording_id = db.insert_recording(&seed)?;

        // Bound 1, as in v2: the hand-off blocks while the writer is busy,
        // which is the intended backpressure signal.
        let (tx, rx) = sync_channel(1);
        // A spawn failure leaves the file in place, unlike v2's `.partial`
        // unlink: it is a valid empty recording at the caller's chosen output
        // path, not a staging artifact a later run would have to interpret.
        let thread = std::thread::Builder::new()
            .name("rez-v3-writer".to_string())
            .spawn(move || writer_thread(rx, db, recording_id))
            .map_err(|e| format!("failed to spawn the .rez writer thread: {e}"))?;

        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            path: path.to_path_buf(),
            recording_id,
        })
    }

    /// The recording being written — valid and readable while it is written.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The `recordings` row this writer appends to.
    pub(crate) fn recording_id(&self) -> i64 {
        self.recording_id
    }

    /// Hand one tick's WAL rows to the writer.
    pub(crate) fn wal(&mut self, rows: Vec<WalRow>) -> Result<(), String> {
        if rows.is_empty() {
            return self.check_alive();
        }
        self.send(Msg::Wal(rows))
    }

    /// Hand one seal batch (= one transaction) to the writer. Blocks while the
    /// channel is full: that is the intended backpressure signal.
    pub(crate) fn seal(&mut self, batch: Vec<SealJob>) -> Result<(), String> {
        if batch.is_empty() {
            return self.check_alive();
        }
        self.send(Msg::Seal(batch))
    }

    /// Record the final clock offset and mark the recording complete.
    pub(crate) fn finalize(mut self, clock_offset: (u64, i64)) -> Result<(), String> {
        self.send(Msg::Finalize { clock_offset })?;
        self.join()
    }

    /// Report a writer that has already failed, on a hand-off that sends
    /// nothing. Without it, writer health would only be polled when there is
    /// something to write, and a recording whose writer died would go on
    /// reporting success for every empty tick in between.
    fn check_alive(&mut self) -> Result<(), String> {
        if self
            .thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            return Err(self.join().err().unwrap_or_else(|| {
                "the .rez writer thread exited before the recording finished".to_string()
            }));
        }
        Ok(())
    }

    fn send(&mut self, msg: Msg) -> Result<(), String> {
        let Some(tx) = self.tx.as_ref() else {
            return Err("the .rez writer thread has already been joined".to_string());
        };
        if tx.send(msg).is_ok() {
            return Ok(());
        }
        // The receiver is gone, so the writer has exited (it exits its receive
        // loop on the first error). Send-failure → join → report the stored
        // error, rather than logging per-tick against a broken recording.
        Err(self.join().err().unwrap_or_else(|| {
            "the .rez writer thread exited before the recording finished".to_string()
        }))
    }

    /// Close the channel and join the writer, returning its stored result.
    /// Idempotent: a second call is a no-op `Ok`.
    fn join(&mut self) -> Result<(), String> {
        self.tx = None;
        match self.thread.take() {
            // The panic arm is unreachable by contract (the global hook exits
            // the process before unwinding); it exists so this path cannot
            // itself panic.
            Some(handle) => handle
                .join()
                .unwrap_or_else(|_| Err("the .rez writer thread panicked".to_string())),
            None => Ok(()),
        }
    }
}

impl Drop for RezV3Writer {
    /// The writer must be joined on every path out — including the ones that
    /// skip an explicit `finalize` — so a dropped handle never leaves a
    /// detached thread still writing to the database.
    fn drop(&mut self) {
        if let Err(e) = self.join() {
            warn!("the .rez writer failed: {e}");
        }
    }
}

/// An encoded segment waiting to be inserted.
struct Encoded {
    sampler: String,
    seq: u64,
    meta: SegmentMeta,
    bytes: Vec<u8>,
}

/// The writer thread body. Every fallible operation returns `Err`; the loop
/// exits on the first error so the failure surfaces on the next hand-off
/// instead of accumulating against a broken recording.
fn writer_thread(rx: Receiver<Msg>, mut db: RezDb, recording_id: i64) -> Result<(), String> {
    // Next segment sequence number per sampler.
    let mut next_seq: BTreeMap<String, u64> = BTreeMap::new();
    // Timestamps the `clock_offsets` series already carries. Only finalize
    // reads it, but it has to be maintained as batches seal — see below.
    let mut observed: BTreeSet<u64> = BTreeSet::new();

    loop {
        match rx.recv() {
            Ok(Msg::Wal(rows)) => db.insert_wal_rows(recording_id, &rows)?,
            Ok(Msg::Seal(batch)) => {
                if let Some(ts) = seal_batch(&mut db, recording_id, &mut next_seq, batch)? {
                    observed.insert(ts);
                }
            }
            Ok(Msg::Finalize { clock_offset }) => {
                // The loop's final tick observation joins the series only when
                // it adds a timestamp no sealed row already covers — otherwise
                // the series would carry two conflicting offsets at one
                // timestamp and consumers could not read it uniformly. The
                // row-derived value wins because it is a projection of the
                // `:wall_offset` column the segment itself carries. Same rule,
                // and same reason, as the v2 writer.
                let novel = !observed.contains(&clock_offset.0);
                return db.transaction(|tx| {
                    if novel {
                        tx.insert_clock_offset(recording_id, clock_offset.0, clock_offset.1)?;
                    }
                    tx.mark_complete(recording_id)
                });
            }
            // The handle was dropped without finalizing. Nothing to clean up:
            // the file is already a valid `.rez` holding every committed tick,
            // with `complete` still 0 — that is the recovery artifact.
            Err(_) => return Ok(()),
        }
    }
}

/// Encode one batch's segments, insert them — with the batch's clock
/// observation — in ONE transaction, then prune the sealed samplers' WAL
/// outside it. Returns the timestamp of the observation recorded, if any.
fn seal_batch(
    db: &mut RezDb,
    recording_id: i64,
    next_seq: &mut BTreeMap<String, u64>,
    batch: Vec<SealJob>,
) -> Result<Option<u64>, String> {
    // Encoding happens BEFORE the transaction opens: it is CPU work
    // proportional to segment size (the fleet's worst single segment is
    // 6.23 MiB) and would hold the write lock for its whole duration.
    //
    // The cost, and it is a real divergence from v2, which encoded and
    // appended one segment at a time: the WHOLE batch is resident before
    // anything is inserted — ~75 MiB for a 12-table co-seal of worst-case
    // segments. That is bounded (by the batch, which the seal policy bounds)
    // and it is the price of "insert all of them in one transaction"; the
    // alternative is holding the write lock across every encode.
    let mut encoded = Vec::with_capacity(batch.len());
    // The batch's clock observation: the NEWEST sealed row's
    // `(timestamp, wall_offset)`, paired with that same table's offset — never
    // one table's timestamp against another's. Derived from the rows just
    // sealed, so every entry in the series is a projection of the
    // `:wall_offset` column it summarizes, exactly as in v2.
    let mut observation: Option<(u64, i64)> = None;
    for job in batch {
        let (Some(&first_ts), Some(&last_ts)) =
            (job.table.timestamps.first(), job.table.timestamps.last())
        else {
            // A zero-row table has no time span to catalog and no WAL rows to
            // prune. The ingest side never seals one; the writer declines to
            // invent a span for it rather than failing the recording.
            continue;
        };
        let bytes = write_table_parquet(&job.table)
            .map_err(|e| format!("failed to encode a {} segment: {e}", job.sampler))?;
        // `>=`, so a later job wins a tie — same rule as v2's `seal_segments`.
        if observation.is_none_or(|(seen, _)| last_ts >= seen) {
            observation = Some((last_ts, job.table.wall_offsets.last().copied().unwrap_or(0)));
        }
        // Bumped before the commit, which is safe only because the writer
        // exits on its first error: no later batch ever reuses this map.
        let seq = next_seq.entry(job.sampler.clone()).or_insert(0);
        encoded.push(Encoded {
            sampler: job.sampler,
            seq: *seq,
            meta: SegmentMeta {
                rows: job.table.timestamps.len() as u64,
                first_ts,
                last_ts,
            },
            bytes,
        });
        *seq += 1;
    }

    // ONE transaction for the whole batch. The fleet seals 12 tables in
    // lockstep, and 12 implicit commits would be 12 fsyncs at
    // `synchronous=FULL` against a ~46 ms tick.
    //
    // The batch's clock observation rides along inside it, for free: no extra
    // commit, no extra fsync, and it lands iff the segments it was derived
    // from do. It is a `clock_offsets` ROW rather than something a reader has
    // to dig out of a segment, which is what keeps drift readable from the
    // catalog alone — including on a recording that is killed before it ever
    // finalizes, where these are the only observations there will be.
    db.transaction(|tx| {
        for e in &encoded {
            tx.insert_segment(recording_id, &e.sampler, e.seq, &e.meta, &e.bytes)?;
        }
        if let Some((ts, offset)) = observation {
            tx.insert_clock_offset(recording_id, ts, offset)?;
        }
        Ok(())
    })?;

    // OUTSIDE the transaction, deliberately: pruning inside it measured p90
    // 78 ms / max 245 ms (a quiet sampler accumulates ~6,500 rows before
    // sealing), and `live_wal`'s watermark filter makes a crash between the
    // commit above and the delete below harmless — a straddling row is simply
    // not live. The prune is a pure background optimisation, worth p90
    // 212.7 → 44.4 ms on seal ticks. `RezTx` does not expose `prune_wal`, so
    // this ordering is enforced by the type, not by this comment.
    //
    // Each sampler is pruned only up to its OWN segment's `last_ts`: rows a
    // sampler ingested after the sealed span, and every other sampler's rows,
    // stay live.
    for e in &encoded {
        db.prune_wal(recording_id, &e.sampler, e.meta.last_ts)?;
    }
    Ok(observation.map(|(ts, _)| ts))
}

/// One metric's contribution to a WAL row: exactly what
/// `TableBuilder::push_row` needs to place the value in its column, and nothing
/// else. The recorder's own `Snapshot` entry carries a good deal more — that is
/// the difference between 1,925 B and 10,908 B per sampler per tick, measured
/// on a real fleet snapshot (see the journal § "WAL rows are values-only").
///
/// Encoded with `rmp_serde::to_vec`, which writes structs as ARRAYS and enums
/// as `[index, payload]`, so these field names cost nothing on the wire and are
/// chosen for readability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct WalCell {
    /// The snapshot entry's name — the segment's column key (`"5"`, `"5x3"`).
    /// Numeric-id strings of a few bytes, so carrying one per cell per tick is
    /// noise next to the value; dropping them and relying on positional order
    /// would not be, because cgroup metrics appear and vanish mid-recording and
    /// a positional decode would silently reattribute every later column.
    pub name: String,
    /// The snapshot **entry's** metadata, verbatim — NOT the parquet column's.
    ///
    /// The difference matters to a reader. `metric_type` is **not** in here:
    /// `TableBuilder::push_row` injects it (`rez.rs`, the `or_insert_with` that
    /// builds a `RezColumn`) and `metriken-exposition` never carries it. A
    /// recovery path that built `RezColumn { metadata: cell.metadata, .. }`
    /// directly would produce a column a natively sealed segment does not
    /// match, and `read_table_parquet` would then read every gauge back as a
    /// counter. Derive `metric_type` from the [`WalValue`] tag — or, simplest
    /// and what makes the two paths identical by construction, rebuild owned
    /// `Counter`/`Gauge`/`Histogram` entries and replay them through
    /// `TableBuilder::push_row`, which injects it exactly as the writer did.
    /// (A histogram's `grouping_power`/`max_value_power` DO appear here, put
    /// there by the agent's exposition; [`WalValue::Histogram`] carries them
    /// too, so a cell decodes without consulting metadata at all.)
    ///
    /// Carried ONLY on the first WAL row in which this metric appears **in the
    /// current segment** — `maybe_seal` clears the tracking for a sampler when
    /// it seals, so each segment's WAL span re-anchors its own metadata.
    ///
    /// Repeating it every tick is precisely the full-msgpack cost the
    /// measurement rejected; re-anchoring costs one payload per metric per
    /// segment, ~1 tick in `max_rows` (~0.02% at the 4096-row default). What
    /// that buys is an invariant contained entirely in the live WAL: **the
    /// first live WAL row mentioning a metric carries its metadata.** No
    /// segment lookup, so no decoding an arbitrarily old segment footer to
    /// learn a tail's labels — the cost the WAL exists to avoid — and nothing
    /// breaks when hindsight retention deletes old segments
    /// (`DELETE FROM segments WHERE last_ts < cutoff`).
    ///
    /// It also makes the WAL's metadata semantics *identical* to a segment
    /// column's, which an anchor held for the recording's lifetime did not:
    /// `seal_completed` installs a fresh `TableBuilder` at every rotation, so a
    /// column re-latches its labels each segment. A metric whose labels drift
    /// mid-recording (a unit correction, an agent restart remapping an id) is
    /// therefore captured in the WAL exactly where it is captured in segments.
    /// And the tracking set no longer grows without bound as cgroup metric
    /// names churn.
    pub metadata: Option<BTreeMap<String, String>>,
    pub value: WalValue,
    /// The acquisition window, as `(begin_ns, end_ns)`.
    pub window: Option<(u64, u64)>,
}

/// A cell's value, tagged by shape — which is also what tells a reader which
/// `RezValues` column the cell belongs in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) enum WalValue {
    Counter(u64),
    Gauge(i64),
    /// `(grouping_power, max_value_power, buckets)`. The H2 config travels with
    /// the buckets so `histogram::Histogram::from_buckets` needs nothing else —
    /// two bytes against a 7,424-bucket payload, and it keeps the cell decodable
    /// without consulting the metadata row.
    Histogram(u8, u8, Vec<u64>),
}

impl WalValue {
    fn of(entry: &Entry<'_>) -> Self {
        match entry {
            Entry::Counter(c) => WalValue::Counter(c.value),
            Entry::Gauge(g) => WalValue::Gauge(g.value),
            Entry::Histogram(h) => WalValue::Histogram(
                h.value.config().grouping_power(),
                h.value.config().max_value_power(),
                h.value.as_slice().to_vec(),
            ),
        }
    }
}

/// Encode one sampler's cells for one tick into a `wal.row` BLOB.
pub(crate) fn encode_wal_row(cells: &[WalCell]) -> Result<Vec<u8>, String> {
    rmp_serde::to_vec(cells).map_err(|e| format!("failed to encode a WAL row: {e}"))
}

/// The inverse of [`encode_wal_row`] — the recovery entry point.
pub(crate) fn decode_wal_row(bytes: &[u8]) -> Result<Vec<WalCell>, String> {
    rmp_serde::from_slice(bytes).map_err(|e| format!("failed to decode a WAL row: {e}"))
}

/// The scrape-side half of the v3 writer: per-sampler open segments, the seal
/// decision, and — new in v3 — a WAL row per sampler per tick.
///
/// Everything except that last part is v2's `StreamRecorder` verbatim, by
/// *importing* its pieces rather than copying them: `SealPolicy`,
/// `BuilderState` (with `open_first`'s FNV-1a stagger and `seal_completed`'s
/// rotation), `TableBuilder`, `group_by_sampler` and `dedup_key`. A second copy
/// of the dedup rule is exactly how the two containers would drift.
///
/// The per-tick WAL write is the entire point of the container swap. In v2,
/// kill-safety was per-segment: at `kill -9` 120 s into a fleet recording, 16
/// of 26 tables recovered nothing at all because they had not sealed yet. Here
/// every tick is committed as it arrives, for quiet tables as much as busy
/// ones, so an unclean kill costs one tick rather than a whole open segment.
pub(crate) struct StreamRecorderV3 {
    /// Open segment per sampler: builder, open instant, and current targets.
    builders: BTreeMap<String, BuilderState>,
    /// Window-advance dedup keys. Held here, not on the builder, so dedup
    /// survives a builder rotation: the key of a row in an already-sealed
    /// segment must still suppress a re-observation.
    last_keys: BTreeMap<String, u64>,
    /// Per sampler, the metrics whose metadata is already in the CURRENT
    /// segment's WAL span. Cleared for a sampler when it seals, so each segment
    /// re-anchors its own metadata — see `WalCell::metadata`.
    described: BTreeMap<String, HashSet<String>>,
    handle: RezV3Writer,
    policy: SealPolicy,
}

impl StreamRecorderV3 {
    pub(crate) fn new(handle: RezV3Writer) -> Self {
        Self::with_policy(handle, SealPolicy::default())
    }

    pub(crate) fn with_policy(handle: RezV3Writer, policy: SealPolicy) -> Self {
        Self {
            builders: BTreeMap::new(),
            last_keys: BTreeMap::new(),
            described: BTreeMap::new(),
            handle,
            policy,
        }
    }

    /// The recording being written — a valid, readable `.rez` throughout.
    pub(crate) fn path(&self) -> &Path {
        self.handle.path()
    }

    /// Append one scraped snapshot: partition by sampler and, for each sampler
    /// whose representative acquisition window advanced, push a row stamped
    /// `anchored_ts` with this tick's `wall_offset_ns` observation — **and**
    /// hand the same row to the WAL.
    ///
    /// The WAL row and the builder row are produced from the same entries
    /// behind the same dedup gate. A tick the segment path skipped must not
    /// reappear as a recovered row, or replaying the WAL would put back a
    /// duplicate observation the dedup rule exists to remove.
    ///
    /// **Fallible, unlike v2's `ingest`**, because unlike v2's it writes. The
    /// alternative — stash the error and report it from the next `maybe_seal` —
    /// can swallow it outright: `RezV3Writer::send` joins the thread on a send
    /// failure, so the subsequent `Drop` finds nothing to join, logs nothing,
    /// and a caller that never calls `maybe_seal` or `finalize` again loses the
    /// failure entirely. The caller already handles `maybe_seal`'s error; this
    /// is the same shape at the same cadence.
    ///
    /// Note that a writer failure is still *asynchronous* — the hand-off can
    /// return `Ok` for the tick that ultimately kills the writer, and the error
    /// surfaces on a later hand-off. That is inherent to the writer thread and
    /// is why `maybe_seal` polls health even with nothing to seal.
    pub(crate) fn ingest(
        &mut self,
        snapshot: &Snapshot,
        anchored_ts: u64,
        wall_offset_ns: i64,
    ) -> Result<(), String> {
        // One `Vec` for the whole tick: `RezV3Writer::wal` commits it as a
        // single transaction, so a tick is atomic across samplers.
        //
        // `wal`'s primary key is `(recording_id, sampler, ts)`, so re-using an
        // `anchored_ts` for one sampler is a UNIQUE violation that kills the
        // recording — stricter than the segment path, which would merely write
        // two rows at one timestamp. Unreachable from the recorder loop, whose
        // stamps are a monotonic clock in nanoseconds, and left strict on
        // purpose: a repeated stamp means the caller's clock is broken, and
        // `ON CONFLICT DO NOTHING` would silently drop the tick instead.
        //
        // Two passes, and the split is deliberate: everything fallible happens
        // in the first, which touches no builder. `encode_wal_row` returning
        // `Err` mid-tick would otherwise leave the samplers already visited
        // holding a builder row whose WAL row was never committed — the one
        // state this design must not produce.
        let mut wal_rows = Vec::new();
        let mut accepted = Vec::new();
        for (sampler, entries) in group_by_sampler(snapshot) {
            let key = dedup_key(&entries, anchored_ts);
            if let Some(&last) = self.last_keys.get(sampler) {
                if key <= last {
                    continue; // window unchanged → same observation → skip
                }
            }

            let described = self.described.entry(sampler.to_string()).or_default();
            let cells: Vec<WalCell> = entries
                .iter()
                .map(|e| {
                    let name = e.name().to_string();
                    // `insert` returns true the first time only — and
                    // `maybe_seal` empties this set when the sampler seals, so
                    // "the first time" means "the first time in this segment".
                    let metadata = described.insert(name.clone()).then(|| {
                        e.metadata()
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    });
                    WalCell {
                        name,
                        metadata,
                        value: WalValue::of(e),
                        window: e.window().map(|w| (w.begin_ns, w.end_ns)),
                    }
                })
                .collect();
            wal_rows.push(WalRow {
                sampler: sampler.to_string(),
                ts: anchored_ts,
                wall_offset: wall_offset_ns,
                row: encode_wal_row(&cells)?,
            });
            accepted.push((sampler, key, entries));
        }

        // Pass 2: infallible. The builders and the dedup keys advance together.
        for (sampler, key, entries) in accepted {
            self.last_keys.insert(sampler.to_string(), key);
            let policy = &self.policy;
            let state = self
                .builders
                .entry(sampler.to_string())
                .or_insert_with(|| BuilderState::open_first(sampler, policy));
            state.push_row(anchored_ts, wall_offset_ns, &entries);
        }
        self.handle.wal(wal_rows)
    }

    /// Seal every open segment past any threshold, as ONE batch → one
    /// transaction. Empty builders never seal.
    ///
    /// Call this every loop iteration, scrape or not: an unreachable endpoint
    /// must still get its pre-outage rows sealed, and it is also where a writer
    /// that died asynchronously gets noticed.
    pub(crate) fn maybe_seal(&mut self) -> Result<(), String> {
        let mut batch = Vec::new();
        for (sampler, builder) in drain_due(&mut self.builders, &self.policy) {
            // The one thing v3 does that v2 does not. The metadata anchor is
            // per SEGMENT, so it rotates with the builder: the next WAL row for
            // this sampler re-carries every metric's metadata. That is what
            // keeps the live WAL self-contained once the prune below the new
            // segment's `last_ts` lands, and what makes the WAL capture label
            // drift exactly where a re-latched `TableBuilder` captures it.
            self.described.remove(&sampler);
            batch.push(SealJob {
                sampler,
                table: builder.finish(),
            });
        }
        self.handle.seal(batch)
    }

    /// Seal the remaining partial segments (small by construction) and mark the
    /// recording complete.
    ///
    /// The tails are sealed even though the WAL already holds them and the
    /// reader can materialize that tail: a cleanly finished recording should be
    /// segments and nothing else, so the WAL is left empty and no consumer pays
    /// for a replay it does not need.
    pub(crate) fn finalize(mut self, clock_offset: (u64, i64)) -> Result<(), String> {
        let tails: Vec<SealJob> = std::mem::take(&mut self.builders)
            .into_iter()
            .filter_map(|(sampler, state)| {
                state.into_tail().map(|builder| SealJob {
                    sampler,
                    table: builder.finish(),
                })
            })
            .collect();
        self.handle.seal(tails)?;
        self.handle.finalize(clock_offset)
    }

    /// Rows in a sampler's open (unsealed) segment.
    #[cfg(test)]
    fn open_rows(&self, sampler: &str) -> usize {
        self.builders
            .get(sampler)
            .map(BuilderState::open_rows)
            .unwrap_or(0)
    }

    /// The row and age targets the sampler's *current* open segment seals at.
    #[cfg(test)]
    fn open_targets(&self, sampler: &str) -> Option<(usize, std::time::Duration)> {
        self.builders.get(sampler).map(BuilderState::targets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::rez::recorder_tests_support::counter;
    use crate::recorder::rez::{detect_rez_format, Entry, RezFormat, RezTable, TableBuilder};
    use metriken::Window;

    const ANCHOR: u64 = 1_700_000_000_000_000_000;

    fn seed() -> ManifestSeed {
        ManifestSeed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: [("sampling_interval_ms".to_string(), "100".to_string())]
                .into_iter()
                .collect(),
            clock_anchor_wall_ns: ANCHOR,
        }
    }

    /// A sealable job with `ts.len()` rows for `sampler`, every row carrying
    /// `wall_offset` in the `:wall_offset` sidecar.
    fn job_with_offset(sampler: &str, ts: &[u64], wall_offset: i64) -> SealJob {
        let mut b = TableBuilder::new(sampler.to_string());
        for (i, &t) in ts.iter().enumerate() {
            let c = counter("0", sampler, i as u64, Some(Window::new(t - 1, t)));
            b.push_row(t, wall_offset, &[Entry::Counter(&c)]);
        }
        SealJob {
            sampler: sampler.to_string(),
            table: b.finish(),
        }
    }

    fn job(sampler: &str, ts: &[u64]) -> SealJob {
        job_with_offset(sampler, ts, 7)
    }

    /// A job the writer cannot encode: `wall_offsets` shorter than
    /// `timestamps` fails `RecordBatch`'s equal-length check inside
    /// `write_table_parquet`. Same shape of mid-recording writer failure as a
    /// full disk: it happens on the writer thread, after the hand-off returned.
    fn unencodable_job(sampler: &str) -> SealJob {
        SealJob {
            sampler: sampler.to_string(),
            table: RezTable {
                sampler: sampler.to_string(),
                timestamps: vec![1_000, 2_000],
                wall_offsets: vec![0],
                columns: Vec::new(),
            },
        }
    }

    fn wal_row(sampler: &str, ts: u64) -> WalRow {
        WalRow {
            sampler: sampler.to_string(),
            ts,
            wall_offset: 7,
            row: format!("row@{ts}").into_bytes(),
        }
    }

    #[test]
    fn create_leaves_a_valid_openable_file_immediately() {
        // THE property that retires `.partial`, rename-aside and the rename at
        // finalize: the output path holds a valid `.rez` before a single row
        // is written, so an early-killed recording needs no staging file to be
        // recoverable and no consumer has to know a second path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let writer = RezV3Writer::create(&path, seed()).unwrap();

        assert!(path.exists(), "the output path itself, not a .partial");
        assert!(
            !dir.path().join("out.rez.partial").exists(),
            "v3 has no staging file"
        );
        assert_eq!(detect_rez_format(&path).unwrap(), RezFormat::V3Sqlite);

        // And it is not merely a well-formed empty database: the recording is
        // already there to be read, by a second connection, while the writer
        // still holds the file.
        let db = RezDb::open(&path).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(recordings.len(), 1);
        assert_eq!(recordings[0].id, writer.recording_id());
        assert_eq!(recordings[0].meta.clock_anchor_wall_ns, ANCHOR);
        assert_eq!(
            recordings[0].meta.labels.get("source").map(String::as_str),
            Some("rezolus")
        );
        assert!(
            !recordings[0].complete,
            "an in-progress recording is never complete"
        );
    }

    #[test]
    fn a_seal_batch_is_one_transaction() {
        // Without batching, a fleet co-seal of 12 tables is 12 implicit
        // commits — 12 fsyncs at synchronous=FULL — against a ~46 ms tick.
        // "One transaction" is only observable by failing ONE insert in the
        // batch and checking that the segment inserted before it, which would
        // have committed fine on its own, is gone too.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut writer = RezV3Writer::create(&path, seed()).unwrap();
        let rid = writer.recording_id();

        // Plant the row the batch's SECOND segment collides with on the
        // primary key `(recording_id, sampler, seq)`, from another connection.
        {
            let db = RezDb::open(&path).unwrap();
            db.insert_segment(
                rid,
                "blockio",
                0,
                &SegmentMeta {
                    rows: 1,
                    first_ts: 1,
                    last_ts: 1,
                },
                b"planted",
            )
            .unwrap();
        }

        writer
            .seal(vec![
                job("cpu_usage", &[1_000, 2_000]),
                job("blockio", &[10]),
            ])
            .unwrap();
        let err = writer
            .finalize((2_000, 7))
            .expect_err("the colliding insert must fail the recording");
        assert!(
            err.contains("failed to insert segment blockio#0"),
            "the writer's own error, not a generic one: {err}"
        );

        let db = RezDb::open(&path).unwrap();
        assert!(
            db.read_segments(rid, "cpu_usage").unwrap().is_empty(),
            "cpu_usage was inserted BEFORE the failing statement; one \
             transaction means it is rolled back with it"
        );
        let blockio = db.read_segments(rid, "blockio").unwrap();
        assert_eq!(blockio.len(), 1, "only the planted row survives");
        assert_eq!(blockio[0].bytes, b"planted");
    }

    #[test]
    fn seal_prunes_only_the_sealed_samplers_wal_and_only_up_to_last_ts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut writer = RezV3Writer::create(&path, seed()).unwrap();
        let rid = writer.recording_id();

        for ts in [10, 20, 30, 40] {
            writer
                .wal(vec![wal_row("cpu_usage", ts), wal_row("blockio", ts)])
                .unwrap();
        }
        // cpu_usage seals its first three ticks; blockio seals nothing.
        writer.seal(vec![job("cpu_usage", &[10, 20, 30])]).unwrap();
        writer.finalize((40, 7)).unwrap();

        let db = RezDb::open(&path).unwrap();
        let cpu_usage = db.read_wal(rid, "cpu_usage").unwrap();
        assert_eq!(
            cpu_usage.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![40],
            "pruned up to the segment's last_ts, and no further"
        );
        let blockio = db.read_wal(rid, "blockio").unwrap();
        assert_eq!(
            blockio.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![10, 20, 30, 40],
            "an unsealed sampler's WAL is untouched by another's prune"
        );
    }

    #[test]
    fn kill_before_finalize_leaves_every_ticks_rows_recoverable() {
        // THE headline guarantee, and the reason for the whole container
        // change. A quiet sampler still inside its first seal period recovered
        // NOTHING under v2 — 16 of 26 fleet tables were in exactly this state
        // at `kill -9`, because kill-safety was per-segment. Here every tick is
        // committed as it arrives, so a recording that never seals and never
        // finalizes still holds every row.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut writer = RezV3Writer::create(&path, seed()).unwrap();
        let rid = writer.recording_id();

        for i in 1..=5u64 {
            writer.wal(vec![wal_row("drivehealth", i * 10)]).unwrap();
        }
        // No finalize, no seal — the writer just goes away.
        drop(writer);

        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.all_samplers(rid).unwrap(),
            vec!["drivehealth"],
            "a sampler that never sealed is still discoverable"
        );
        let live = db.live_wal(rid, "drivehealth").unwrap();
        assert_eq!(
            live.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![10, 20, 30, 40, 50],
            "every ingested tick is recoverable"
        );
        assert_eq!(live[0].row, b"row@10");
        assert!(
            !db.read_recordings().unwrap()[0].complete,
            "a recording killed before finalize is not complete"
        );
    }

    #[test]
    fn kill_before_finalize_still_has_clock_observations() {
        // Clock drift must survive the kill path, which is the path v3 exists
        // for. If only `finalize` contributed an observation, a killed
        // recording would have NONE — and reading drift back out of a sealed
        // segment means decoding its `:wall_offset` column from the parquet
        // BLOB, since `segments` catalogs only rows/first_ts/last_ts. v2 could
        // render drift straight from the manifest with no decode; the
        // `clock_offsets` rows are what keep that true here.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut writer = RezV3Writer::create(&path, seed()).unwrap();
        let rid = writer.recording_id();

        writer
            .seal(vec![
                // The newest row is in the FIRST job, and the older sampler
                // carries a wildly different offset — so a derivation that
                // took the last job's, or the batch's oldest row, or mixed one
                // table's timestamp with another's offset, is visible here.
                job_with_offset("cpu_usage", &[3_000], 7),
                job_with_offset("scheduler", &[1_000, 2_000], 99),
            ])
            .unwrap();
        // Killed: no finalize.
        drop(writer);

        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.read_clock_offsets(rid).unwrap(),
            vec![(3_000, 7)],
            "the batch's newest sealed row, paired with its OWN table's offset"
        );
        assert!(!db.read_recordings().unwrap()[0].complete);
    }

    #[test]
    fn finalize_drops_a_tick_observation_a_sealed_row_already_covers() {
        // Two conflicting offsets at one timestamp would make the series
        // unreadable, so the row-derived value wins: it is a projection of the
        // `:wall_offset` column the segment itself carries. Same rule as v2.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut writer = RezV3Writer::create(&path, seed()).unwrap();
        let rid = writer.recording_id();

        writer
            .seal(vec![job_with_offset("cpu_usage", &[1_000], 7)])
            .unwrap();
        writer.finalize((1_000, -11)).unwrap();

        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.read_clock_offsets(rid).unwrap(),
            vec![(1_000, 7)],
            "one observation per timestamp, and the sealed row's wins"
        );
    }

    #[test]
    fn writer_error_surfaces_on_the_next_handoff() {
        // A writer-thread failure must surface as the writer's OWN error on a
        // hand-off — that is what decides whether a broken recording gets
        // noticed instead of producing per-tick log spam.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut writer = RezV3Writer::create(&path, seed()).unwrap();

        // The hand-off itself succeeds: the failure happens on the writer.
        writer.seal(vec![unencodable_job("cpu_usage")]).unwrap();

        // The next send that finds the receiver gone joins and reports. A
        // bounded retry, because the channel buffers one message and the
        // writer fails asynchronously.
        let mut surfaced = None;
        for _ in 0..500 {
            match writer.seal(vec![job("scheduler", &[1_000])]) {
                Ok(()) => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(e) => {
                    surfaced = Some(e);
                    break;
                }
            }
        }
        let err = surfaced.expect("the writer's error must surface on a hand-off");
        assert!(
            err.contains("failed to encode a cpu_usage segment"),
            "the writer's stored error, not a generic send failure: {err}"
        );

        // Exit-on-first-error: nothing the writer accepted after the failure
        // reached the file, and the recording is not complete.
        let db = RezDb::open(&path).unwrap();
        let recording = &db.read_recordings().unwrap()[0];
        assert!(db.samplers(recording.id).unwrap().is_empty());
        assert!(!recording.complete);
    }

    #[test]
    fn writer_error_surfaces_on_an_empty_handoff() {
        // Seals are size/age driven, so most ticks hand over nothing. Without
        // a health check on that path, a dead writer would go unnoticed for as
        // long as nothing happens to be due.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut writer = RezV3Writer::create(&path, seed()).unwrap();
        writer.seal(vec![unencodable_job("cpu_usage")]).unwrap();

        let mut surfaced = None;
        for _ in 0..500 {
            match writer.seal(Vec::new()) {
                Ok(()) => std::thread::sleep(std::time::Duration::from_millis(1)),
                Err(e) => {
                    surfaced = Some(e);
                    break;
                }
            }
        }
        let err = surfaced.expect("a dead writer must surface on an empty batch too");
        assert!(
            err.contains("failed to encode a cpu_usage segment"),
            "the writer's stored error, not a generic one: {err}"
        );
    }

    #[test]
    fn finalize_marks_the_recording_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut writer = RezV3Writer::create(&path, seed()).unwrap();
        let rid = writer.recording_id();

        writer.wal(vec![wal_row("cpu_usage", 1_000)]).unwrap();
        writer.seal(vec![job("cpu_usage", &[1_000])]).unwrap();
        writer.finalize((2_000, -11)).unwrap();

        // Still the same file at the same path — nothing was renamed into
        // place, because nothing was ever staged.
        assert_eq!(detect_rez_format(&path).unwrap(), RezFormat::V3Sqlite);
        let db = RezDb::open(&path).unwrap();
        let recordings = db.read_recordings().unwrap();
        assert_eq!(recordings.len(), 1);
        assert!(
            recordings[0].complete,
            "finalize marks the recording complete"
        );
        assert_eq!(
            db.read_clock_offsets(rid).unwrap(),
            vec![(1_000, 7), (2_000, -11)],
            "the sealed batch's observation, then the loop's last one — which \
             covers a span no sealed row does, so it joins the series"
        );
        assert_eq!(db.total_rows(rid, "cpu_usage").unwrap(), 1);
    }

    #[test]
    fn segments_get_consecutive_seq_numbers_per_sampler() {
        // `read_segments` splices in `seq` order, so a writer that restarted
        // numbering — or shared one counter across samplers — would produce a
        // table the reader cannot reassemble.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut writer = RezV3Writer::create(&path, seed()).unwrap();
        let rid = writer.recording_id();

        writer.seal(vec![job("cpu_usage", &[10, 20])]).unwrap();
        writer
            .seal(vec![job("cpu_usage", &[30]), job("blockio", &[30])])
            .unwrap();
        writer.seal(vec![job("cpu_usage", &[40])]).unwrap();
        writer.finalize((40, 7)).unwrap();

        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.read_segments(rid, "cpu_usage")
                .unwrap()
                .iter()
                .map(|s| (s.seq, s.meta.rows, s.meta.first_ts, s.meta.last_ts))
                .collect::<Vec<_>>(),
            vec![(0, 2, 10, 20), (1, 1, 30, 30), (2, 1, 40, 40)],
        );
        assert_eq!(
            db.read_segments(rid, "blockio")
                .unwrap()
                .iter()
                .map(|s| s.seq)
                .collect::<Vec<_>>(),
            vec![0],
            "each sampler numbers its own segments from 0"
        );
    }

    // ---------------------------------------------------------------------
    // `StreamRecorderV3` — the ingest side.
    // ---------------------------------------------------------------------

    use crate::recorder::rez::recorder_tests_support::snap;
    use metriken_exposition::{Counter, Gauge, Histogram as ExpHistogram, Snapshot, SnapshotV2};
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    fn policy(max_rows: usize) -> SealPolicy {
        SealPolicy {
            max_bytes: usize::MAX,
            max_rows,
            max_age: Duration::from_secs(3600),
        }
    }

    /// A policy no builder can ever meet: only `finalize` closes a segment.
    fn never_seals() -> SealPolicy {
        policy(usize::MAX)
    }

    fn recorder(path: &Path, policy: SealPolicy) -> (StreamRecorderV3, i64) {
        let writer = RezV3Writer::create(path, seed()).unwrap();
        let rid = writer.recording_id();
        (StreamRecorderV3::with_policy(writer, policy), rid)
    }

    /// One row per sampler per tick, every sampler's window advancing each
    /// tick so nothing dedups and every table grows at the same rate.
    fn multi_snap(samplers: &[&str], i: u64) -> (Snapshot, u64) {
        let ts = 10_000 + i * 1_000;
        let end = 9_500 + i * 1_000;
        let counters = samplers
            .iter()
            .map(|s| counter(&format!("{s}_ops"), s, i, Some(Window::new(end - 500, end))))
            .collect();
        (snap(ts, counters), ts)
    }

    /// Drive `rec` one tick at a time, returning the row count at which each
    /// sampler sealed its **first** segment. Mirrors the v2 helper.
    fn first_seal_rows(rec: &mut StreamRecorderV3, samplers: &[&str], ticks: u64) -> Vec<usize> {
        let mut out = vec![0usize; samplers.len()];
        for i in 0..ticks {
            let (s, ts) = multi_snap(samplers, i);
            rec.ingest(&s, ts, 0).unwrap();
            let before: Vec<usize> = samplers.iter().map(|n| rec.open_rows(n)).collect();
            rec.maybe_seal().unwrap();
            for (k, name) in samplers.iter().enumerate() {
                if out[k] == 0 && before[k] > 0 && rec.open_rows(name) == 0 {
                    out[k] = before[k];
                }
            }
        }
        out
    }

    fn cells(row: &WalRow) -> Vec<WalCell> {
        decode_wal_row(&row.row).unwrap()
    }

    #[test]
    fn every_tick_writes_wal_rows_even_when_nothing_seals() {
        // The kill-loss guarantee rests on the WAL being written per TICK,
        // independent of the seal schedule — that is the whole reason for the
        // container swap. Under a policy no builder can meet, not one segment
        // exists, and every tick must still be on disk.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (mut rec, rid) = recorder(&path, never_seals());

        let mut want = Vec::new();
        for i in 0..5u64 {
            let (s, ts) = multi_snap(&["cpu_usage"], i);
            rec.ingest(&s, ts, i as i64).unwrap();
            rec.maybe_seal().unwrap();
            want.push(ts);
        }
        assert_eq!(rec.open_rows("cpu_usage"), 5, "nothing sealed");
        // Drop, not finalize: finalize would seal the tails and hide the point.
        drop(rec);

        let db = RezDb::open(&path).unwrap();
        assert!(
            db.samplers(rid).unwrap().is_empty(),
            "no segment was ever sealed"
        );
        let wal = db.read_wal(rid, "cpu_usage").unwrap();
        assert_eq!(
            wal.iter().map(|r| r.ts).collect::<Vec<_>>(),
            want,
            "every tick's row is in the WAL, in order"
        );
        assert_eq!(
            wal.iter().map(|r| r.wall_offset).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4],
            "each row carries its own tick's wall observation"
        );
        // The rows are the tick's values, not placeholders.
        for (i, row) in wal.iter().enumerate() {
            let c = cells(row);
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].name, "cpu_usage_ops");
            assert_eq!(c[0].value, WalValue::Counter(i as u64));
        }
    }

    #[test]
    fn sealing_prunes_only_the_sealed_samplers_wal() {
        // This is why WAL rows are per-sampler rather than whole snapshots:
        // one slow table (drivehealth seals every 300 s at fleet cadence) must
        // not pin every other sampler's tail, and a busy table's prune must
        // not take the slow one's rows with it. `drivehealth`'s single row sits
        // BELOW `cpu_usage`'s sealed watermark, so a prune that forgot which
        // sampler it was pruning would delete it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        // Bucket 29 of 64 against `max_rows = 4` reduces the target by
        // `(4 / 128) * 29 = 0`, so cpu_usage seals at exactly 4 rows.
        let (mut rec, rid) = recorder(&path, policy(4));

        let quiet = Window::new(0, 500); // never advances → deduped after tick 0
        for i in 0..7u64 {
            let ts = 10_000 + i * 1_000;
            let end = 9_500 + i * 1_000;
            let s = snap(
                ts,
                vec![
                    counter(
                        "cpu_usage_ops",
                        "cpu_usage",
                        i,
                        Some(Window::new(end - 500, end)),
                    ),
                    counter("drivehealth_ops", "drivehealth", i, Some(quiet)),
                ],
            );
            rec.ingest(&s, ts, 0).unwrap();
            rec.maybe_seal().unwrap();
        }
        drop(rec);

        let db = RezDb::open(&path).unwrap();
        let segments = db.read_segments(rid, "cpu_usage").unwrap();
        assert_eq!(segments.len(), 1, "cpu_usage sealed exactly one segment");
        assert_eq!(segments[0].meta.last_ts, 13_000);
        assert!(
            db.read_segments(rid, "drivehealth").unwrap().is_empty(),
            "drivehealth never sealed"
        );

        assert_eq!(
            db.read_wal(rid, "cpu_usage")
                .unwrap()
                .iter()
                .map(|r| r.ts)
                .collect::<Vec<_>>(),
            vec![14_000, 15_000, 16_000],
            "the sealed sampler is pruned up to its own segment's last_ts"
        );
        assert_eq!(
            db.read_wal(rid, "drivehealth")
                .unwrap()
                .iter()
                .map(|r| r.ts)
                .collect::<Vec<_>>(),
            vec![10_000],
            "the unsealed sampler's row survives another sampler's prune, \
             even though its ts is below that sampler's watermark"
        );
    }

    #[test]
    fn dedup_and_stagger_carry_over_from_v2() {
        // Both landed in 8dd4f442 on main and must not regress here: `last_keys`
        // lives outside the builder so it survives a rotation, and the first
        // seal is staggered per sampler so equal-rate tables never co-seal.
        let dir = tempfile::tempdir().unwrap();

        // (a) dedup survives a builder rotation — and suppresses the WAL row
        //     too, or recovery would resurrect a row the segment path dropped.
        let path = dir.path().join("dedup.rez");
        let (mut rec, rid) = recorder(&path, policy(2));
        for i in 0..4u64 {
            let (s, ts) = multi_snap(&["cpu_usage"], i);
            rec.ingest(&s, ts, 0).unwrap();
            rec.maybe_seal().unwrap();
        }
        assert_eq!(rec.open_rows("cpu_usage"), 0, "two full segments sealed");

        // Tick 3's window lives in an already-sealed segment. Re-observing it
        // must still dedup, which only holds if `last_key` survived rotation.
        let (dup, dup_ts) = multi_snap(&["cpu_usage"], 3);
        rec.ingest(&dup, dup_ts + 1, 0).unwrap();
        assert_eq!(rec.open_rows("cpu_usage"), 0, "dedup survived the seal");
        drop(rec);

        let db = RezDb::open(&path).unwrap();
        assert_eq!(db.total_rows(rid, "cpu_usage").unwrap(), 4);
        // 4 WAL rows written, all four pruned by the two seals, and the
        // deduped re-observation never became a fifth.
        assert!(
            db.read_wal(rid, "cpu_usage").unwrap().is_empty(),
            "a deduped tick writes no WAL row"
        );

        // (b) the first seal is staggered across samplers.
        const MAX_ROWS: usize = 256;
        let path = dir.path().join("stagger.rez");
        let (mut rec, _) = recorder(&path, policy(MAX_ROWS));
        let samplers = ["cpu_usage", "scheduler"];
        let sealed = first_seal_rows(&mut rec, &samplers, MAX_ROWS as u64);
        assert_ne!(
            sealed[0], sealed[1],
            "same ingest rate must still give different first-seal row counts"
        );
        for (k, name) in samplers.iter().enumerate() {
            assert!(
                sealed[k] >= MAX_ROWS / 2 && sealed[k] <= MAX_ROWS,
                "{name} first-sealed at {} rows, outside [{}, {MAX_ROWS}]",
                sealed[k],
                MAX_ROWS / 2
            );
        }
        assert_eq!(
            rec.open_targets("cpu_usage"),
            Some((MAX_ROWS, Duration::from_secs(3600))),
            "every segment after the first uses the full policy"
        );
    }

    #[test]
    fn a_quiet_sampler_that_never_seals_is_fully_recoverable() {
        // THE headline guarantee, end to end through the ingest path. At
        // `kill -9` 120 s into a v2 fleet recording, 16 of 26 tables recovered
        // NOTHING because kill-safety was per-segment and they had not sealed
        // one yet. Here a table that never seals and never finalizes still
        // holds every row it ever ingested.
        //
        // "Fully" is the load-bearing word, so this checks every property a
        // recovered table needs rather than just the row count: all three
        // metric shapes, each tick's own `wall_offset` (the clock observation
        // the `:wall_offset` column is built from), the acquisition windows the
        // uncertainty bands are computed from, and the labels — without which
        // the rows would recover as values with no identity.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (mut rec, rid) = recorder(&path, never_seals());

        const TICKS: u64 = 200;
        for i in 0..TICKS {
            let ts = 10_000 + i * 1_000;
            // A drifting, signed, non-zero offset: a fixed 0 would pass against
            // a writer that dropped the observation entirely.
            rec.ingest(
                &shapes_snap("drivehealth", ts, "nanoseconds", i),
                ts,
                i as i64 - 100,
            )
            .unwrap();
            rec.maybe_seal().unwrap();
        }
        // Killed: no finalize, no seal, nothing flushed on the way out.
        drop(rec);

        let db = RezDb::open(&path).unwrap();
        assert_eq!(
            db.all_samplers(rid).unwrap(),
            vec!["drivehealth"],
            "a sampler with no segment at all is still discoverable"
        );
        assert!(
            db.read_segments(rid, "drivehealth").unwrap().is_empty(),
            "nothing sealed, so the WAL is the ONLY record"
        );
        let live = db.live_wal(rid, "drivehealth").unwrap();
        assert_eq!(live.len() as u64, TICKS, "every ingested tick is live");
        for (i, row) in live.iter().enumerate() {
            let i = i as u64;
            let ts = 10_000 + i * 1_000;
            assert_eq!(row.ts, ts);
            assert_eq!(
                row.wall_offset,
                i as i64 - 100,
                "each tick's own clock observation, not a shared one"
            );
            let c = cells(row);
            assert_eq!(
                c.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
                vec!["0", "1", "2"],
                "all three shapes recovered"
            );
            assert_eq!(c[0].value, WalValue::Counter(i));
            assert_eq!(c[1].value, WalValue::Gauge(-(i as i64)));
            let WalValue::Histogram(gp, mvp, ref buckets) = c[2].value else {
                panic!("the third metric is a histogram: {:?}", c[2].value);
            };
            assert_eq!((gp, mvp), (3, 8), "the H2 config rebuilds the histogram");
            assert_eq!(buckets.iter().sum::<u64>(), 1);
            assert_eq!(c[0].window, Some((ts - 500, ts)), "windows recovered");
        }
        // The labels are in the live WAL itself, so the recovered columns carry
        // the same identity a sealed segment's would.
        for c in cells(&live[0]) {
            let m = c.metadata.as_ref().expect("the first live row describes");
            assert_eq!(m.get("sampler").map(String::as_str), Some("drivehealth"));
            assert_eq!(m.get("unit").map(String::as_str), Some("nanoseconds"));
        }
        assert!(!db.read_recordings().unwrap()[0].complete);
    }

    fn shape_meta(metric: &str, sampler: &str, unit: &str) -> HashMap<String, String> {
        [
            ("metric".to_string(), metric.to_string()),
            ("sampler".to_string(), sampler.to_string()),
            ("unit".to_string(), unit.to_string()),
        ]
        .into_iter()
        .collect()
    }

    /// One tick of `sampler` carrying all three metric shapes, with `unit` in
    /// every metric's metadata so a label change is observable.
    fn shapes_snap(sampler: &str, ts: u64, unit: &str, i: u64) -> Snapshot {
        let w = Some(Window::new(ts - 500, ts));
        let mut h = histogram::Histogram::new(3, 8).unwrap();
        h.increment(37).unwrap();
        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH + Duration::from_nanos(ts),
            duration: Duration::ZERO,
            metadata: HashMap::new(),
            counters: vec![
                Counter::new("0".to_string(), i, shape_meta("0", sampler, unit)).with_window(w),
            ],
            gauges: vec![
                Gauge::new("1".to_string(), -(i as i64), shape_meta("1", sampler, unit))
                    .with_window(w),
            ],
            histograms: vec![{
                // The agent's exposition puts the H2 config in a histogram's
                // metadata (`src/agent/exposition/http/snapshot.rs`), and
                // `read_table_parquet` needs it to rebuild the buckets — so a
                // fixture without it is not a snapshot this writer ever sees.
                let mut m = shape_meta("2", sampler, unit);
                m.insert("grouping_power".to_string(), "3".to_string());
                m.insert("max_value_power".to_string(), "8".to_string());
                ExpHistogram::new("2".to_string(), h, m).with_window(w)
            }],
        })
    }

    /// One tick of `cpu_usage` carrying all three metric shapes.
    fn mixed_snap(ts: u64) -> Snapshot {
        shapes_snap("cpu_usage", ts, "nanoseconds", 42)
    }

    /// The `unit` label on a sealed segment's `"0"` column, decoded from the
    /// stored parquet BLOB.
    fn segment_unit(bytes: &[u8]) -> String {
        let table =
            crate::recorder::rez::read_table_parquet("cpu_usage".to_string(), bytes.to_vec())
                .unwrap();
        table
            .columns
            .iter()
            .find(|c| c.name == "0")
            .unwrap()
            .metadata["unit"]
            .clone()
    }

    #[test]
    fn each_segments_wal_span_re_anchors_its_own_metadata() {
        // The metadata anchor is per SEGMENT, not per recording, and that is
        // load-bearing three ways.
        //
        // 1. Label drift. `seal_completed` installs a fresh `TableBuilder`, so
        //    a segment column re-latches its labels at every rotation. An
        //    anchor held for the recording's lifetime would leave the WAL
        //    describing a recovered tail with the FIRST segment's stale labels
        //    while a sealed tail carried the new ones — the two paths
        //    disagreeing about the same rows.
        // 2. Self-containment. The first LIVE WAL row must carry the metadata,
        //    so recovery never decodes an old segment's footer to learn the
        //    tail's labels — the exact cost the WAL exists to avoid.
        // 3. Retention. Hindsight deletes segments by `last_ts < cutoff`
        //    (`rez_sqlite.rs`'s schema note); an anchor living in a deleted
        //    segment would put the metadata nowhere at all.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        // Bucket 29 against `max_rows = 2` reduces by `(2 / 128) * 29 = 0`.
        let (mut rec, rid) = recorder(&path, policy(2));

        // Ticks 0-1 seal segment 0; the unit corrects at tick 2; ticks 2-3 seal
        // segment 1; tick 4 is the live tail.
        for i in 0..5u64 {
            let ts = 10_000 + i * 1_000;
            let unit = if i < 2 { "nanoseconds" } else { "microseconds" };
            rec.ingest(&shapes_snap("cpu_usage", ts, unit, i), ts, 0)
                .unwrap();
            rec.maybe_seal().unwrap();
        }
        drop(rec);

        let db = RezDb::open(&path).unwrap();
        let segments = db.read_segments(rid, "cpu_usage").unwrap();
        assert_eq!(segments.len(), 2, "two rotations at max_rows = 2");
        assert_eq!(
            segments
                .iter()
                .map(|s| segment_unit(&s.bytes))
                .collect::<Vec<_>>(),
            vec!["nanoseconds", "microseconds"],
            "each segment re-latches its own labels — and they survive the \
             prune, being in the segment BLOB rather than the WAL"
        );

        let live = db.live_wal(rid, "cpu_usage").unwrap();
        assert_eq!(
            live.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![14_000],
            "only the tail is past the second segment's watermark"
        );
        for c in cells(&live[0]) {
            let m = c
                .metadata
                .as_ref()
                .unwrap_or_else(|| panic!("{} must re-carry metadata after a seal", c.name));
            assert_eq!(
                m.get("unit").map(String::as_str),
                Some("microseconds"),
                "the tail's labels are the CURRENT ones, and they are in the \
                 live WAL itself — no segment lookup, nothing lost to retention"
            );
        }
    }

    #[test]
    fn wal_rows_carry_each_metrics_metadata_once_then_values_only() {
        // The WAL row is the recovery record for a table that may never seal,
        // so it has to be self-describing — but repeating every metric's label
        // map on every tick is exactly the 10,908 B/sampler/tick that the
        // values-only measurement rejected. Metadata therefore rides the FIRST
        // row a metric appears in and never again; by the time that row can be
        // pruned, a segment covering it carries the same metadata.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (mut rec, rid) = recorder(&path, never_seals());

        rec.ingest(&mixed_snap(1_000), 1_000, 3).unwrap();
        rec.ingest(&mixed_snap(2_000), 2_000, 4).unwrap();
        drop(rec);

        let db = RezDb::open(&path).unwrap();
        let wal = db.read_wal(rid, "cpu_usage").unwrap();
        assert_eq!(wal.len(), 2);

        let first = cells(&wal[0]);
        assert_eq!(
            first.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["0", "1", "2"],
            "counters, then gauges, then histograms — `group_by_sampler` order"
        );
        assert_eq!(first[0].value, WalValue::Counter(42));
        assert_eq!(first[1].value, WalValue::Gauge(-42));
        let WalValue::Histogram(gp, mvp, ref buckets) = first[2].value else {
            panic!("the third metric is a histogram: {:?}", first[2].value);
        };
        assert_eq!((gp, mvp), (3, 8), "the H2 config travels with the buckets");
        assert_eq!(buckets.iter().sum::<u64>(), 1);
        assert_eq!(first[0].window, Some((500, 1_000)));

        // Every metric's labels are on its first row, and nothing else needs to
        // be consulted to rebuild the column.
        for c in &first {
            let m = c
                .metadata
                .as_ref()
                .expect("first sighting carries metadata");
            assert_eq!(m.get("sampler").map(String::as_str), Some("cpu_usage"));
            assert_eq!(m.get("unit").map(String::as_str), Some("nanoseconds"));
        }

        let second = cells(&wal[1]);
        assert_eq!(second.len(), 3);
        for c in &second {
            assert!(
                c.metadata.is_none(),
                "{} repeated its metadata on the second tick",
                c.name
            );
        }
        assert_eq!(second[0].value, WalValue::Counter(42));
        assert_eq!(second[0].window, Some((1_500, 2_000)));
    }

    #[test]
    fn finalize_seals_the_tails_and_marks_the_recording_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let (mut rec, rid) = recorder(&path, never_seals());
        assert_eq!(rec.path(), path);

        for i in 0..3u64 {
            let (s, ts) = multi_snap(&["cpu_usage"], i);
            rec.ingest(&s, ts, 0).unwrap();
            rec.maybe_seal().unwrap();
        }
        rec.finalize((12_000, 5)).unwrap();

        let db = RezDb::open(&path).unwrap();
        assert!(db.read_recordings().unwrap()[0].complete);
        assert_eq!(
            db.total_rows(rid, "cpu_usage").unwrap(),
            3,
            "the open tail is sealed rather than left for WAL replay"
        );
        assert!(
            db.read_wal(rid, "cpu_usage").unwrap().is_empty(),
            "sealing the tail prunes its WAL"
        );
    }
}
