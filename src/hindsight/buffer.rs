//! The hindsight rolling buffer: **an archive recording with retention**, and
//! nothing else. See docs/journal/2026-08-12-rez-sqlite-container.md
//! § "Hindsight" and docs/journal/2026-09-28-dendro-writer-adoption.md,
//! "B: built". The buffer is a dendro archive written through
//! metriken-storage (`create_dendro`, `dump`); a `.rez` buffer was removed in
//! 6.0.
//!
//! What this replaces is a fixed-size ring of 4 KB-aligned slots
//! (`snapshot_len` × `snapshot_count`) written round-robin and overwritten in
//! place, with a separate msgpack→parquet path to get data out of it. Two
//! things were wrong with that, and the second is the one that mattered:
//!
//! 1. **Nothing could read it.** The ring was a private on-disk format with one
//!    consumer — its own dump routine. The buffer is an ordinary archive: the
//!    viewer, the MCP tools and `recording metadata` open it with no special
//!    casing, live, while it is being written.
//! 2. **A dump could tear**, because it copied a buffer that was being written
//!    in place. Here the buffer is immutable sealed segments plus a WAL, so a
//!    dump is a copy taken inside one read snapshot, consistent by
//!    construction.
//!
//! Retention is `SourceRecorder::evict_before` each tick. The file stays
//! bounded because freed pages are reused.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::state::TimeRange;
use crate::recorder::rez_v3_writer::ManifestSeed;
use crate::recorder::seal_policy::SealPolicy;

/// The rolling buffer. One recording, fed a row per tick, trimmed to the
/// configured lookback every tick.
///
/// The recorder is **declared before the writer deliberately.** Fields drop
/// in declaration order, and dropping the writer joins its thread. Dropping
/// it first would not hang — `join` sends a shutdown before releasing its own
/// sender, so the writer stops whoever still holds a clone — but it would stop
/// the writer while the recorder could still queue a final seal, and that work
/// would be dropped.
pub struct HindsightBuffer {
    rec: metriken_storage::SourceRecorder,
    writer: metriken_storage::ArchiveWriter,
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
    /// The recording's metadata as last written, for the restart patch to be
    /// computed against.
    metadata: BTreeMap<String, String>,
}

/// Row time the dendro writer lets pass between restatements of a long
/// table's live occupants: the writer's default, which the buffer uses.
fn restate_every_ns() -> u64 {
    metriken_storage::WriterConfig::default().restate_every_ns
}

fn archive_err(e: Box<dyn std::error::Error + Send + Sync>) -> String {
    e.to_string()
}

impl HindsightBuffer {
    /// Create the buffer at `path`, which must not exist, through
    /// metriken-storage's writer: groups with slots are written long.
    ///
    /// The seal policy is a parameter rather than a constant because segment
    /// size has to track the interval: `[general] segment_rows` sets it
    /// and defaults to the writer's 900, which is a segment per ~15 minutes at
    /// the default 1 s interval and per ~90 seconds at 10 Hz.
    pub fn create_dendro(
        path: &Path,
        seed: ManifestSeed,
        lookback: Duration,
        policy: SealPolicy,
    ) -> Result<Self, String> {
        let config = metriken_storage::WriterConfig {
            seal: dendro::seal::SealPolicy {
                max_bytes: policy.max_bytes,
                max_rows: policy.max_rows,
                max_age: policy.max_age,
                align: None,
            },
            ..metriken_storage::WriterConfig::default()
        };
        let mut writer = metriken_storage::ArchiveWriter::create(path, config)
            .map_err(|e| format!("failed to create {}: {e}", path.display()))?;
        let metadata = seed.metadata.clone();
        let rec = writer
            .add_source(seed.labels, seed.metadata, seed.clock_anchor_wall_ns)
            .map_err(archive_err)?;
        Ok(Self {
            rec,
            writer,
            lookback,
            newest_ts: None,
            first_ts: None,
            metadata,
        })
    }

    /// Write agent restarts into the buffer's metadata: see
    /// [`crate::recorder::restart::metadata_patch`].
    pub fn record_restarts(
        &mut self,
        restarts: &[crate::recorder::restart::Restart],
        agent: &crate::recorder::restart::Agent,
    ) -> Result<(), String> {
        let patch = crate::recorder::restart::metadata_patch(
            &self.metadata,
            self.first_ts,
            restarts,
            agent,
            false,
        );
        let mut merged = self.metadata.clone();
        merged.extend(patch.clone());
        self.rec.update_metadata(patch).map_err(archive_err)?;
        self.metadata = merged;
        Ok(())
    }

    /// Append one snapshot, committed at once. The tests' way of filling a
    /// buffer without a stream.
    #[cfg(test)]
    pub fn ingest(
        &mut self,
        snapshot: &metriken_exposition::Snapshot,
        anchored_ts: u64,
        wall_offset_ns: i64,
    ) -> Result<(), String> {
        let staged = self
            .rec
            .stage(snapshot, anchored_ts, wall_offset_ns)
            .map_err(archive_err)?;
        self.writer.commit(vec![staged]).map_err(archive_err)?;
        self.note_ts(anchored_ts);
        Ok(())
    }

    /// Append one pass taken off the stream, staging the groups as they
    /// arrived.
    pub fn ingest_streamed(
        &mut self,
        groups: Vec<metriken_storage::StreamedGroup>,
        anchored_ts: u64,
        wall_offset_ns: i64,
    ) -> Result<(), String> {
        let staged = self
            .rec
            .stage_streamed(groups, anchored_ts, wall_offset_ns)
            .map_err(archive_err)?;
        self.writer.commit(vec![staged]).map_err(archive_err)?;
        self.note_ts(anchored_ts);
        Ok(())
    }

    fn note_ts(&mut self, ts: u64) {
        self.newest_ts = Some(self.newest_ts.map_or(ts, |t| t.max(ts)));
        self.first_ts.get_or_insert(ts);
    }

    /// Whether any row has been written into the buffer.
    pub fn has_rows(&self) -> bool {
        self.first_ts.is_some()
    }

    /// Whether retention has begun: the buffer is now dropping as much as it
    /// takes in rather than still filling.
    ///
    /// Derived from the span the recording has COVERED — newest minus the very
    /// first row, which eviction never moves — not from the span it currently
    /// retains. The retained span is the wrong instrument: it converges to just
    /// *under* the lookback (the oldest surviving row sits one tick inside the
    /// cutoff, and a row is stamped in nanoseconds), so `retained >= lookback`
    /// reads false forever at steady state. Ask the question the buffer can
    /// answer exactly instead of the one that needs a fudge factor.
    pub fn at_retention_bound(&self) -> bool {
        match (self.first_ts, self.newest_ts) {
            (Some(first), Some(newest)) => {
                Duration::from_nanos(newest.saturating_sub(first)) >= self.lookback
            }
            _ => false,
        }
    }

    /// Seal whatever is due, then apply retention. Call it every tick, whether
    /// or not an interval arrived; a writer that died asynchronously surfaces
    /// here.
    ///
    /// `maybe_seal` runs first, so a segment closed on this tick is in the
    /// catalog before the cutoff is applied to it. Occupant streams are
    /// evicted one restatement period behind their data
    /// (`SourceRecorder::evict_before`), so the oldest rows kept can still
    /// find their occupants' labels.
    pub fn maintain(&mut self) -> Result<(), String> {
        let cutoff = self.cutoff();
        self.rec.maybe_seal().map_err(archive_err)?;
        if let Some(cutoff) = cutoff {
            self.rec.evict_before(cutoff).map_err(archive_err)?;
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
        self.rec.sync().map_err(archive_err)
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
/// This is what `/status` reports, and it is deliberately stated in the
/// archive's own terms. The ring's geometry (slot size, slot count, write
/// index, "has it wrapped") described a fixed-size array that no longer
/// exists; reporting a fabricated equivalent would be worse than not
/// reporting it.
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

/// Summarize the dendro archive at `path` without disturbing whoever is
/// writing it. In WAL mode this reader never blocks the writer and is never
/// blocked by it.
pub fn summarize(path: &Path) -> Result<Summary, String> {
    let mut out =
        summarize_dendro(&dendro::archive::Archive::open(path).map_err(|e| e.to_string())?)?;
    out.bytes = bytes_on_disk(path);
    Ok(out)
}

/// The figures of [`Summary`] from dendro's catalog. A long table's occupant
/// stream (`<table>/occupants`) is listed as a table of its own, as it is
/// stored.
fn summarize_dendro(db: &dendro::archive::Archive) -> Result<Summary, String> {
    let err = |e: dendro::Error| e.to_string();
    let pages = db.page_stats().map_err(err)?;
    let mut out = Summary {
        free_pages: pages.free,
        pages: pages.pages,
        ..Summary::default()
    };
    let widen = |out: &mut Summary, first: Option<i64>, last: Option<i64>| {
        let (first, last) = (
            first.and_then(|t| u64::try_from(t).ok()),
            last.and_then(|t| u64::try_from(t).ok()),
        );
        out.first_ts = match (out.first_ts, first) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        out.last_ts = match (out.last_ts, last) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    };
    for source in db.read_sources().map_err(err)? {
        for stream in db.all_streams(source.id).map_err(err)? {
            let (segments, span) = db.segment_span(source.id, &stream).map_err(err)?;
            let live = db.live_wal_span(source.id, &stream).map_err(err)?;
            widen(&mut out, span.first_ts, span.last_ts);
            widen(&mut out, live.first_ts, live.last_ts);
            out.rows += span.rows + live.rows;
            out.tables.push(TableSummary {
                sampler: stream,
                rows: span.rows + live.rows,
                segments,
                live_wal_rows: live.rows,
            });
        }
    }
    Ok(out)
}

/// The database plus its `-wal`/`-shm` sidecars. The sidecar is real disk
/// usage and is capped rather than unbounded, but reporting only the main
/// file would under-count a buffer that is actively being written.
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

/// Write the buffer out to `dest` as a standalone, finished dendro archive,
/// replacing whatever was there.
///
/// This is the whole of what `perform_dump_to_file` used to do by walking ring
/// slots and running a msgpack→parquet conversion over them.
///
/// Both the whole buffer and a range go through dendro's
/// `copy_sources_into`: whole segments overlapping the range, copied as they
/// are, the live WAL tail encoded into a final segment through
/// metriken-storage's encoder, and the clock offsets, all in one read
/// snapshot of the buffer. The writer keeps committing throughout and the
/// copy cannot tear. The dump is fully sealed: a reader opening it has no WAL
/// tail to rebuild.
///
/// `range` trims the copy — never the buffer — at SEGMENT granularity, so a
/// dump reaches at least as far as asked and possibly further. The returned
/// span says exactly how far, which is why the caller reports it back rather
/// than echoing the request. A range starts one restatement period early when
/// the buffer has long tables: an occupant's labels are written when it is
/// first seen and restated every period, so one first seen before `start` is
/// named only by a restatement at most a period before it.
///
/// The copy is staged beside `dest` and renamed into place, so a failed dump
/// leaves the previous one intact and no consumer ever sees a half-written
/// file at `dest`.
pub fn dump(buffer: &Path, dest: &Path, range: &TimeRange) -> Result<Summary, String> {
    use dendro::archive::{Archive, ArchiveMut};
    use dendro::rewrite::CopySpec;
    let err = |e: dendro::Error| e.to_string();

    let parent = dest.parent().filter(|p| !p.as_os_str().is_empty());
    let staging = match parent {
        Some(dir) => tempfile::tempdir_in(dir),
        None => tempfile::tempdir(),
    }
    .map_err(|e| format!("failed to stage the dump beside {}: {e}", dest.display()))?;
    let staged = staging.path().join("dump.dendro");

    // A second connection, deliberately: the writer thread owns its own.
    let src = Archive::open(buffer).map_err(err)?;
    let streams = crate::dendro_copy::streams(&src)?;
    let spec = match (range.start_ns(), range.end_ns()) {
        (None, None) => CopySpec::everything(),
        (start, end) => {
            let long = streams
                .iter()
                .any(|s| ::rez::occupants::table_of(s).is_some());
            let lead = if long { restate_every_ns() } else { 0 };
            let start = start.unwrap_or(0).saturating_sub(lead);
            CopySpec {
                start: i64::try_from(start).unwrap_or(i64::MAX),
                end: end.map_or(i64::MAX, |e| i64::try_from(e).unwrap_or(i64::MAX)),
                ..CopySpec::everything()
            }
        }
    };
    crate::dendro_copy::copy(&src, &staged, &spec)?;
    drop(src);

    // The buffer's source is still running and so is never complete; this
    // copy of it is finished by definition. Without this every dump would
    // open with a "not cleanly finalized, data may be missing" warning that
    // says nothing true about the artifact.
    let mut db = ArchiveMut::open(&staged).map_err(err)?;
    let ids: Vec<i64> = db
        .read_sources()
        .map_err(err)?
        .iter()
        .map(|s| s.id)
        .collect();
    for id in ids {
        db.mark_complete(id).map_err(err)?;
    }
    // Summarized before the rename, through the write handle; reopening the
    // output read-only would leave `-wal`/`-shm` sidecars beside it, which
    // only a write handle's close removes.
    let mut summary = summarize_dendro(&db)?;
    // Dropping the write handle folds its sidecar into the file and removes
    // it, so what gets renamed is the whole archive and nothing beside it.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::rez::recorder_tests_support::{counter, snap};
    use metriken_exposition::Snapshot;
    use metriken_storage::Catalog;
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

    /// A policy that seals every `rows` rows (dendro staggers each stream's
    /// first segment, so the first seal may come sooner).
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
                    Some(::rez::window::Window::new(ts - SECOND / 2, ts)),
                )
            })
            .collect();
        (snap(ts, counters), ts)
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
        let path = dir.path().join("buffer.dendro");
        let mut buf =
            HindsightBuffer::create_dendro(&path, seed(), Duration::from_secs(5), seal_every(2))
                .unwrap();
        for i in 0..4u64 {
            let (s, ts) = tick(&["cpu_usage"], i);
            buf.ingest(&s, ts, 0).unwrap();
            buf.maintain().unwrap();
        }
        // No further ingest: the source has gone away. Retention keeps running.
        for _ in 0..5 {
            buf.maintain().unwrap();
        }
        buf.sync().unwrap();

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
        //
        // A 4.5 s lookback against 1 s ticks, so the cutoff lands BETWEEN two
        // rows: in the real loop the same misalignment comes from tick
        // jitter, since rows are stamped in nanoseconds and never land
        // exactly a lookback apart.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.dendro");
        let lookback = Duration::from_millis(4_500);
        let mut buf =
            HindsightBuffer::create_dendro(&path, seed(), lookback, seal_every(usize::MAX))
                .unwrap();
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

        // And the retained span is now just UNDER the lookback: these rows
        // are unsealed, so retention drops them one at a time and the oldest
        // survivor sits strictly inside the cutoff. `summarize` opens its own
        // connection, so the barrier makes it see the eviction above.
        buf.sync().unwrap();
        let retained = summarize(&path).unwrap().retained().unwrap();
        assert!(
            retained < lookback,
            "retained {retained:?} — a span-based check would read false here \
             forever, even though eviction is plainly running"
        );
    }

    /// A table's sealed segments as `(first_ts, last_ts)`, and its live WAL
    /// stamps.
    type Timeline = (Vec<(u64, u64)>, Vec<u64>);

    /// The tables of the one source in the dendro archive at `path`.
    fn timelines(path: &Path) -> BTreeMap<String, Timeline> {
        let catalog = metriken_storage::DendroCatalog::open(path).unwrap();
        let id = catalog.sources().unwrap()[0].id;
        catalog
            .tables(id)
            .unwrap()
            .into_iter()
            .map(|table| {
                let segments = catalog
                    .segment_meta(id, &table)
                    .unwrap()
                    .into_iter()
                    .map(|(_, m)| (m.first_ts, m.last_ts))
                    .collect();
                let wal = catalog
                    .live_wal(id, &table)
                    .unwrap()
                    .iter()
                    .map(|r| r.ts)
                    .collect();
                (table, (segments, wal))
            })
            .collect()
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
        let path = dir.path().join("buffer.dendro");
        let dest = dir.path().join("dump.dendro");
        let mut buf =
            HindsightBuffer::create_dendro(&path, seed(), Duration::from_secs(3600), seal_every(8))
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

        assert!(
            metriken_storage::DendroCatalog::is_archive(&dest).unwrap(),
            "the dump is a dendro archive"
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

        // Self-consistent: the dump is fully sealed, and each sampler's
        // segments form one increasing timeline that starts where the
        // recording did, with no segment overlapping the one before it.
        let catalog = metriken_storage::DendroCatalog::open(&dest).unwrap();
        assert!(
            catalog.sources().unwrap()[0].complete,
            "a dump is a finished artifact even though the buffer runs on"
        );
        drop(catalog);
        let tables = timelines(&dest);
        assert_eq!(
            tables.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["cpu_usage", "scheduler"]
        );
        for (table, (segments, wal)) in tables {
            assert!(wal.is_empty(), "{table}: a dump has no WAL rows");
            assert_eq!(segments.first().map(|s| s.0), Some(ANCHOR), "{table}");
            assert!(
                segments.windows(2).all(|w| w[0].1 < w[1].0),
                "{table}: segments must form one increasing timeline: {segments:?}"
            );
        }
    }

    #[test]
    fn a_hindsight_buffer_opens_in_the_ordinary_readers() {
        // The unification. Nothing could read the slot ring — not the viewer,
        // not the MCP tools, not `recording metadata`; its only consumer was
        // its own dump routine. The buffer is read by the generic reader,
        // live, mid-recording, with no hindsight-specific casing anywhere.
        //
        // Nothing has sealed here (the policy is far out of reach), so every
        // row the reader returns comes from the live WAL — the state a running
        // buffer is in almost all the time.
        use metriken_query::MetricsSource;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.dendro");
        let mut buf = HindsightBuffer::create_dendro(
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
        buf.sync().unwrap();

        // `recording metadata` — dispatched by content, not by extension, and
        // handed a file that is still being written.
        let described = crate::parquet_tools::metadata::describe_rez_string_at(&path).unwrap();
        assert!(
            described.contains("cpu_usage"),
            "the metadata tool must name the buffer's tables: {described}"
        );

        // And the viewer/MCP front door, with real values out the other side.
        let reader = crate::rez_reader::RezReader::open_recordings(
            &path,
            metriken_storage::BufferPool::new(8 * 1024 * 1024),
        )
        .unwrap()
        .remove(0)
        .1;
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
    }

    #[test]
    fn a_ranged_dump_keeps_metric_identity_for_a_wal_only_table() {
        // A metric's metadata rides on the row that carries its schema. A
        // ranged dump that copied a *slice* of raw WAL rows would leave any
        // metric whose schema row fell before `start` with no identity:
        // values with no labels. The dump encodes the live tail into a
        // segment instead, and segments describe themselves.
        //
        // Nothing seals here, so this table's entire history is WAL rows and
        // its schema row is tick 0, well before the requested start.
        use metriken_query::MetricsSource;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.dendro");
        let mut buf = HindsightBuffer::create_dendro(
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
        buf.sync().unwrap();

        let dest = dir.path().join("dump.dendro");
        let range = TimeRange::new(
            Some(std::time::UNIX_EPOCH + Duration::from_nanos(ANCHOR + 5 * SECOND)),
            None,
        );
        dump(&path, &dest, &range).unwrap();

        // The dump is segments and nothing else — no WAL rows were copied.
        let (segments, wal) = timelines(&dest).remove("drivehealth").unwrap();
        assert!(wal.is_empty(), "{wal:?}");
        assert_eq!(segments.len(), 1, "the tail was sealed into one segment");

        // And the metric kept its identity.
        let reader = crate::rez_reader::RezReader::open_recordings(
            &dest,
            metriken_storage::BufferPool::new(8 * 1024 * 1024),
        )
        .unwrap()
        .remove(0)
        .1;
        assert_eq!(reader.counter_names(), vec!["drivehealth_ops".to_string()]);
    }

    #[test]
    fn a_dump_can_be_trimmed_to_a_time_range() {
        // `/dump?start=&end=` works at segment granularity, so the caller is
        // told the span it actually got rather than the one it asked for.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.dendro");
        let mut buf =
            HindsightBuffer::create_dendro(&path, seed(), Duration::from_secs(3600), seal_every(4))
                .unwrap();
        for i in 0..12u64 {
            let (s, ts) = tick(&["cpu_usage"], i);
            buf.ingest(&s, ts, 0).unwrap();
            buf.maintain().unwrap();
        }
        buf.sync().unwrap();

        // Ask for [5s, 6s]: the segments overlapping it are kept whole, the
        // rest go.
        let range = TimeRange::new(
            Some(std::time::UNIX_EPOCH + Duration::from_nanos(ANCHOR + 5 * SECOND)),
            Some(std::time::UNIX_EPOCH + Duration::from_nanos(ANCHOR + 6 * SECOND)),
        );
        let dest = dir.path().join("dump.dendro");
        let summary = dump(&path, &dest, &range).unwrap();

        assert!(summary.rows < 12, "the trim dropped rows: {summary:?}");
        assert!(
            summary.first_ts <= Some(ANCHOR + 5 * SECOND)
                && summary.last_ts >= Some(ANCHOR + 6 * SECOND),
            "the dump covers the range asked for: {summary:?}"
        );
        // The untrimmed buffer is not what got trimmed.
        assert_eq!(summarize(&path).unwrap().rows, 12);
    }

    /// A `.dendro` buffer with a quiet table. dendro's first
    /// segment is staggered by a hash of the stream, so the assertions are
    /// on what retention guarantees rather than on exact segment edges.
    #[test]
    fn a_dendro_buffer_evicts_whole_segments_and_quiet_wal_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.dendro");
        let mut buf =
            HindsightBuffer::create_dendro(&path, seed(), Duration::from_secs(5), seal_every(4))
                .unwrap();
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
        buf.sync().unwrap();
        let cutoff = ANCHOR + 6 * SECOND;

        let db = metriken_storage::DendroCatalog::open(&path).unwrap();
        let id = db.sources().unwrap()[0].id;
        let segments = db.segment_meta(id, "cpu_usage").unwrap();
        assert!(!segments.is_empty(), "cpu_usage sealed");
        for (_, m) in &segments {
            assert!(
                m.last_ts >= cutoff,
                "a segment wholly older than the cutoff survived: {m:?}"
            );
        }
        let wal: Vec<u64> = db
            .live_wal(id, "drivehealth")
            .unwrap()
            .iter()
            .map(|r| r.ts)
            .collect();
        assert_eq!(
            wal,
            vec![ANCHOR + 10 * SECOND],
            "drivehealth's WAL rows before the cutoff are evicted"
        );
        let summary = summarize(&path).unwrap();
        assert_eq!(summary.last_ts, Some(ANCHOR + 11 * SECOND));
        assert!(summary.first_ts.unwrap() <= cutoff);
        assert!(summary
            .tables
            .iter()
            .any(|t| t.sampler == "drivehealth" && t.live_wal_rows == 1 && t.segments == 0));
        assert!(summary.pages > 0 && summary.bytes > 0);
    }

    /// A whole-buffer dump of a live `.dendro` buffer is a complete dendro
    /// archive that reads back through the viewer's reader.
    #[test]
    fn a_dendro_dump_is_complete_and_readable() {
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.dendro");
        let mut buf =
            HindsightBuffer::create_dendro(&path, seed(), Duration::from_secs(3600), seal_every(4))
                .unwrap();
        for i in 0..10u64 {
            let (s, ts) = tick(&["cpu_usage"], i);
            buf.ingest(&s, ts, 0).unwrap();
            buf.maintain().unwrap();
        }
        buf.sync().unwrap();

        let dest = dir.path().join("dump.dendro");
        let summary = dump(&path, &dest, &TimeRange::new(None, None)).unwrap();
        assert_eq!(summary.rows, 10);
        for suffix in ["-wal", "-shm"] {
            let sidecar = dir.path().join(format!("dump.dendro{suffix}"));
            assert!(
                !sidecar.exists(),
                "the dump leaves no {suffix} sidecar beside the output"
            );
        }
        let reader = crate::rez_reader::RezReader::open_recordings(
            &dest,
            metriken_storage::BufferPool::new(64 * 1024 * 1024),
        )
        .unwrap()
        .remove(0)
        .1;
        assert!(reader.complete(), "a dump is a finished artifact");
        let (start, end) = reader.time_range().unwrap();
        let metriken_query::QueryResult::Matrix { result } = reader
            .query_range("rate(cpu_usage_ops[3s])", start, end + 1.0, 1.0)
            .unwrap()
        else {
            panic!("a matrix");
        };
        let points: Vec<f64> = result
            .iter()
            .flat_map(|s| s.values.iter().map(|v| v.1))
            .collect();
        assert!(
            !points.is_empty() && points.iter().all(|v| (v - 1.0).abs() < 1e-6),
            "{points:?}"
        );
    }

    /// A V3 snapshot of one slotted group, `threads/tasks`: the given
    /// `(slot, comm)` members, each a counter at `value`.
    fn slotted(ts: u64, members: &[(u64, &str)], value: u64) -> (Snapshot, u64) {
        use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc, SnapshotV3};
        let mut schema = GroupSchema::default();
        for (slot, comm) in members {
            schema.counters.push(MetricDesc {
                name: format!("0x{slot}"),
                metadata: [
                    ("metric", "task_ops"),
                    ("sampler", "threads"),
                    ("id", &slot.to_string()),
                    ("comm", comm),
                ]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            });
        }
        let group = GroupSnapshot {
            name: "threads/tasks".to_string(),
            schema_hash: schema.hash(),
            counters: vec![Some(value); members.len()],
            schema: Some(std::sync::Arc::new(schema)),
            window: Some(metriken::Window::new(ts - SECOND / 2, ts)),
            gauges: Vec::new(),
            histograms: Vec::new(),
        };
        (
            Snapshot::V3(SnapshotV3 {
                systemtime: std::time::UNIX_EPOCH + Duration::from_nanos(ts),
                duration: Duration::ZERO,
                metadata: Default::default(),
                groups: vec![group],
            }),
            ts,
        )
    }

    /// A ranged dump of a long table keeps the labels of an occupant first
    /// seen long before the range. Its labels were written at first sight
    /// (tick 0) and restated at tick 300; the dump of [350 s, 400 s] reaches
    /// back one restatement period, to the restatement.
    #[test]
    fn a_ranged_dendro_dump_keeps_labels_of_an_occupant_seen_before_it() {
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("buffer.dendro");
        let mut buf = HindsightBuffer::create_dendro(
            &path,
            seed(),
            Duration::from_secs(3600),
            seal_every(64),
        )
        .unwrap();
        for i in 0..400u64 {
            let (s, ts) = slotted(ANCHOR + i * SECOND, &[(0, "nginx")], i);
            buf.ingest(&s, ts, 0).unwrap();
            buf.maintain().unwrap();
        }
        buf.sync().unwrap();

        let range = TimeRange::new(
            Some(std::time::UNIX_EPOCH + Duration::from_nanos(ANCHOR + 350 * SECOND)),
            Some(std::time::UNIX_EPOCH + Duration::from_nanos(ANCHOR + 399 * SECOND)),
        );
        let dest = dir.path().join("dump.dendro");
        dump(&path, &dest, &range).unwrap();
        let reader = crate::rez_reader::RezReader::open_recordings(
            &dest,
            metriken_storage::BufferPool::new(64 * 1024 * 1024),
        )
        .unwrap()
        .remove(0)
        .1;
        let (start, end) = reader.time_range().unwrap();
        let metriken_query::QueryResult::Matrix { result } = reader
            .query_range("rate(task_ops{comm=\"nginx\"}[3s])", start, end + 1.0, 1.0)
            .unwrap()
        else {
            panic!("a matrix");
        };
        assert_eq!(result.len(), 1, "the occupant is found by its label");
        assert!(
            result[0]
                .values
                .iter()
                .any(|(t, _)| *t >= (ANCHOR + 350 * SECOND) as f64 / 1e9),
            "and has values inside the range"
        );
    }
}
