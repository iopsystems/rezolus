//! Rewriting a v3 (SQLite) `.rez` container.
//!
//! `combine`, `filter` and `annotate` all produce a new archive from existing
//! ones without decoding a single segment: the parquet BLOBs pass through
//! byte-identical and only the catalog around them changes. Hindsight's ranged
//! dump is the same operation with a time bound, so all four share this copy
//! rather than each growing their own — the WAL-tail handling below is subtle
//! enough that a second implementation would be a second set of bugs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::recorder::rez::table_sampler;
use crate::recorder::rez_sqlite::{RecordingMeta, RezDb, RezTx, SegmentMeta};

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
/// Each copied recording keeps its source's `complete` flag. That flag answers
/// "may data after the last row be missing", which is a property of the DATA
/// and survives being copied — a recording recovered from a checkpoint rather
/// than cleanly finalized is still truncated after a combine or a filter, and
/// claiming otherwise would hide the loss. Missing beats wrong.
///
/// The one caller that overrides it is hindsight's dump, which marks its copy
/// complete afterwards for a specific reason: the buffer it copied is
/// perpetually mid-recording and would otherwise never produce a snapshot that
/// did not warn.
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
        if rec.complete {
            tx.mark_complete(id)?;
        }
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

/// One segment's catalog facts, read from the parquet itself.
///
/// A v1/v2 manifest carries `rows` and `cadence_ns` for a whole TABLE and
/// nothing per segment, but v3's catalog is per segment and wants a time span,
/// so the numbers have to come from the bytes. Only the `timestamp` column is
/// decoded — projecting it away from a cgroup-heavy table with thousands of
/// columns is the difference between reading a few KB and reading the segment.
fn segment_catalog_facts(bytes: &[u8]) -> Result<Option<SegmentMeta>, String> {
    use arrow::array::{Array, UInt64Array};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ProjectionMask;

    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::copy_from_slice(bytes))
        .map_err(|e| format!("failed to open a segment: {e}"))?;
    let ts_idx = builder
        .parquet_schema()
        .columns()
        .iter()
        .position(|c| c.name() == "timestamp")
        .ok_or_else(|| "segment has no `timestamp` column".to_string())?;
    let mask = ProjectionMask::leaves(builder.parquet_schema(), [ts_idx]);
    let reader = builder
        .with_projection(mask)
        .build()
        .map_err(|e| format!("failed to read a segment's timestamps: {e}"))?;

    let mut rows = 0u64;
    let mut first: Option<u64> = None;
    let mut last: Option<u64> = None;
    for batch in reader {
        let batch = batch.map_err(|e| format!("failed to read a segment's timestamps: {e}"))?;
        let col = batch
            .column(0)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or_else(|| "segment `timestamp` column is not UInt64".to_string())?;
        for i in 0..col.len() {
            if col.is_null(i) {
                continue;
            }
            let ts = col.value(i);
            first.get_or_insert(ts);
            last = Some(ts);
            rows += 1;
        }
    }
    match (first, last) {
        // An empty segment carries no time span, so it has nothing to catalog
        // and is dropped rather than inserted with a fabricated one.
        (Some(first_ts), Some(last_ts)) => Ok(Some(SegmentMeta {
            rows,
            first_ts,
            last_ts,
        })),
        _ => Ok(None),
    }
}

/// One table's segments, each paired with the catalog facts read from it.
type CatalogedTable<'a> = (&'a str, Vec<(SegmentMeta, &'a Vec<u8>)>);

/// Convert a v1/v2 (tar) `.rez` archive into a v3 (SQLite) one.
///
/// Segment parquet BLOBs are carried across byte-identical — the container
/// changes, the data does not. v1 and v2 come through the same path because
/// they differ only in how the manifest names a table's segments (`file` vs
/// `files`), which the reader already normalizes.
///
/// The whole archive is held in memory during the conversion, because the v2
/// reader materializes it that way. That is the same footprint `parquet
/// combine` has always had on a v2 input, but it does bound the size of
/// archive this can upgrade in one pass.
pub(crate) fn upgrade_tar_to_v3(src: &Path, dest: &Path) -> Result<usize, String> {
    use crate::recorder::rez;

    let (manifest, recordings) = rez::read_archive_bytes(src)
        .map_err(|e| format!("failed to read {}: {e}", src.display()))?;

    let mut db = RezDb::create(dest)?;
    let mut complete_ids: Vec<i64> = Vec::new();
    let count = db.transaction(|tx| {
        let mut n = 0usize;
        for (entry, rb) in manifest.recordings.iter().zip(recordings.iter()) {
            // Catalog every segment first: a v1 manifest has no clock anchor,
            // and the earliest row is the only truthful stand-in for one.
            let mut cataloged: Vec<CatalogedTable<'_>> = Vec::new();
            let mut earliest: Option<u64> = None;
            for (sampler, segments) in &rb.tables {
                let mut kept = Vec::new();
                for bytes in segments {
                    if let Some(meta) = segment_catalog_facts(bytes)? {
                        earliest = Some(earliest.map_or(meta.first_ts, |e| e.min(meta.first_ts)));
                        kept.push((meta, bytes));
                    }
                }
                cataloged.push((sampler.as_str(), kept));
            }

            let id = tx.insert_recording(&RecordingMeta {
                labels: rb.labels.clone(),
                metadata: rb.metadata.clone(),
                clock_anchor_wall_ns: entry.clock_anchor_wall_ns.or(earliest).unwrap_or_default(),
            })?;
            n += 1;
            if rb.complete {
                complete_ids.push(id);
            }

            for (sampler, segments) in cataloged {
                for (seq, (meta, bytes)) in segments.into_iter().enumerate() {
                    tx.insert_segment(id, sampler, seq as u64, &meta, bytes)?;
                }
            }
        }
        Ok(n)
    })?;

    // Faithfully, not unconditionally: a v2 archive recovered from a
    // checkpoint rather than cleanly finalized presents as incomplete, and an
    // upgrade must not launder that into a clean one.
    for id in complete_ids {
        db.mark_complete(id)?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use crate::recorder::rez_sqlite::RezDb;

    /// Every table in the v3 schema is either copied by
    /// [`super::copy_recordings_into`] or deliberately not carried, and this
    /// test is what makes that a decision rather than an oversight.
    ///
    /// The weakness of copying instead of deleting is exactly here: a delete
    /// preserves whatever it does not remove, so schema growth is free, while
    /// a copy only carries what it was told to. Adding a table to the schema
    /// without teaching the copy about it would silently drop that table from
    /// every combined, filtered or dumped archive — a data-loss bug with no
    /// error and no symptom until someone queries for what is missing.
    ///
    /// So: adding a table here fails this test. Either copy it in
    /// `copy_recordings_into` or add it to `NOT_CARRIED` with the reason.
    #[test]
    fn every_schema_table_is_either_copied_or_deliberately_dropped() {
        /// Carried across by `copy_recordings_into`.
        const COPIED: &[&str] = &["recordings", "segments", "wal", "clock_offsets"];
        /// Not carried, and correct not to be.
        const NOT_CARRIED: &[&str] = &[
            // Written by `RezDb::create` for the destination itself; copying
            // the source's would say nothing new and could disagree.
            "schema_version",
        ];

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema.rez");
        let db = RezDb::create(&path).unwrap();

        let mut actual = db.user_table_names().unwrap();
        actual.sort();
        let mut expected: Vec<String> = COPIED
            .iter()
            .chain(NOT_CARRIED)
            .map(|s| s.to_string())
            .collect();
        expected.sort();

        assert_eq!(
            actual, expected,
            "the v3 schema changed. `copy_recordings_into` copies a fixed set of tables, so a \
             new one is silently dropped from every combined/filtered/dumped archive until it \
             is handled. Copy it, or list it in NOT_CARRIED with the reason."
        );
    }
    /// A tar archive upgrades to v3 with its data and its identity intact:
    /// segment BLOBs byte-for-byte, labels, metadata, and — the one most
    /// easily lost — the `complete` flag, so a recording recovered from a
    /// checkpoint still reads as recovered afterwards.
    #[test]
    fn upgrading_a_tar_archive_carries_data_labels_and_completeness() {
        use crate::recorder::rez;
        use crate::recorder::rez_stream::write_segmented_rez;

        let d = tempfile::tempdir().unwrap();
        // One cleanly finalized, one recovered from a checkpoint.
        let clean = write_segmented_rez(
            &d.path().join("clean.rez"),
            "rezolus",
            [("arm".to_string(), "baseline".to_string())]
                .into_iter()
                .collect(),
            &["cpu_usage", "scheduler"],
            6,
            2,
            true,
        );
        let dirty = write_segmented_rez(
            &d.path().join("dirty.rez"),
            "rezolus",
            [("arm".to_string(), "experiment".to_string())]
                .into_iter()
                .collect(),
            &["cpu_usage"],
            6,
            2,
            false,
        );

        for (src, arm, expect_complete) in
            [(&clean, "baseline", true), (&dirty, "experiment", false)]
        {
            let before = rez::read_archive_bytes(src).unwrap().1.remove(0).tables;
            let out = d.path().join(format!("{arm}-v3.rez"));
            let n = super::upgrade_tar_to_v3(src, &out).unwrap();
            assert_eq!(n, 1);

            assert_eq!(
                rez::detect_rez_format(&out).unwrap(),
                rez::RezFormat::V3Sqlite
            );
            let db = RezDb::open(&out).unwrap();
            let recordings = db.read_recordings().unwrap();
            assert_eq!(recordings.len(), 1);
            assert_eq!(
                recordings[0].meta.labels.get("arm").map(String::as_str),
                Some(arm),
                "labels come across"
            );
            assert_eq!(
                recordings[0].complete, expect_complete,
                "`complete` is a property of the data and must survive the upgrade — \
                 laundering a recovered recording into a clean one would hide the loss"
            );

            // Segment BLOBs verbatim, in order.
            for (sampler, segments) in &before {
                let got = db.read_segments(recordings[0].id, sampler).unwrap();
                assert_eq!(
                    got.iter().map(|s| s.bytes.clone()).collect::<Vec<_>>(),
                    *segments,
                    "{sampler} segments must be carried byte-for-byte"
                );
                assert!(
                    got.iter().all(|s| s.meta.first_ts <= s.meta.last_ts),
                    "{sampler} segment spans must be cataloged from the parquet itself"
                );
            }
        }
    }
}
