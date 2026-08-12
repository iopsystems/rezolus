//! The `.rez` v3 writer thread. See docs/journal/2026-08-12-rez-sqlite-container.md.
//!
//! Same shape as the v2 writer (`rez_stream.rs`): a dedicated thread behind a
//! bounded channel, so encoding a large segment cannot skew the scrape cadence
//! and a disk that cannot keep up backpressures the loop instead of growing
//! memory. Everything the tar container needed to fake transactions is gone —
//! no `.partial`, no rename, no rename-aside, no checkpoint manifests, no
//! two-sync ordering protocol. **A seal batch is one transaction**, and the
//! file at `path` is a valid, openable `.rez` from the moment `create` returns.
//!
//! Contract: PANIC-FREE — every fallible op returns `Err`. The global panic
//! hook (`src/main.rs`) prints and calls `process::exit(101)` BEFORE
//! unwinding, so a panic here never reaches the send-error path, skips
//! finalize, and in wrapped mode orphans the child.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread::JoinHandle;

use tracing::warn;

use super::rez::{write_table_parquet, RezTable};
use super::rez_sqlite::{RecordingMeta, RezDb, SegmentMeta, WalRow};

/// Everything known when the recording starts. v3 has no manifest and no
/// per-recording tar directory — a recording IS a row in `recordings` — so the
/// v2 writer's `ManifestSeed` is exactly `RecordingMeta` here, minus `dir`.
/// The name is kept so the two writers read alike at their call sites.
pub(crate) type ManifestSeed = RecordingMeta;

/// One sealed segment handed to the writer thread. The table already carries
/// its `wall_offsets`; everything here is owned data (`Send + 'static`).
pub(crate) struct SealJob {
    pub sampler: String,
    pub table: RezTable,
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::rez::recorder_tests_support::counter;
    use crate::recorder::rez::{detect_rez_format, Entry, RezFormat, TableBuilder};
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
}
