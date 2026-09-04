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

use crate::rez::table_sampler;
use crate::rez_sqlite::{RecordingMeta, RezDb, RezTx, SegmentMeta};

/// What one copy pass carries across.
pub struct CopySpec<'a> {
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
    /// When set, project each copied segment's parquet down to the columns for
    /// these metrics (plus the mandatory timestamp / offset / acquisition-window
    /// sidecars), decoding and re-encoding it. `None` is the fast path — segment
    /// BLOBs pass through byte-identical. This is the ONE copy that touches
    /// segment bytes; see [`project_segment_columns`]. A table left with no
    /// value column (it holds none of the kept metrics) is dropped.
    pub keep_metrics: Option<&'a BTreeSet<String>>,
}

impl CopySpec<'_> {
    /// Every recording, every table, every row, metadata untouched.
    pub fn everything() -> Self {
        CopySpec {
            start: 0,
            end: u64::MAX,
            keep_samplers: None,
            metadata_extra: None,
            keep_metrics: None,
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
pub fn copy_recordings_into(
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
                match spec.keep_metrics {
                    // Column trim re-encodes; a table with none of the kept
                    // metrics projects to no value column and is dropped (its
                    // segments simply never inserted). Row count, timestamps
                    // and windows are unchanged by a projection, so the
                    // segment's own `meta` is reused verbatim.
                    Some(keep) => {
                        if let Some(projected) = project_segment_columns(&segment.bytes, keep)? {
                            tx.insert_segment(id, &table, seq, &segment.meta, &projected)?;
                            seq += 1;
                        }
                    }
                    None => {
                        tx.insert_segment(id, &table, seq, &segment.meta, &segment.bytes)?;
                        seq += 1;
                    }
                }
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
            let materialized = crate::wal::materialize_wal_tail(&table, &tail)
                .map_err(|e| format!("failed to seal the {table} tail: {e}"))?;
            if let Some(materialized) = materialized {
                let meta = SegmentMeta {
                    rows: materialized.rows,
                    first_ts: materialized.first_ts,
                    last_ts: last.ts,
                };
                match spec.keep_metrics {
                    Some(keep) => {
                        if let Some(projected) = project_segment_columns(&materialized.bytes, keep)?
                        {
                            tx.insert_segment(id, &table, seq, &meta, &projected)?;
                        }
                    }
                    None => {
                        tx.insert_segment(id, &table, seq, &meta, &materialized.bytes)?;
                    }
                }
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

/// Columns every projection keeps regardless of which metrics are requested:
/// the timestamp, the wall-clock offset sidecar, and the table-level
/// acquisition-window pair (the BARE `:window_*`, which a V3 group table
/// applies to all of its metrics). Dropping any of these breaks the reader's
/// ability to place rows in time or band a rate.
fn is_structural_column(name: &str) -> bool {
    name == "timestamp"
        || name == crate::rez::WALL_OFFSET_COLUMN
        || name == crate::rez::WINDOW_BEGIN_COLUMN
        || name == crate::rez::WINDOW_WIDTH_COLUMN
}

/// The metric a per-metric window sidecar (`<m>:window_begin` /
/// `<m>:window_width`, a V2-derived table) belongs to. `None` for the bare
/// table-level pair (empty prefix) and for non-window columns.
fn per_metric_window_owner(name: &str) -> Option<&str> {
    name.strip_suffix(":window_begin")
        .or_else(|| name.strip_suffix(":window_width"))
        .filter(|base| !base.is_empty())
}

/// A column carrying actual metric values — not timestamp, offset, or any
/// window sidecar. The presence of at least one decides whether a table
/// survives a metric projection at all.
fn is_value_column(name: &str) -> bool {
    !is_structural_column(name) && per_metric_window_owner(name).is_none()
}

/// Whether a column survives a projection down to `keep_metrics`. Structural
/// columns always do; a per-metric window rides its metric; a value column is
/// matched by exact name, by the base before `:` (`foo` for `foo:buckets`), or
/// by the `metric` metadata fallback (Prometheus numeric-id columns).
fn keep_rez_column(f: &arrow::datatypes::Field, keep_metrics: &BTreeSet<String>) -> bool {
    let name = f.name();
    if is_structural_column(name) {
        return true;
    }
    if let Some(metric) = per_metric_window_owner(name) {
        return keep_metrics.contains(metric);
    }
    keep_metrics.contains(name)
        || name
            .split_once(':')
            .is_some_and(|(base, _)| keep_metrics.contains(base))
        || f.metadata()
            .get("metric")
            .is_some_and(|m| keep_metrics.contains(m))
}

/// Project one segment's parquet down to the columns for `keep_metrics` plus
/// the structural sidecars, decoding and re-encoding it with
/// `rez::segment_writer_props()` so the result is indistinguishable from a
/// natively-sealed segment (LZ4_RAW, no dictionary, default row groups — NOT
/// report-save's ZSTD).
///
/// A column projection changes neither the row count nor the timestamps nor
/// the windows, so the caller reuses the segment's existing `SegmentMeta`
/// unchanged. Returns `None` when no value column survives — the table holds
/// none of the kept metrics and should be dropped rather than reduced to bare
/// structural columns.
///
/// This is the one place the rewrite tools decode a segment: `combine`,
/// `filter --samplers` and `annotate` all move BLOBs verbatim, but a
/// per-column trim cannot.
pub fn project_segment_columns(
    bytes: &[u8],
    keep_metrics: &BTreeSet<String>,
) -> Result<Option<Vec<u8>>, String> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ArrowWriter;

    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::copy_from_slice(bytes))
        .map_err(|e| format!("failed to open a segment for projection: {e}"))?;
    let schema = builder.schema().clone();

    let mut indices: Vec<usize> = Vec::new();
    let mut has_value = false;
    for (i, f) in schema.fields().iter().enumerate() {
        if keep_rez_column(f, keep_metrics) {
            indices.push(i);
            has_value |= is_value_column(f.name());
        }
    }
    if !has_value {
        return Ok(None);
    }

    let projected_schema = std::sync::Arc::new(
        schema
            .project(&indices)
            .map_err(|e| format!("failed to project a segment schema: {e}"))?,
    );
    let reader = builder
        .build()
        .map_err(|e| format!("failed to read a segment for projection: {e}"))?;

    let mut buf: Vec<u8> = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(
            &mut buf,
            projected_schema,
            Some(crate::rez::segment_writer_props()),
        )
        .map_err(|e| format!("failed to open a projected segment writer: {e}"))?;
        for batch in reader {
            let batch = batch.map_err(|e| format!("failed to read a segment batch: {e}"))?;
            let projected = batch
                .project(&indices)
                .map_err(|e| format!("failed to project a segment batch: {e}"))?;
            writer
                .write(&projected)
                .map_err(|e| format!("failed to write a projected segment batch: {e}"))?;
        }
        writer
            .close()
            .map_err(|e| format!("failed to finalize a projected segment: {e}"))?;
    }
    Ok(Some(buf))
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
pub fn upgrade_tar_to_v3(src: &Path, dest: &Path) -> Result<usize, String> {
    use crate::rez;

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
    use crate::rez_sqlite::RezDb;
    use std::collections::BTreeSet;

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
        use crate::rez;
        use crate::rez_stream::write_segmented_rez;

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

    // ── column projection ──

    /// Build a segment parquet with two value columns, the table-level window
    /// pair, timestamp and wall-offset — a minimal V3 group table shape — and
    /// return its bytes plus the row count.
    fn two_metric_segment() -> (Vec<u8>, usize) {
        use arrow::array::{Int64Array, UInt64Array};
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use std::sync::Arc;

        let rows = 4usize;
        let ts: Vec<u64> = (0..rows as u64).map(|i| 1_000 + i).collect();
        let schema = Arc::new(Schema::new(vec![
            Field::new("timestamp", DataType::UInt64, false),
            Field::new(crate::rez::WALL_OFFSET_COLUMN, DataType::Int64, true),
            Field::new(crate::rez::WINDOW_BEGIN_COLUMN, DataType::Int64, true),
            Field::new(crate::rez::WINDOW_WIDTH_COLUMN, DataType::UInt64, true),
            Field::new("cpu_usage_busy", DataType::UInt64, true),
            Field::new("cpu_usage_ops", DataType::UInt64, true),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(UInt64Array::from(ts.clone())),
                Arc::new(Int64Array::from(vec![0i64; rows])),
                Arc::new(Int64Array::from(
                    ts.iter().map(|&t| t as i64).collect::<Vec<_>>(),
                )),
                Arc::new(UInt64Array::from(vec![50u64; rows])),
                Arc::new(UInt64Array::from(vec![7u64; rows])),
                Arc::new(UInt64Array::from(vec![9u64; rows])),
            ],
        )
        .unwrap();

        let mut buf = Vec::new();
        {
            let mut w =
                ArrowWriter::try_new(&mut buf, schema, Some(crate::rez::segment_writer_props()))
                    .unwrap();
            w.write(&batch).unwrap();
            w.close().unwrap();
        }
        (buf, rows)
    }

    fn segment_columns(bytes: &[u8]) -> Vec<String> {
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        let b =
            ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::copy_from_slice(bytes)).unwrap();
        b.schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }

    /// A projection keeps the requested metric's value column plus every
    /// structural sidecar (timestamp, wall-offset, the table-level window
    /// pair), drops the unrequested metric, and preserves the row count.
    #[test]
    fn projecting_keeps_requested_metric_and_all_structural_columns() {
        let (bytes, rows) = two_metric_segment();
        let keep: BTreeSet<String> = ["cpu_usage_ops".to_string()].into_iter().collect();

        let projected = super::project_segment_columns(&bytes, &keep)
            .unwrap()
            .expect("a kept metric survives, so the table is not dropped");
        let cols = segment_columns(&projected);

        assert!(
            cols.contains(&"cpu_usage_ops".to_string()),
            "kept: {cols:?}"
        );
        assert!(
            !cols.contains(&"cpu_usage_busy".to_string()),
            "the unrequested metric is dropped: {cols:?}"
        );
        for structural in [
            "timestamp",
            crate::rez::WALL_OFFSET_COLUMN,
            crate::rez::WINDOW_BEGIN_COLUMN,
            crate::rez::WINDOW_WIDTH_COLUMN,
        ] {
            assert!(
                cols.contains(&structural.to_string()),
                "structural column {structural} must survive: {cols:?}"
            );
        }

        // Row count is unchanged by a column projection.
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(projected))
            .unwrap()
            .build()
            .unwrap();
        let got: usize = reader.map(|b| b.unwrap().num_rows()).sum();
        assert_eq!(got, rows, "projection drops columns, never rows");
    }

    /// A table holding none of the kept metrics projects to no value column and
    /// is signalled for dropping (`None`) rather than reduced to bare sidecars.
    #[test]
    fn projecting_a_table_with_no_kept_metric_returns_none() {
        let (bytes, _) = two_metric_segment();
        let keep: BTreeSet<String> = ["something_else".to_string()].into_iter().collect();
        assert!(
            super::project_segment_columns(&bytes, &keep)
                .unwrap()
                .is_none(),
            "no value column survives, so the table is dropped"
        );
    }

    /// A per-metric window sidecar (`<m>:window_begin`) rides its metric: kept
    /// when the metric is kept, dropped when it is not.
    #[test]
    fn projecting_keeps_per_metric_window_only_for_kept_metrics() {
        use arrow::array::{Int64Array, UInt64Array};
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use std::sync::Arc;

        let rows = 3usize;
        let schema = Arc::new(Schema::new(vec![
            Field::new("timestamp", DataType::UInt64, false),
            Field::new("a", DataType::UInt64, true),
            Field::new("a:window_begin", DataType::Int64, true),
            Field::new("a:window_width", DataType::UInt64, true),
            Field::new("b", DataType::UInt64, true),
            Field::new("b:window_begin", DataType::Int64, true),
            Field::new("b:window_width", DataType::UInt64, true),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(UInt64Array::from(vec![1u64, 2, 3])),
                Arc::new(UInt64Array::from(vec![10u64; rows])),
                Arc::new(Int64Array::from(vec![0i64; rows])),
                Arc::new(UInt64Array::from(vec![5u64; rows])),
                Arc::new(UInt64Array::from(vec![20u64; rows])),
                Arc::new(Int64Array::from(vec![0i64; rows])),
                Arc::new(UInt64Array::from(vec![5u64; rows])),
            ],
        )
        .unwrap();
        let mut buf = Vec::new();
        {
            let mut w =
                ArrowWriter::try_new(&mut buf, schema, Some(crate::rez::segment_writer_props()))
                    .unwrap();
            w.write(&batch).unwrap();
            w.close().unwrap();
        }

        let keep: BTreeSet<String> = ["a".to_string()].into_iter().collect();
        let projected = super::project_segment_columns(&buf, &keep)
            .unwrap()
            .unwrap();
        let cols = segment_columns(&projected);
        assert!(cols.contains(&"a".to_string()));
        assert!(
            cols.contains(&"a:window_begin".to_string()),
            "kept metric's window rides it: {cols:?}"
        );
        assert!(cols.contains(&"a:window_width".to_string()));
        assert!(
            !cols.contains(&"b".to_string()),
            "dropped metric gone: {cols:?}"
        );
        assert!(
            !cols.contains(&"b:window_begin".to_string()),
            "a dropped metric's window is dropped too: {cols:?}"
        );
    }
}
