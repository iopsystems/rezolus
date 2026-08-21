//! The hindsight rolling buffer: **a `.rez` v3 recording with retention**, and
//! nothing else. See docs/journal/2026-08-12-rez-sqlite-container.md
//! § "Hindsight".
//!
//! What this replaces is a fixed-size ring of 4 KB-aligned slots
//! (`snapshot_len` × `snapshot_count`) written round-robin and overwritten in
//! place, with a separate msgpack→parquet path to get data out of it. Two
//! things were wrong with that, and the second is the one that mattered:
//!
//! 1. **Nothing could read it.** The ring was a private on-disk format with one
//!    consumer — its own dump routine. A v3 buffer is an ordinary `.rez`: the
//!    viewer, the MCP tools and `parquet metadata` open it with no special
//!    casing, live, while it is being written.
//! 2. **A dump could tear**, because it copied a buffer that was being written
//!    in place. Here the buffer is immutable sealed segments plus a WAL, so a
//!    dump is `VACUUM INTO` — a point-in-time copy taken inside a read
//!    transaction, consistent by construction. Measured: 0 torn reads, 0
//!    `SQLITE_BUSY` over 92 s, +0.7 ms writer impact.
//!
//! Retention is [`RezDb::evict_before`], made an indexed lookup by the
//! `segments_by_time` index. The file stays bounded because freed pages are
//! reused (measured 1.004–1.011× live), and stays bounded *after a spike*
//! because `auto_vacuum=INCREMENTAL` lets the writer trickle pages back.

use std::path::{Path, PathBuf};
use std::time::Duration;

use metriken_exposition::Snapshot;

use super::state::TimeRange;
use crate::recorder::rez_sqlite::RezDb;
use crate::recorder::rez_v3_writer::{ManifestSeed, RezV3Writer, StreamRecorderV3};
use crate::recorder::seal_policy::SealPolicy;

/// The rolling buffer. One `.rez` recording, fed a row per tick, trimmed to
/// the configured lookback every tick.
pub struct HindsightBuffer {
    rec: StreamRecorderV3,
    /// How far back the buffer reaches — `[general] duration`.
    lookback: Duration,
    /// The newest row stamp ingested so far. Retention is measured from THIS,
    /// not from `now`, and the difference matters at exactly the moment
    /// hindsight is for: if the agent dies, a `now`-relative cutoff would keep
    /// deleting until the buffer was empty, throwing away the minutes leading
    /// up to the very incident being investigated. Anchored to the data, an
    /// outage freezes the buffer instead.
    newest_ts: Option<u64>,
    /// The FIRST row stamp ever ingested — kept only so
    /// [`at_retention_bound`](Self::at_retention_bound) can be answered
    /// exactly. It is not affected by eviction, which is the point.
    first_ts: Option<u64>,
}

impl HindsightBuffer {
    /// Create the buffer at `path`, which must not exist.
    ///
    /// The seal policy is a parameter rather than a constant because segment
    /// size has to track the scrape interval: `[general] segment_rows` sets it
    /// and defaults to the writer's 900, which is a segment per ~15 minutes at
    /// the default 1 s interval and per ~90 seconds at 10 Hz.
    pub fn create(
        path: &Path,
        seed: ManifestSeed,
        lookback: Duration,
        policy: SealPolicy,
    ) -> Result<Self, String> {
        let writer = RezV3Writer::create(path, seed)?;
        Ok(Self {
            rec: StreamRecorderV3::with_policy(writer, policy),
            lookback,
            newest_ts: None,
            first_ts: None,
        })
    }

    /// Append one scraped snapshot. Every tick is committed as it arrives, so
    /// an unclean kill of the daemon costs one tick, not a whole open segment.
    pub fn ingest(
        &mut self,
        snapshot: &Snapshot,
        anchored_ts: u64,
        wall_offset_ns: i64,
    ) -> Result<(), String> {
        self.rec.ingest(snapshot, anchored_ts, wall_offset_ns)?;
        self.newest_ts = Some(self.newest_ts.map_or(anchored_ts, |t| t.max(anchored_ts)));
        self.first_ts.get_or_insert(anchored_ts);
        Ok(())
    }

    /// Whether retention has begun: the buffer is now dropping as much as it
    /// takes in rather than still filling.
    ///
    /// Derived from the span the recording has COVERED — newest minus the very
    /// first row, which eviction never moves — not from the span it currently
    /// retains. The retained span is the wrong instrument: it converges to just
    /// *under* the lookback (the oldest surviving row sits one tick inside the
    /// cutoff, and a `.rez` row is stamped in nanoseconds), so `retained >=
    /// lookback` reads false forever at steady state. Ask the question the
    /// buffer can answer exactly instead of the one that needs a fudge factor.
    pub fn at_retention_bound(&self) -> bool {
        match (self.first_ts, self.newest_ts) {
            (Some(first), Some(newest)) => {
                Duration::from_nanos(newest.saturating_sub(first)) >= self.lookback
            }
            _ => false,
        }
    }

    /// Seal whatever is due, then apply retention. Call it every tick, scrape
    /// or not — that is also where a writer that died asynchronously surfaces.
    ///
    /// Order is load-bearing: `maybe_seal` first, so a segment closed on this
    /// tick is in the catalog before the cutoff is applied to it. Retention
    /// only ever sees committed data — rows still sitting in an open builder
    /// are evicted when their segment seals and later ages out.
    pub fn maintain(&mut self) -> Result<(), String> {
        self.rec.maybe_seal()?;
        if let Some(cutoff) = self.cutoff() {
            self.rec.evict_before(cutoff)?;
        }
        Ok(())
    }

    /// Block until the writer has committed everything handed off so far.
    ///
    /// The recording loop must NOT call this — the writer being asynchronous
    /// is what keeps sealing and eviction off the tick path. It is for a caller
    /// that is about to inspect the file through a second connection and needs
    /// to see its own last tick, rather than the state from before it.
    #[cfg(test)]
    pub fn sync(&mut self) -> Result<(), String> {
        self.rec.sync()
    }

    /// The retention cutoff: everything wholly older than this goes. `None`
    /// until the first row lands, since there is nothing to measure back from.
    fn cutoff(&self) -> Option<u64> {
        let lookback = self.lookback.as_nanos().min(u64::MAX as u128) as u64;
        self.newest_ts.map(|ts| ts.saturating_sub(lookback))
    }
}

/// What a buffer or a dump holds, from catalog columns alone — no segment or
/// WAL payload is read, so this costs the same on a 4 MB buffer and a 4 GB one.
///
/// This is what `/status` reports, and it is deliberately stated in v3's own
/// terms. The ring's geometry (slot size, slot count, write index, "has it
/// wrapped") described a fixed-size array that no longer exists; reporting a
/// fabricated equivalent would be worse than not reporting it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// Every table in the buffer, including one that has not sealed a segment
    /// yet — a quiet sampler that lives entirely in the WAL is still a table.
    pub tables: Vec<TableSummary>,
    /// Rows a reader will see, across every table: sealed rows plus live WAL.
    pub rows: u64,
    /// The time span actually retained. Not the same as the configured
    /// lookback: retention drops whole segments, so the buffer reaches at
    /// least the lookback and typically a little further.
    pub first_ts: Option<u64>,
    pub last_ts: Option<u64>,
    /// Bytes on disk, sidecars included — what an operator watching the
    /// filesystem actually sees.
    pub bytes: u64,
    /// Pages parked on the free list, and the file's total. Their ratio is the
    /// signal for whether the trickle-reclaim is keeping up: it sits near zero
    /// in steady state (freed pages are reused in place) and rises only when
    /// the working set shrank, which is the case that would otherwise leave
    /// the file stuck at its high-water mark.
    pub free_pages: u32,
    pub pages: u32,
}

/// One table's contribution, from the catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableSummary {
    pub sampler: String,
    pub rows: u64,
    pub segments: u64,
    /// Rows committed per tick but not yet sealed into a segment. Recoverable
    /// after a kill, and for a quiet table this may be *all* of its rows.
    pub live_wal_rows: u64,
}

impl Summary {
    /// The retained span, when there is one.
    pub fn retained(&self) -> Option<Duration> {
        match (self.first_ts, self.last_ts) {
            (Some(a), Some(b)) => Some(Duration::from_nanos(b.saturating_sub(a))),
            _ => None,
        }
    }
}

/// Summarize a `.rez` at `path` without disturbing whoever is writing it. In
/// WAL mode this reader never blocks the writer and is never blocked by it.
pub fn summarize(path: &Path) -> Result<Summary, String> {
    let db = RezDb::open(path)?;
    let mut out = summarize_db(&db)?;
    out.bytes = bytes_on_disk(path);
    Ok(out)
}

fn summarize_db(db: &RezDb) -> Result<Summary, String> {
    let mut out = Summary {
        free_pages: db.pragma_u32("freelist_count")?,
        pages: db.pragma_u32("page_count")?,
        ..Summary::default()
    };
    for rec in db.read_recordings()? {
        let (first, last) = db.recording_time_span(rec.id)?;
        out.first_ts = match (out.first_ts, first) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        out.last_ts = match (out.last_ts, last) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        for sampler in db.all_samplers(rec.id)? {
            let (segments, span) = db.segment_span(rec.id, &sampler)?;
            let live_wal_rows = db.live_wal_span(rec.id, &sampler)?.rows;
            out.rows += span.rows + live_wal_rows;
            out.tables.push(TableSummary {
                sampler,
                rows: span.rows + live_wal_rows,
                segments,
                live_wal_rows,
            });
        }
    }
    Ok(out)
}

/// The database plus its `-wal`/`-shm` sidecars. The sidecar is real disk
/// usage and is capped (4 MiB) rather than unbounded, but reporting only the
/// main file would under-count a buffer that is actively being written.
fn bytes_on_disk(path: &Path) -> u64 {
    let mut total = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        total += std::fs::metadata(PathBuf::from(sidecar))
            .map(|m| m.len())
            .unwrap_or(0);
    }
    total
}

/// Write the buffer out to `dest` as a standalone `.rez`, replacing whatever
/// was there.
///
/// This is the whole of what `perform_dump_to_file` used to do by walking ring
/// slots and running a msgpack→parquet conversion over them. `VACUUM INTO`
/// takes a point-in-time copy inside a read transaction: the writer keeps
/// committing throughout, the copy cannot tear, and the copy is compacted on
/// the way out (a rolling buffer's free list does not travel with it).
///
/// `range` trims the copy — never the buffer — at SEGMENT granularity, so a
/// dump reaches at least as far as asked and possibly further. The returned
/// span says exactly how far, which is why the caller reports it back rather
/// than echoing the request.
///
/// The copy is staged beside `dest` and renamed into place, so a failed dump
/// leaves the previous one intact and no consumer ever sees a half-written
/// file at `dest`.
pub fn dump(buffer: &Path, dest: &Path, range: &TimeRange) -> Result<Summary, String> {
    let parent = dest.parent().filter(|p| !p.as_os_str().is_empty());
    let staging = match parent {
        Some(dir) => tempfile::tempdir_in(dir),
        None => tempfile::tempdir(),
    }
    .map_err(|e| format!("failed to stage the dump beside {}: {e}", dest.display()))?;
    let staged = staging.path().join("dump.rez");

    // A second connection, deliberately: the writer thread owns its own and
    // keeps using it. WAL mode is what makes that safe.
    let src = RezDb::open(buffer)?;
    match (range.start_ns(), range.end_ns()) {
        (None, None) => src.vacuum_into(&staged)?,
        (start, end) => copy_range(
            &src,
            &staged,
            start.unwrap_or(0),
            end.unwrap_or(u64::MAX),
            &|| {},
        )?,
    }
    drop(src);

    let mut db = RezDb::open(&staged)?;
    for id in db
        .read_recordings()?
        .iter()
        .map(|r| r.id)
        .collect::<Vec<_>>()
    {
        // The buffer's recording is still running and so is never `complete`;
        // this copy of it is finished by definition. Without this every
        // hindsight dump would open with a "not cleanly finalized, data may be
        // missing" warning that says nothing true about the artifact.
        db.mark_complete(id)?;
    }
    let mut summary = summarize_db(&db)?;
    // Close before the rename: dropping the connection is what checkpoints and
    // removes the staged file's `-wal`, so what gets renamed is the whole
    // database and not just its older half.
    drop(db);

    std::fs::rename(&staged, dest).map_err(|e| {
        format!(
            "failed to move the dump into place at {}: {e}",
            dest.display()
        )
    })?;
    summary.bytes = bytes_on_disk(dest);
    Ok(summary)
}

/// Build a fresh `.rez` at `staged` holding only the segments that overlap
/// `[start, end]`.
///
/// **Segments only — no WAL rows are copied, and that is the point.** A
/// metric's metadata rides on the *first* WAL row that mentions it within a
/// segment's span, re-anchored at every seal. Copying a slice of raw WAL rows
/// would therefore leave a metric whose anchor row fell before `start` with no
/// identity at all: values with no labels. Sealed segments have no such
/// problem — `TableBuilder::push_row` latches a column's metadata when it
/// creates the column, so every segment describes itself.
///
/// So the live tail is *materialized into a segment* first, through the very
/// function the reader uses ([`crate::recorder::rez_v3_writer::materialize_wal_tail`]), and
/// selection then happens purely over segments. Reusing that function rather
/// than assembling columns here is not stylistic either: it injects
/// `metric_type` via `push_row` exactly as the writer does, and a hand-rolled
/// equivalent produces a segment whose gauges read back as counters.
///
/// Whole segments, always: a segment is an immutable parquet BLOB, so the dump
/// carries a little more than was asked for at each edge. The caller reports
/// the span it actually got.
///
/// Every read happens in ONE snapshot, so retention cannot evict a segment
/// between selecting it and copying its bytes.
///
/// `listed` is that claim's test seam and is `&|| {}` in production — a
/// callback rather than a `#[cfg(test)]` hook or a list/copy split, chosen so
/// that the statements this function runs, and the order it runs them in, are
/// **the same ones the tests run**. A `cfg(test)` hook would compile a
/// different function than the one that ships; splitting the phases would
/// change the shipped structure to suit a test. A no-op closure changes
/// neither: there is no branch here that only a test takes, only a call whose
/// body is empty everywhere except in
/// `a_dump_keeps_a_segment_evicted_after_its_snapshot_opened`.
///
/// It fires once the snapshot is PINNED (`read_recordings` is the first read,
/// and `BEGIN DEFERRED` takes its read mark there) and before a single segment
/// BLOB has been copied — i.e. exactly the window in which retention running
/// on the writer's connection would, without the snapshot, delete a segment
/// out from under the copy.
fn copy_range(
    src: &RezDb,
    staged: &Path,
    start: u64,
    end: u64,
    listed: &dyn Fn(),
) -> Result<(), String> {
    use crate::recorder::rez_v3_rewrite::{copy_recordings_into, CopySpec};
    let mut dst = RezDb::create(staged)?;
    src.read_snapshot(|src| {
        // Pins the snapshot before a single segment BLOB is copied:
        // `read_recordings` is the first read, so `BEGIN DEFERRED` takes its
        // read mark here.
        let _pinned = src.read_recordings()?;
        listed();
        dst.transaction(|tx| {
            let spec = CopySpec {
                start,
                end,
                ..CopySpec::everything()
            };
            copy_recordings_into(src, tx, &spec)?;
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::rez::recorder_tests_support::{counter, snap};
    use crate::recorder::rez::{detect_rez_format, read_table_parquet, RezFormat};
    use crate::recorder::rez_sqlite::{Evicted, RecordingMeta, SegmentMeta};
    use metriken::Window;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;

    const ANCHOR: u64 = 1_700_000_000_000_000_000;
    const SECOND: u64 = 1_000_000_000;

    fn seed() -> ManifestSeed {
        ManifestSeed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: [("sampling_interval_ms".to_string(), "1000".to_string())]
                .into_iter()
                .collect(),
            clock_anchor_wall_ns: ANCHOR,
        }
    }

    /// A policy that seals every `rows` rows. The first-seal stagger reduces
    /// the target by `(rows / 128) * bucket`, which is 0 for any `rows < 128` —
    /// so small policies seal at exactly `rows` for every sampler.
    fn seal_every(rows: usize) -> SealPolicy {
        SealPolicy {
            max_bytes: usize::MAX,
            max_rows: rows,
            max_age: Duration::from_secs(3600),
        }
    }

    /// One tick: a row for each named sampler, every window advancing so
    /// nothing dedups.
    fn tick(samplers: &[&str], i: u64) -> (Snapshot, u64) {
        let ts = ANCHOR + i * SECOND;
        let counters = samplers
            .iter()
            .map(|s| {
                counter(
                    &format!("{s}_ops"),
                    s,
                    i,
                    Some(Window::new(ts - SECOND / 2, ts)),
                )
            })
            .collect();
        (snap(ts, counters), ts)
    }

    #[test]
    fn retention_evicts_segments_older_than_the_lookback() {
        // The mechanism that makes a bounded rolling buffer possible at all,
        // and it has two halves that must move together: whole segments past
        // the lookback, AND the WAL rows of a sampler that has not sealed one.
        // `drivehealth` reports only every fifth tick, so it never fills a
        // segment and lives entirely in the WAL — exactly the quiet-table case
        // a segments-only eviction would leave to grow forever.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.rez");
        let mut buf =
            HindsightBuffer::create(&path, seed(), Duration::from_secs(5), seal_every(4)).unwrap();

        for i in 0..12u64 {
            let samplers: &[&str] = if i % 5 == 0 {
                &["cpu_usage", "drivehealth"]
            } else {
                &["cpu_usage"]
            };
            let (s, ts) = tick(samplers, i);
            buf.ingest(&s, ts, 0).unwrap();
            buf.maintain().unwrap();
        }
        // Newest row is ANCHOR+11s and the lookback is 5s, so the cutoff is
        // ANCHOR+6s.
        let cutoff = ANCHOR + 6 * SECOND;
        drop(buf);

        let db = RezDb::open(&path).unwrap();
        let rid = db.read_recordings().unwrap()[0].id;

        // cpu_usage sealed at rows 0-3, 4-7, 8-11. The first is wholly older
        // than the cutoff and goes; the second straddles it and is kept whole.
        let segments = db.read_segments(rid, "cpu_usage").unwrap();
        assert_eq!(
            segments
                .iter()
                .map(|s| (s.meta.first_ts, s.meta.last_ts))
                .collect::<Vec<_>>(),
            vec![
                (ANCHOR + 4 * SECOND, ANCHOR + 7 * SECOND),
                (ANCHOR + 8 * SECOND, ANCHOR + 11 * SECOND),
            ],
            "the segment wholly before the cutoff is gone; the straddling one \
             is kept whole, because trimming inside a sealed segment would \
             mean rewriting an immutable parquet BLOB"
        );
        // The kept segments are untouched, not merely present.
        for s in &segments {
            let table = read_table_parquet("cpu_usage".to_string(), s.bytes.clone()).unwrap();
            assert_eq!(table.timestamps.len(), s.meta.rows as usize);
        }

        // drivehealth ticked at 0, 5 and 10 and never sealed: its whole history
        // is WAL rows, and the two before the cutoff must go with the segments.
        assert!(
            db.read_segments(rid, "drivehealth").unwrap().is_empty(),
            "drivehealth never filled a segment"
        );
        assert_eq!(
            db.read_wal(rid, "drivehealth")
                .unwrap()
                .iter()
                .map(|r| r.ts)
                .collect::<Vec<_>>(),
            vec![ANCHOR + 10 * SECOND],
            "WAL rows older than the cutoff are evicted too; the one inside \
             the lookback is untouched"
        );
        assert!(cutoff > ANCHOR + 5 * SECOND && cutoff < ANCHOR + 10 * SECOND);
    }

    #[test]
    fn retention_is_measured_from_the_newest_row_not_from_now() {
        // The cutoff follows the DATA, and that choice matters at exactly the
        // moment hindsight is for. If the agent dies — which may well be the
        // incident — a `now`-relative cutoff would keep evicting until the
        // buffer was empty, destroying the minutes leading up to it while an
        // engineer was still being paged. Anchored to the newest row, an
        // outage freezes the buffer instead.
        //
        // These rows are stamped in 2023 and the lookback is 5s, so a cutoff
        // taken from the wall clock would evict every one of them.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.rez");
        let mut buf =
            HindsightBuffer::create(&path, seed(), Duration::from_secs(5), seal_every(2)).unwrap();
        for i in 0..4u64 {
            let (s, ts) = tick(&["cpu_usage"], i);
            buf.ingest(&s, ts, 0).unwrap();
            buf.maintain().unwrap();
        }
        // No further ingest: the source has gone away. Retention keeps running.
        for _ in 0..5 {
            buf.maintain().unwrap();
        }
        drop(buf);

        let summary = summarize(&path).unwrap();
        assert_eq!(
            summary.rows, 4,
            "a stalled source freezes the buffer; it does not drain it"
        );
        assert_eq!(summary.first_ts, Some(ANCHOR));
    }

    #[test]
    fn at_retention_bound_flips_once_the_recording_outlasts_the_lookback() {
        // `/status` reports this, and the obvious derivation is wrong: the
        // span the buffer RETAINS converges to just under the lookback — the
        // oldest surviving row sits one tick inside the cutoff — so
        // `retained >= lookback` reads false forever at steady state. This is
        // the span the recording has COVERED, which crosses the lookback
        // exactly once and stays crossed.
        // A 4.5 s lookback against 1 s ticks, so the cutoff lands BETWEEN two
        // rows. That is the ordinary case, not a contrived one: in the real
        // loop the same misalignment comes from tick jitter, since rows are
        // stamped `anchor + monotonic elapsed` in nanoseconds and never land
        // exactly a lookback apart.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.rez");
        let lookback = Duration::from_millis(4_500);
        let mut buf =
            HindsightBuffer::create(&path, seed(), lookback, seal_every(usize::MAX)).unwrap();
        assert!(!buf.at_retention_bound(), "nothing ingested yet");

        for i in 0..5u64 {
            let (s, ts) = tick(&["cpu_usage"], i);
            buf.ingest(&s, ts, 0).unwrap();
            buf.maintain().unwrap();
        }
        // Ticks 0..4 span 4s against a 4.5s lookback: still filling.
        assert!(!buf.at_retention_bound(), "4s covered of a 4.5s lookback");

        let (s, ts) = tick(&["cpu_usage"], 5);
        buf.ingest(&s, ts, 0).unwrap();
        buf.maintain().unwrap();
        assert!(buf.at_retention_bound(), "5s covered: eviction has started");

        // And the retained span is now permanently just UNDER the lookback:
        // these rows are unsealed, so retention drops them one at a time and
        // the oldest survivor sits strictly inside the cutoff.
        //
        // A span-based rule cannot be rescued by flipping the comparison
        // either, because SEALED rows go the other way: a segment is dropped
        // only when its newest row is out of the window, so a segmented table
        // retains *more* than the lookback. The retained span straddles the
        // lookback from both sides depending on what has sealed; the covered
        // span crosses it once.
        // `summarize` opens its own connection, so it sees the file rather than
        // this buffer's queued work. Without the barrier the eviction above may
        // still be in flight and the file still spans all six ticks — which is
        // how this read, and only this read, went flaky.
        buf.sync().unwrap();
        let retained = summarize(&path).unwrap().retained().unwrap();
        assert!(
            retained < lookback,
            "retained {retained:?} — a span-based check would read false here \
             forever, even though eviction is plainly running"
        );
    }

    #[test]
    fn the_file_plateaus_across_many_evict_cycles() {
        // THE property that makes a bounded hindsight buffer possible: freed
        // pages are reused, so a file that evicts as fast as it fills stops
        // growing instead of growing forever. Measured at 1.004-1.011x live
        // (journal § "Eviction without VACUUM"); asserted here as "the page
        // count at cycle 60 is the page count at cycle 240", which is the
        // statement that goes red the moment eviction stops reclaiming.
        //
        // Driven at the container level rather than through `HindsightBuffer`
        // on purpose: this is a property of insert-and-evict, and 240 cycles
        // of parquet encoding would buy nothing but runtime.
        const BLOB: usize = 256 * 1024;
        const RETAIN: u64 = 24;
        const CYCLES: u64 = 240;
        const WARM: u64 = 60;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.rez");
        let mut db = RezDb::create(&path).unwrap();
        let rid = db
            .insert_recording(&RecordingMeta {
                labels: BTreeMap::new(),
                metadata: BTreeMap::new(),
                clock_anchor_wall_ns: ANCHOR,
            })
            .unwrap();

        let bytes = vec![0xa5u8; BLOB];
        let mut warm_pages = 0u32;
        for cycle in 0..CYCLES {
            db.insert_segment(
                rid,
                "cpu_usage",
                cycle,
                &SegmentMeta {
                    rows: 1,
                    first_ts: cycle,
                    last_ts: cycle,
                },
                &bytes,
            )
            .unwrap();
            db.evict_before(rid, cycle.saturating_sub(RETAIN - 1))
                .unwrap();
            if cycle == WARM {
                warm_pages = db.pragma_u32("page_count").unwrap();
            }
        }

        // The claim first, the fixture check after: this is the assertion the
        // test is named for, and it should be the one that speaks when it goes
        // red.
        let end_pages = db.pragma_u32("page_count").unwrap();
        assert!(
            end_pages <= warm_pages,
            "the file must plateau: {warm_pages} pages at cycle {WARM}, \
             {end_pages} after {CYCLES} — {} cycles of turnover wrote \
             {} MiB through a {} MiB working set",
            CYCLES - WARM,
            (CYCLES as usize * BLOB) >> 20,
            (RETAIN as usize * BLOB) >> 20,
        );

        // And the plateau is near the live size, not merely flat at some
        // arbitrary high-water mark.
        //
        // 1.1x, not the journal's 1.004-1.011x, and the gap is scale rather
        // than a discrepancy: that range was measured at 226-839 MB live,
        // where the schema, the pointer maps and a few dozen free pages are
        // noise. At the 6 MiB this test can afford they are not — the measured
        // value here is 1.052x, of which 65 free pages (266 KiB) is the bulk.
        // What the assertion is for is a REGRESSION into unbounded growth, and
        // for that the plateau check above is the sharp instrument.
        let live = RETAIN * BLOB as u64;
        let file = (end_pages as u64) * db.pragma_u32("page_size").unwrap() as u64;
        assert!(
            file < live * 11 / 10,
            "{file} bytes on disk for {live} bytes live is more than 1.1x ({} free pages)",
            db.pragma_u32("freelist_count").unwrap()
        );

        assert_eq!(
            db.read_segments(rid, "cpu_usage").unwrap().len(),
            RETAIN as usize,
            "fixture: the working set is constant across the run"
        );
    }

    #[test]
    fn a_dump_is_readable_and_consistent_while_recording_continues() {
        // The tearing problem that motivated the migration. The old ring
        // copied slots that were being overwritten in place; here the dump is
        // a point-in-time copy taken while the writer keeps committing.
        //
        // Two things are asserted, and the second is what makes this more than
        // "the file parses": the dump must contain everything the buffer held
        // when the dump was asked for. A naive copy of the database file alone
        // would satisfy the first — SQLite's main file is always a *valid*
        // database — while silently missing every commit since the last
        // checkpoint.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.rez");
        let dest = dir.path().join("dump.rez");
        let mut buf =
            HindsightBuffer::create(&path, seed(), Duration::from_secs(3600), seal_every(8))
                .unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let ticks = Arc::new(AtomicU64::new(0));
        let writer = {
            let stop = Arc::clone(&stop);
            let ticks = Arc::clone(&ticks);
            std::thread::spawn(move || {
                let mut i = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let (s, ts) = tick(&["cpu_usage", "scheduler"], i);
                    buf.ingest(&s, ts, 0).unwrap();
                    buf.maintain().unwrap();
                    i += 1;
                    ticks.store(i, Ordering::SeqCst);
                }
                buf
            })
        };

        // Wait until the buffer is genuinely being written, then witness what
        // it holds from a third connection — everything up to this point must
        // appear in a dump that starts afterwards.
        while ticks.load(Ordering::SeqCst) < 20 {
            std::thread::yield_now();
        }
        let witness = summarize(&path).unwrap();
        let dumped = dump(&path, &dest, &TimeRange::default()).unwrap();

        stop.store(true, Ordering::Relaxed);
        drop(writer.join().unwrap());

        assert_eq!(
            detect_rez_format(&dest).unwrap(),
            RezFormat::V3Sqlite,
            "the dump is an ordinary .rez"
        );
        assert!(
            dumped.last_ts >= witness.last_ts && dumped.last_ts.is_some(),
            "the dump must reach at least as far as the buffer did when it was \
             requested: dump {:?} vs witness {:?}",
            dumped.last_ts,
            witness.last_ts
        );
        assert!(
            dumped.rows >= witness.rows,
            "and hold at least the rows the buffer held: {} vs {}",
            dumped.rows,
            witness.rows
        );

        // Self-consistent: every sampler's rows form one strictly increasing
        // timeline across the segment/WAL splice, with nothing duplicated at
        // the seam and no half-written segment.
        let db = RezDb::open(&dest).unwrap();
        let rid = db.read_recordings().unwrap()[0].id;
        assert!(
            db.read_recordings().unwrap()[0].complete,
            "a dump is a finished artifact even though the buffer runs on"
        );
        let samplers = db.all_samplers(rid).unwrap();
        assert_eq!(samplers, vec!["cpu_usage", "scheduler"]);
        for sampler in samplers {
            let mut stamps = Vec::new();
            for segment in db.read_segments(rid, &sampler).unwrap() {
                let table = read_table_parquet(sampler.clone(), segment.bytes).unwrap();
                assert_eq!(
                    table.timestamps.len(),
                    segment.meta.rows as usize,
                    "{sampler}: a segment's parquet must hold the rows the \
                     catalog claims"
                );
                stamps.extend(table.timestamps);
            }
            stamps.extend(db.live_wal(rid, &sampler).unwrap().iter().map(|r| r.ts));
            assert!(
                stamps.windows(2).all(|w| w[0] < w[1]),
                "{sampler}: rows must form one strictly increasing timeline, \
                 with no duplicate at the segment/WAL seam: {stamps:?}"
            );
            assert_eq!(
                stamps.first().copied(),
                Some(ANCHOR),
                "{sampler}: the dump starts where the recording did"
            );
        }
    }

    #[test]
    fn a_hindsight_buffer_opens_as_an_ordinary_rez() {
        // The unification. Nothing could read the slot ring — not the viewer,
        // not the MCP tools, not `parquet metadata`; its only consumer was its
        // own dump routine. A v3 buffer is read by the generic reader, live,
        // mid-recording, with no hindsight-specific casing anywhere.
        //
        // Nothing has sealed here (the policy is far out of reach), so every
        // row the reader returns comes from the live WAL — the state a running
        // buffer is in almost all the time.
        use metriken_query::MetricsSource;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.rez");
        let mut buf = HindsightBuffer::create(
            &path,
            seed(),
            Duration::from_secs(900),
            seal_every(usize::MAX),
        )
        .unwrap();
        for i in 0..6u64 {
            let (s, ts) = tick(&["cpu_usage"], i);
            buf.ingest(&s, ts, 0).unwrap();
            buf.maintain().unwrap();
        }

        assert_eq!(detect_rez_format(&path).unwrap(), RezFormat::V3Sqlite);

        // `parquet metadata` — dispatched by content, not by extension, and
        // handed a file that is still being written.
        let described = crate::parquet_tools::metadata::describe_rez_string_at(&path).unwrap();
        assert!(
            described.contains("cpu_usage"),
            "the metadata tool must name the buffer's tables: {described}"
        );

        // And the viewer/MCP front door, with real values out the other side.
        let reader = crate::rez_reader::RezReader::open_with_pool(
            &path,
            metriken_query::BufferPool::new(8 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(reader.counter_names(), vec!["cpu_usage_ops".to_string()]);
        let (start, end) = reader.time_range().unwrap();
        let metriken_query::QueryResult::Matrix { result } = reader
            .query_range("rate(cpu_usage_ops[5s])", start, end + 1.0, 1.0)
            .expect("the query must resolve")
        else {
            panic!("a range query over a counter is a matrix");
        };
        let points: Vec<f64> = result
            .iter()
            .flat_map(|s| s.values.iter().map(|(_, v)| *v))
            .collect();
        assert!(!points.is_empty(), "the buffer's rows must come back out");
        assert!(
            points.iter().all(|v| (*v - 1.0).abs() < 1e-6),
            "a counter rising 1/s must read back as 1/s: {points:?}"
        );

        // Still true after the buffer is dumped: the dump is the same shape.
        //
        // `sync()` here fixes a real, observed intermittent flake: `dump`
        // opens a SECOND connection (`RezDb::open`), so without this, the
        // read can race the async writer thread and see the file before the
        // last `ingest`/`maintain` tick actually committed — exactly what
        // `RezV3Writer::sync`'s doc warns a second-connection reader must
        // guard against. Reproduced under heavy parallel-test contention on
        // a multi-core Linux container (never locally, on a quiet machine),
        // as `rows >= 6` failing with fewer rows than were ingested.
        buf.sync().unwrap();
        let dest = dir.path().join("dump.rez");
        assert!(dump(&path, &dest, &TimeRange::default()).unwrap().rows >= 6);
        assert_eq!(detect_rez_format(&dest).unwrap(), RezFormat::V3Sqlite);
    }

    #[test]
    fn a_ranged_dump_keeps_metric_identity_for_a_wal_only_table() {
        // The trap a ranged dump has to avoid, and one this project has
        // already been caught by once (B4/d420414e): a metric's metadata rides
        // on the FIRST WAL row that mentions it within a segment's span. Copy
        // a *slice* of raw WAL rows and any metric whose anchor row fell
        // before `start` arrives with no identity — values with no labels.
        //
        // The dump sidesteps it structurally rather than carefully: the live
        // tail is materialized into a SEGMENT first — segments latch their
        // column metadata at creation and are self-describing — and selection
        // then happens purely over segments. Which is also why the tail always
        // travels whole: it is one segment, and segments are never split.
        //
        // Nothing seals here, so this table's entire history is WAL rows and
        // its anchor row is tick 0, well before the requested start.
        use metriken_query::MetricsSource;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.rez");
        let mut buf = HindsightBuffer::create(
            &path,
            seed(),
            Duration::from_secs(3600),
            seal_every(usize::MAX),
        )
        .unwrap();
        for i in 0..10u64 {
            let (s, ts) = tick(&["drivehealth"], i);
            buf.ingest(&s, ts, 0).unwrap();
            buf.maintain().unwrap();
        }
        drop(buf);

        let dest = dir.path().join("dump.rez");
        let range = TimeRange::new(
            Some(std::time::UNIX_EPOCH + Duration::from_nanos(ANCHOR + 5 * SECOND)),
            None,
        );
        dump(&path, &dest, &range).unwrap();

        // The dump is segments and nothing else — no WAL rows were copied.
        let db = RezDb::open(&dest).unwrap();
        let rid = db.read_recordings().unwrap()[0].id;
        assert!(db.read_wal(rid, "drivehealth").unwrap().is_empty());
        let segments = db.read_segments(rid, "drivehealth").unwrap();
        assert_eq!(segments.len(), 1, "the tail was sealed into one segment");

        // And the metric kept its identity, labels and all.
        let table = read_table_parquet("drivehealth".to_string(), segments[0].bytes.clone())
            .expect("the materialized tail must be readable parquet");
        let column = table
            .columns
            .iter()
            .find(|c| c.name == "drivehealth_ops")
            .expect("the metric's column must be present");
        assert_eq!(
            column.metadata.get("metric").map(String::as_str),
            Some("drivehealth_ops"),
            "a value with no labels is the failure this test exists for: {:?}",
            column.metadata
        );
        assert_eq!(
            column.metadata.get("sampler").map(String::as_str),
            Some("drivehealth")
        );
        // `metric_type` is injected by `push_row`, not carried on the WAL cell:
        // a tail assembled by hand instead of replayed would read every gauge
        // back as a counter.
        assert_eq!(
            column.metadata.get("metric_type").map(String::as_str),
            Some("counter")
        );

        let reader = crate::rez_reader::RezReader::open_with_pool(
            &dest,
            metriken_query::BufferPool::new(8 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(reader.counter_names(), vec!["drivehealth_ops".to_string()]);
    }

    #[test]
    fn a_dump_keeps_a_segment_evicted_after_its_snapshot_opened() {
        // **Why hindsight needs no eviction pause.** Retention and a dump run
        // on different connections with nothing between them — no lock, no
        // quiesce, no "hold off evicting while a dump is in flight". The only
        // thing standing between them is the read transaction `copy_range`
        // opens, and the claim is that it is enough: a segment deleted after
        // the snapshot opened is still in the dump, because the dump is
        // reading the database as it stood when it started.
        //
        // Deterministic, not a race. The eviction runs in the `listed` seam,
        // which fires after `read_recordings` has pinned the snapshot and
        // before the first segment BLOB is read — so "the delete landed inside
        // the window" is a fact of the call order rather than something the
        // scheduler has to be persuaded to do. No sleeps, no retries, no
        // "eventually".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.rez");
        let staged = dir.path().join("dump.rez");
        // A lookback far past the data, so the ONLY eviction in this test is
        // the one the seam performs.
        let mut buf =
            HindsightBuffer::create(&path, seed(), Duration::from_secs(3600), seal_every(2))
                .unwrap();
        for i in 0..12u64 {
            let (s, ts) = tick(&["cpu_usage"], i);
            buf.ingest(&s, ts, 0).unwrap();
            buf.maintain().unwrap();
        }
        drop(buf);

        // Everything before ANCHOR+6s: segments [0,1], [2,3] and [4,5].
        let cutoff = ANCHOR + 6 * SECOND;
        let evicted = std::cell::Cell::new(Evicted::default());

        let src = RezDb::open(&path).unwrap();
        copy_range(&src, &staged, 0, u64::MAX, &|| {
            // A SECOND connection, which is what retention actually is here:
            // the writer thread owns its own and evicts from it while a dump
            // reads. Opened inside the seam so it cannot be blamed for pinning
            // anything itself.
            let mut writer = RezDb::open(&path).unwrap();
            let rid = writer.read_recordings().unwrap()[0].id;
            evicted.set(writer.evict_before(rid, cutoff).unwrap());
        })
        .unwrap();
        drop(src);

        // Fixture, and it has to come first: if the eviction did not actually
        // delete anything then the assertion below passes for no reason at all,
        // which is precisely the failure mode this test exists to avoid.
        assert_eq!(
            evicted.get().segments,
            3,
            "fixture: the seam must really have deleted segments mid-dump"
        );
        let after = RezDb::open(&path).unwrap();
        let rid = after.read_recordings().unwrap()[0].id;
        assert_eq!(
            after
                .read_segments(rid, "cpu_usage")
                .unwrap()
                .iter()
                .map(|s| s.meta.first_ts)
                .collect::<Vec<_>>(),
            vec![
                ANCHOR + 6 * SECOND,
                ANCHOR + 8 * SECOND,
                ANCHOR + 10 * SECOND
            ],
            "fixture: the SOURCE really did lose those three segments"
        );

        // The claim. The dump holds all six, including the three the source no
        // longer has, and their bytes are intact rather than merely catalogued
        // — a snapshot that covered the catalog but not the BLOBs would leave a
        // segment row pointing at bytes that were deleted.
        let dumped = RezDb::open(&staged).unwrap();
        let rid = dumped.read_recordings().unwrap()[0].id;
        let segments = dumped.read_segments(rid, "cpu_usage").unwrap();
        assert_eq!(
            segments
                .iter()
                .map(|s| (s.meta.first_ts, s.meta.last_ts))
                .collect::<Vec<_>>(),
            (0..6)
                .map(|i| (ANCHOR + 2 * i * SECOND, ANCHOR + (2 * i + 1) * SECOND))
                .collect::<Vec<_>>(),
            "a dump reads the database as it stood when its snapshot opened; \
             eviction afterwards cannot reach into it"
        );
        for s in &segments {
            let table = read_table_parquet("cpu_usage".to_string(), s.bytes.clone())
                .expect("a dumped segment's bytes must still be readable parquet");
            assert_eq!(table.timestamps.len(), s.meta.rows as usize);
        }
    }

    #[test]
    fn a_dump_can_be_trimmed_to_a_time_range() {
        // `/dump?start=&end=` survives the migration, but at segment
        // granularity — so the caller is told the span it actually got rather
        // than the one it asked for.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.rez");
        let mut buf =
            HindsightBuffer::create(&path, seed(), Duration::from_secs(3600), seal_every(4))
                .unwrap();
        for i in 0..12u64 {
            let (s, ts) = tick(&["cpu_usage"], i);
            buf.ingest(&s, ts, 0).unwrap();
            buf.maintain().unwrap();
        }
        drop(buf);

        // Segments cover 0-3, 4-7, 8-11. Ask for [5s, 6s]: the straddling
        // segment is kept whole, its neighbours go.
        let range = TimeRange::new(
            Some(std::time::UNIX_EPOCH + Duration::from_nanos(ANCHOR + 5 * SECOND)),
            Some(std::time::UNIX_EPOCH + Duration::from_nanos(ANCHOR + 6 * SECOND)),
        );
        let dest = dir.path().join("dump.rez");
        let summary = dump(&path, &dest, &range).unwrap();

        assert_eq!(summary.rows, 4, "one segment survives the trim");
        assert_eq!(summary.first_ts, Some(ANCHOR + 4 * SECOND));
        assert_eq!(summary.last_ts, Some(ANCHOR + 7 * SECOND));
        // The untrimmed buffer is not what got trimmed.
        assert_eq!(summarize(&path).unwrap().rows, 12);
    }
}
