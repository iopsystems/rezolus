//! Ingest a plain `.parquet` recording into a `.rez` recording.
//!
//! Reader-available (metriken-free): a pure arrow reshape plus the SQLite
//! catalog, so it runs in the browser as well as the CLI. Each parquet becomes
//! one recording, its columns split into a table per `sampler` metadata value
//! (default `unattributed`). A parquet carries no acquisition windows, so the
//! resulting tables are WINDOWLESS — the `duration` column is dropped so the
//! query engine's `duration` fallback does not fabricate a band, and no
//! `:window_*` columns are emitted, which is what makes the reader report no
//! rate uncertainty band.
//!
//! `keep_metrics`, when set, trims each table to those metrics' columns (plus
//! the structural timestamp/offset), dropping a table left with no value
//! column — the projection a Save-as-Report needs.

use std::collections::BTreeSet;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, Int64Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;

use crate::rez::{build_labels, segment_writer_props, WALL_OFFSET_COLUMN};
use crate::rez_sqlite::{RecordingMeta, RezTx, SegmentMeta};

/// The metric a value column belongs to, for `keep_metrics` matching: the
/// column name, its base before `:` (`foo` for `foo:buckets`), or the `metric`
/// metadata fallback (Prometheus numeric-id columns).
fn value_column_kept(field: &Field, keep_metrics: &BTreeSet<String>) -> bool {
    let name = field.name();
    keep_metrics.contains(name)
        || name
            .split_once(':')
            .is_some_and(|(base, _)| keep_metrics.contains(base))
        || field
            .metadata()
            .get("metric")
            .is_some_and(|m| keep_metrics.contains(m))
}

/// Ingest one `.parquet` (as bytes) as a new recording in the open destination
/// transaction. Returns how many sampler tables were written.
///
/// When `keep_metrics` is `Some`, only those metrics' columns survive and a
/// table left with no value column is dropped.
pub fn ingest_parquet_bytes(
    parquet_bytes: &[u8],
    tx: &RezTx<'_>,
    keep_metrics: Option<&BTreeSet<String>>,
) -> Result<usize, String> {
    let builder =
        ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::copy_from_slice(parquet_bytes))
            .map_err(|e| format!("input is not readable as parquet: {e}"))?;
    let schema = builder.schema().clone();
    let kv = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .cloned()
        .unwrap_or_default();
    let reader = builder
        .build()
        .map_err(|e| format!("failed to read parquet: {e}"))?;
    let batches: Vec<RecordBatch> = reader
        .collect::<Result<_, _>>()
        .map_err(|e| format!("failed to read parquet batches: {e}"))?;
    let batch = arrow::compute::concat_batches(&schema, &batches)
        .map_err(|e| format!("failed to concatenate parquet batches: {e}"))?;

    let ts_idx = schema
        .fields()
        .iter()
        .position(|f| f.name() == "timestamp")
        .ok_or_else(|| "parquet has no `timestamp` column".to_string())?;

    // Group value columns by sampler; skip structural columns and `duration`
    // (dropped so no phantom band is fabricated).
    let mut groups: std::collections::BTreeMap<String, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, f) in schema.fields().iter().enumerate() {
        if i == ts_idx {
            continue;
        }
        let name = f.name();
        let mt = f.metadata().get("metric_type").map(String::as_str);
        if name == "duration" || mt == Some("timestamp") || mt == Some("duration") {
            continue;
        }
        if mt == Some("sparse_histogram")
            || name.ends_with(":bucket_indices")
            || name.ends_with(":bucket_counts")
        {
            return Err(format!(
                "sparse-histogram column {name:?} is not supported for .rez ingest; \
                 re-record with standard (dense) histograms"
            ));
        }
        if name == WALL_OFFSET_COLUMN
            || name.ends_with(":window_begin")
            || name.ends_with(":window_width")
        {
            continue;
        }
        if let Some(keep) = keep_metrics {
            if !value_column_kept(f, keep) {
                continue;
            }
        }
        let sampler = f
            .metadata()
            .get("sampler")
            .cloned()
            .unwrap_or_else(|| "unattributed".to_string());
        groups.entry(sampler).or_default().push(i);
    }
    if groups.is_empty() {
        // No metric columns survived. With a `keep_metrics` filter this is a
        // legitimate "this side has none of the kept metrics"; without one it
        // means an empty parquet. Either way the recording carries no tables.
        let (labels, metadata) = recording_identity(&kv);
        let rec_id = tx.insert_recording(&RecordingMeta {
            labels,
            metadata,
            clock_anchor_wall_ns: 0,
        })?;
        tx.mark_complete(rec_id)?;
        return Ok(0);
    }

    let (labels, metadata) = recording_identity(&kv);
    let rec_id = tx.insert_recording(&RecordingMeta {
        labels,
        metadata,
        // An ingested parquet has no recorded clock anchor; row timestamps are
        // already absolute wall-clock nanoseconds, so no anchor is needed.
        clock_anchor_wall_ns: 0,
    })?;
    tx.mark_complete(rec_id)?;

    let ts_array: ArrayRef = batch.column(ts_idx).clone();
    let ts_u64 = ts_array
        .as_any()
        .downcast_ref::<UInt64Array>()
        .ok_or_else(|| "`timestamp` column is not UInt64".to_string())?;
    let rows = ts_u64.len() as u64;
    if rows == 0 {
        return Ok(0);
    }
    let first_ts = ts_u64.value(0);
    let last_ts = ts_u64.value(ts_u64.len() - 1);
    let ts_field = schema.field(ts_idx).clone();

    let mut tables = 0usize;
    for (sampler, idxs) in &groups {
        let mut fields: Vec<Field> = Vec::with_capacity(idxs.len() + 2);
        let mut arrays: Vec<ArrayRef> = Vec::with_capacity(idxs.len() + 2);

        fields.push(ts_field.clone());
        arrays.push(ts_array.clone());

        // A null `:wall_offset`, matching the canonical segment shape.
        fields.push(Field::new(WALL_OFFSET_COLUMN, DataType::Int64, true));
        arrays.push(Arc::new(Int64Array::from(vec![None::<i64>; rows as usize])));

        for &i in idxs {
            let f = schema.field(i);
            // Native `.rez` value columns carry a `metric` metadata key; a
            // parquet encodes the metric only in the column name. Add it so the
            // reader's metadata listing sees the metric (querying keys off
            // `metric_type`, listing off `metric`). A histogram's metric is the
            // name without the `:buckets` suffix.
            let mut md = f.metadata().clone();
            md.entry("metric".to_string()).or_insert_with(|| {
                f.name()
                    .strip_suffix(":buckets")
                    .unwrap_or(f.name())
                    .to_string()
            });
            fields.push(
                Field::new(f.name(), f.data_type().clone(), f.is_nullable()).with_metadata(md),
            );
            arrays.push(batch.column(i).clone());
        }

        let seg_schema = Arc::new(Schema::new(fields));
        let seg_batch = RecordBatch::try_new(seg_schema.clone(), arrays)
            .map_err(|e| format!("failed to build a segment batch: {e}"))?;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = ArrowWriter::try_new(&mut buf, seg_schema, Some(segment_writer_props()))
                .map_err(|e| format!("failed to open a segment writer: {e}"))?;
            w.write(&seg_batch)
                .map_err(|e| format!("failed to write a segment: {e}"))?;
            w.close()
                .map_err(|e| format!("failed to finalize a segment: {e}"))?;
        }
        let meta = SegmentMeta {
            rows,
            first_ts,
            last_ts,
        };
        tx.insert_segment(rec_id, sampler, 0, &meta, &buf)?;
        tables += 1;
    }
    Ok(tables)
}

/// Labels + metadata for the recording, from the parquet footer. Metadata is
/// the footer verbatim; labels are derived the way a live recording's are.
fn recording_identity(
    kv: &[parquet::file::metadata::KeyValue],
) -> (
    std::collections::BTreeMap<String, String>,
    std::collections::BTreeMap<String, String>,
) {
    let mut metadata: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for kvp in kv {
        if let Some(v) = &kvp.value {
            metadata.insert(kvp.key.clone(), v.clone());
        }
    }
    let source = metadata
        .get("source")
        .cloned()
        .unwrap_or_else(|| "parquet".to_string());
    let systeminfo = metadata.get("systeminfo").cloned();
    let labels = build_labels(&source, systeminfo.as_deref(), &[]);
    (labels, metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rez_sqlite::RezDb;
    use arrow::datatypes::{DataType, Field, Schema};
    use parquet::arrow::ArrowWriter;
    use parquet::file::metadata::KeyValue;
    use parquet::file::properties::WriterProperties;
    use std::collections::HashMap;

    fn field(name: &str, metric_type: &str, sampler: Option<&str>) -> Field {
        let mut md = HashMap::from([("metric_type".to_string(), metric_type.to_string())]);
        if let Some(s) = sampler {
            md.insert("sampler".to_string(), s.to_string());
        }
        Field::new(name, DataType::UInt64, metric_type == "duration").with_metadata(md)
    }

    /// A minimal rezolus-shaped parquet in bytes: timestamp + duration + two
    /// counters in different samplers, tagged with a `source`.
    fn parquet_bytes(source: &str) -> Vec<u8> {
        let schema = Arc::new(Schema::new(vec![
            field("timestamp", "timestamp", None),
            field("duration", "duration", None),
            field("cpu_cycles", "counter", Some("cpu_usage")),
            field("ctx_switches", "counter", Some("scheduler")),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(UInt64Array::from(vec![1000u64, 2000, 3000])),
                Arc::new(UInt64Array::from(vec![Some(10u64), Some(10), Some(10)])),
                Arc::new(UInt64Array::from(vec![1u64, 2, 3])),
                Arc::new(UInt64Array::from(vec![10u64, 20, 30])),
            ],
        )
        .unwrap();
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(vec![KeyValue {
                key: "source".to_string(),
                value: Some(source.to_string()),
            }]))
            .build();
        let mut buf = Vec::new();
        {
            let mut w = ArrowWriter::try_new(&mut buf, schema, Some(props)).unwrap();
            w.write(&batch).unwrap();
            w.close().unwrap();
        }
        buf
    }

    fn segment_columns(bytes: &[u8]) -> Vec<String> {
        let b =
            ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::copy_from_slice(bytes)).unwrap();
        b.schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }

    /// A parquet ingests as one recording, split into a table per sampler, and
    /// each segment is WINDOWLESS: timestamp + wall_offset + value cols, with
    /// `duration` and any window columns dropped.
    #[test]
    fn ingests_a_parquet_as_a_windowless_recording_split_by_sampler() {
        let bytes = parquet_bytes("redis");
        let mut db = RezDb::create_in_memory().unwrap();
        db.transaction(|tx| {
            assert_eq!(ingest_parquet_bytes(&bytes, tx, None)?, 2);
            Ok(())
        })
        .unwrap();

        let recs = db.read_recordings().unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(
            recs[0].meta.labels.get("source").map(String::as_str),
            Some("redis")
        );
        assert!(recs[0].complete);
        let mut tables = db.all_samplers(recs[0].id).unwrap();
        tables.sort();
        assert_eq!(
            tables,
            vec!["cpu_usage".to_string(), "scheduler".to_string()]
        );

        let segs = db.read_segments(recs[0].id, "cpu_usage").unwrap();
        assert_eq!(segs.len(), 1);
        let cols = segment_columns(&segs[0].bytes);
        assert!(cols.iter().any(|c| c == "cpu_cycles"), "{cols:?}");
        assert!(
            !cols.iter().any(|c| c == "duration"),
            "duration dropped: {cols:?}"
        );
        assert!(
            !cols.iter().any(|c| c.contains(":window")),
            "no window cols: {cols:?}"
        );
        assert!(
            !cols.iter().any(|c| c == "ctx_switches"),
            "other sampler not here: {cols:?}"
        );
        assert_eq!(segs[0].meta.rows, 3);
        assert_eq!(segs[0].meta.first_ts, 1000);
        assert_eq!(segs[0].meta.last_ts, 3000);
    }

    /// `keep_metrics` trims to the named metrics and drops a table left with no
    /// value column.
    #[test]
    fn keep_metrics_trims_and_drops_empty_tables() {
        let bytes = parquet_bytes("redis");
        let keep: BTreeSet<String> = ["cpu_cycles".to_string()].into_iter().collect();
        let mut db = RezDb::create_in_memory().unwrap();
        db.transaction(|tx| {
            assert_eq!(ingest_parquet_bytes(&bytes, tx, Some(&keep))?, 1);
            Ok(())
        })
        .unwrap();
        let recs = db.read_recordings().unwrap();
        assert_eq!(
            db.all_samplers(recs[0].id).unwrap(),
            vec!["cpu_usage".to_string()],
            "only the table holding the kept metric survives"
        );
    }

    /// The ingested archive round-trips through serialize/open_bytes and the
    /// metric is readable — proof the in-memory build produces a valid `.rez`.
    #[test]
    fn ingested_archive_serializes_and_reads_back() {
        let bytes = parquet_bytes("redis");
        let mut db = RezDb::create_in_memory().unwrap();
        db.transaction(|tx| {
            ingest_parquet_bytes(&bytes, tx, None)?;
            Ok(())
        })
        .unwrap();
        let archive = db.serialize().unwrap();

        let pool = metriken_query::BufferPool::new(8 * 1024 * 1024);
        let readers = crate::reader::RezReader::open_recordings_from_bytes(archive, pool).unwrap();
        assert_eq!(readers.len(), 1);
        assert!(readers[0].1.metric_metadata().contains_key("cpu_cycles"));
    }
}
