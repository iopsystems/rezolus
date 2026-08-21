//! Rewriting a v3 (SQLite) `.rez` container.
//!
//! `combine`, `filter` and `annotate` all produce a new archive from existing
//! ones without decoding a single segment: the parquet BLOBs pass through
//! byte-identical and only the catalog around them changes. Hindsight's ranged
//! dump is the same operation with a time bound, so all four share this copy
//! rather than each growing their own — the WAL-tail handling below is subtle
//! enough that a second implementation would be a second set of bugs.

use std::collections::{BTreeMap, BTreeSet};

use crate::recorder::rez::table_sampler;
use crate::recorder::rez_sqlite::{RezDb, RezTx, SegmentMeta};

/// What one copy pass carries across.
pub(crate) struct CopySpec<'a> {
    /// Row-timestamp bound in nanoseconds. The rewrite tools copy everything;
    /// hindsight's dump narrows it to the incident window.
    pub start: u64,
    pub end: u64,
    /// Keep only tables whose *sampler* — the part of a `<sampler>/<group>`
    /// key before the first `/` — is in this set. `None` keeps every table.
    ///
    /// Filtering by sampler rather than by table key is deliberate: a sampler
    /// is the unit an operator names, and under V3 one sampler owns several
    /// group tables. Dropping "the sampler" has to drop all of its groups.
    pub keep_samplers: Option<&'a BTreeSet<String>>,
    /// Extra metadata merged into each copied recording's own, overwriting on
    /// key collision. `annotate` embeds KPIs this way; the others pass `None`.
    pub metadata_extra: Option<&'a BTreeMap<String, String>>,
}

impl CopySpec<'_> {
    /// Every recording, every table, every row, metadata untouched.
    pub(crate) fn everything() -> Self {
        CopySpec {
            start: 0,
            end: u64::MAX,
            keep_samplers: None,
            metadata_extra: None,
        }
    }
}

/// Copy every recording in `src` into the open destination transaction,
/// returning how many recordings were copied.
///
/// The destination transaction is the caller's so that `combine` can fold
/// several sources into one atomic write: either the combined archive has all
/// of its inputs or it does not exist.
///
/// Recordings are inserted with fresh ids and are NOT marked complete — the
/// caller decides that, because "was this recording finished" is a property of
/// the copy's purpose, not of the copy. See `mark_copies_complete`.
pub(crate) fn copy_recordings_into(
    src: &RezDb,
    tx: &RezTx<'_>,
    spec: &CopySpec<'_>,
) -> Result<usize, String> {
    let recordings = src.read_recordings()?;
    let mut copied = 0usize;
    for rec in &recordings {
        let mut meta = rec.meta.clone();
        if let Some(extra) = spec.metadata_extra {
            for (k, v) in extra {
                meta.metadata.insert(k.clone(), v.clone());
            }
        }
        let id = tx.insert_recording(&meta)?;
        copied += 1;

        for table in src.all_samplers(rec.id)? {
            if let Some(keep) = spec.keep_samplers {
                if !keep.contains(table_sampler(&table)) {
                    continue;
                }
            }
            // `seq` is renumbered from 0 per table rather than carried over.
            // A filtered or range-bounded copy leaves holes in the source's
            // numbering, and the reader splices segments in `seq` order, so
            // the copy's own numbering has to be dense and start at zero.
            let mut seq = 0u64;
            for segment in src.segments_overlapping(rec.id, &table, spec.start, spec.end)? {
                tx.insert_segment(id, &table, seq, &segment.meta, &segment.bytes)?;
                seq += 1;
            }

            // The unsealed tail is the newest data in the archive and the only
            // data a quiet table may have at all, so it is never optional —
            // only out of range. A `.rez` still being written (a hindsight
            // buffer, or a recording combined mid-flight) keeps real rows here
            // that no segment holds yet.
            let tail = src.live_wal(rec.id, &table)?;
            let (Some(first), Some(last)) = (tail.first(), tail.last()) else {
                continue;
            };
            if last.ts < spec.start || first.ts > spec.end {
                continue;
            }
            // `first`/`tail.len()` served the range check above and nothing
            // else: the catalog's `first_ts`/`rows` come from what actually
            // materializes, because a V3 group's leading un-anchored rows are
            // real WAL rows that never reach the segment. Cataloguing the raw
            // tail's span would claim a start the bytes do not contain.
            // `last_ts` stays the raw tail's own last row — a skip is always a
            // leading run, so that one is always right.
            let materialized = crate::recorder::rez_v3_writer::materialize_wal_tail(&table, &tail)
                .map_err(|e| format!("failed to seal the {table} tail: {e}"))?;
            if let Some(materialized) = materialized {
                let meta = SegmentMeta {
                    rows: materialized.rows,
                    first_ts: materialized.first_ts,
                    last_ts: last.ts,
                };
                tx.insert_segment(id, &table, seq, &meta, &materialized.bytes)?;
            }
        }

        // Drift observations are part of the recording's identity and cost
        // nothing to carry; they are already only a handful of rows per seal.
        for (ts, offset) in src.read_clock_offsets(rec.id)? {
            tx.insert_clock_offset(id, ts, offset)?;
        }
    }
    Ok(copied)
}

/// Mark every recording in a freshly written copy complete.
///
/// A copy is finished by definition — the source may still be recording (a
/// hindsight buffer always is), but the file just written is not. Without this
/// every rewritten archive would open with "not cleanly finalized, recovered
/// from its write-ahead log", which is alarming and, for a copy, false.
pub(crate) fn mark_copies_complete(db: &mut RezDb) -> Result<(), String> {
    let ids: Vec<i64> = db.read_recordings()?.iter().map(|r| r.id).collect();
    for id in ids {
        db.mark_complete(id)?;
    }
    Ok(())
}
