//! Ingest a plain `.parquet` recording into a `.rez` recording — the
//! parquet→`.rez` half of `combine`, so `combine a.parquet b.parquet -o out.rez`
//! assembles a multi-recording archive (one recording per input) the same way
//! `combine --ab` packed a tarball, but in the container the viewer prefers.
//!
//! **Windowless by construction.** A `.parquet` carries no acquisition-window
//! columns, so each ingested table is written WITHOUT them and the query
//! engine reports no rate uncertainty band — the honest outcome for data whose
//! true windows were never recorded. Two things make that hold:
//!
//! - The `duration` column is DROPPED. metriken-query's `ParquetReader` has a
//!   fallback that fabricates a band from `duration` when window columns are
//!   absent; a native `.rez` segment never carries `duration`, and neither may
//!   this one, or a windowless table would sprout a phantom band.
//! - No `:window_begin`/`:window_width` columns are emitted at all. (Going
//!   through the streaming writer would emit all-null window columns, which the
//!   reader reads as a degenerate zero-width band rather than "no band" — hence
//!   this arrow-level reshape instead.)
//!
//! The reshape is metriken-free (pure arrow + the SQLite catalog), so it needs
//! nothing from the write-feature ingest path beyond the segment writer props.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, Int64Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;

use crate::recorder::rez::{build_labels, segment_writer_props, WALL_OFFSET_COLUMN};
use crate::recorder::rez_sqlite::{RecordingMeta, RezTx, SegmentMeta};

/// Ingest one `.parquet` file as a new recording in the open destination
/// transaction. Returns how many sampler tables were written.
///
/// Columns are grouped into tables by their `sampler` metadata (the same key
/// the streaming writer groups on), defaulting to `"unattributed"` for a
/// source that has none (a Prometheus scrape or a row-merged multi-source
/// file). Each table becomes one windowless segment.
pub fn ingest_parquet_as_recording(
    parquet_path: &Path,
    tx: &RezTx<'_>,
) -> Result<usize, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(parquet_path)
        .map_err(|e| format!("failed to open {}: {e}", parquet_path.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| format!("{} is not readable as parquet: {e}", parquet_path.display()))?;
    let schema = builder.schema().clone();
    let kv = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .cloned()
        .unwrap_or_default();
    let reader = builder.build()?;
    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>()?;
    let batch = arrow::compute::concat_batches(&schema, &batches)?;

    let ts_idx = schema
        .fields()
        .iter()
        .position(|f| f.name() == "timestamp")
        .ok_or("parquet has no `timestamp` column")?;

    // Group value columns by sampler; skip the structural columns. `duration`
    // is deliberately dropped (see the module doc).
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
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
                "sparse-histogram column {name:?} in {} is not supported for .rez ingest; \
                 re-record with standard (dense) histograms",
                parquet_path.display()
            )
            .into());
        }
        // A `.parquet` shouldn't carry these, but never let one leak in as a
        // metric or as a band the source never actually measured.
        if name == WALL_OFFSET_COLUMN
            || name.ends_with(":window_begin")
            || name.ends_with(":window_width")
        {
            continue;
        }
        let sampler = f
            .metadata()
            .get("sampler")
            .cloned()
            .unwrap_or_else(|| "unattributed".to_string());
        groups.entry(sampler).or_default().push(i);
    }
    if groups.is_empty() {
        return Err(format!("{} has no metric columns to ingest", parquet_path.display()).into());
    }

    // Recording metadata is the parquet footer verbatim (source, systeminfo,
    // descriptions, service_queries, per_source_metadata, ...). Labels are
    // derived the same way a live recording's are.
    let mut metadata: BTreeMap<String, String> = BTreeMap::new();
    for kvp in &kv {
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
        .ok_or("`timestamp` column is not UInt64")?;
    let rows = ts_u64.len() as u64;
    if rows == 0 {
        // A recording with no rows: valid (an endpoint scraped but silent), and
        // it needs no segments.
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

        // A null `:wall_offset`, matching the canonical segment shape — the
        // reader reads an all-null column as "no wall observation" and skips
        // it, rather than fabricating zeros.
        fields.push(Field::new(WALL_OFFSET_COLUMN, DataType::Int64, true));
        arrays.push(Arc::new(Int64Array::from(vec![None::<i64>; rows as usize])));

        for &i in idxs {
            let f = schema.field(i);
            // Native `.rez` value columns carry a `metric` metadata key (the
            // canonical metric name); a parquet column encodes that only in its
            // name. Add it so the reader's metadata listing (`metric_metadata`,
            // MCP `describe-metrics`) sees the metric — querying keys off
            // `metric_type`, but listing keys off `metric`. A histogram's
            // metric is the name without the `:buckets` suffix.
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
        let seg_batch = RecordBatch::try_new(seg_schema.clone(), arrays)?;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = ArrowWriter::try_new(&mut buf, seg_schema, Some(segment_writer_props()))?;
            w.write(&seg_batch)?;
            w.close()?;
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

#[cfg(test)]
mod tests {
    use super::ingest_parquet_as_recording;
    use crate::recorder::rez_sqlite::RezDb;
    use arrow::array::UInt64Array;
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ArrowWriter;
    use parquet::file::metadata::KeyValue;
    use parquet::file::properties::WriterProperties;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;

    fn field(name: &str, metric_type: &str, sampler: Option<&str>) -> Field {
        let mut md = HashMap::from([("metric_type".to_string(), metric_type.to_string())]);
        if let Some(s) = sampler {
            md.insert("sampler".to_string(), s.to_string());
        }
        Field::new(name, DataType::UInt64, metric_type == "duration").with_metadata(md)
    }

    /// A minimal rezolus-shaped parquet: timestamp + duration + two counters in
    /// different samplers, tagged with a `source` in the footer.
    fn write_parquet(path: &Path, source: &str) {
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
        let f = std::fs::File::create(path).unwrap();
        let mut w = ArrowWriter::try_new(f, schema, Some(props)).unwrap();
        w.write(&batch).unwrap();
        w.close().unwrap();
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
    /// each table's segment is WINDOWLESS: timestamp + wall_offset + the
    /// sampler's value columns, with `duration` and any window columns dropped.
    #[test]
    fn ingests_a_parquet_as_a_windowless_recording_split_by_sampler() {
        let dir = tempfile::tempdir().unwrap();
        let pq = dir.path().join("a.parquet");
        write_parquet(&pq, "redis");
        let out = dir.path().join("out.rez");

        let mut dst = RezDb::create(&out).unwrap();
        dst.transaction(|tx| {
            ingest_parquet_as_recording(&pq, tx).map_err(|e| e.to_string())?;
            Ok(())
        })
        .unwrap();
        drop(dst);

        let db = RezDb::open(&out).unwrap();
        let recs = db.read_recordings().unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(
            recs[0].meta.labels.get("source").map(String::as_str),
            Some("redis"),
            "the recording is labelled from the parquet's source"
        );
        assert!(
            recs[0].complete,
            "an ingested finalized parquet is complete"
        );

        let mut tables = db.all_samplers(recs[0].id).unwrap();
        tables.sort();
        assert_eq!(
            tables,
            vec!["cpu_usage".to_string(), "scheduler".to_string()],
            "columns are split into a table per sampler"
        );

        let segs = db.read_segments(recs[0].id, "cpu_usage").unwrap();
        assert_eq!(segs.len(), 1);
        let cols = segment_columns(&segs[0].bytes);
        assert!(cols.iter().any(|c| c == "timestamp"), "{cols:?}");
        assert!(cols.iter().any(|c| c == "cpu_cycles"), "{cols:?}");
        assert!(
            !cols.iter().any(|c| c == "duration"),
            "duration is dropped so no phantom band is fabricated: {cols:?}"
        );
        assert!(
            !cols.iter().any(|c| c.contains(":window")),
            "a windowless table carries no window columns: {cols:?}"
        );
        assert!(
            !cols.iter().any(|c| c == "ctx_switches"),
            "another sampler's metric does not leak into this table: {cols:?}"
        );
        assert_eq!(segs[0].meta.rows, 3);
        assert_eq!(segs[0].meta.first_ts, 1000);
        assert_eq!(segs[0].meta.last_ts, 3000);
    }

    /// The ingested archive opens through the production reader and the metric
    /// is queryable — the windowless segment is a valid `.rez`, not just a
    /// smaller blob.
    #[test]
    fn ingested_archive_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let pq = dir.path().join("a.parquet");
        write_parquet(&pq, "redis");
        let out = dir.path().join("out.rez");

        let mut dst = RezDb::create(&out).unwrap();
        dst.transaction(|tx| {
            ingest_parquet_as_recording(&pq, tx).map_err(|e| e.to_string())?;
            Ok(())
        })
        .unwrap();
        drop(dst);

        let pool = metriken_query::BufferPool::new(8 * 1024 * 1024);
        let readers = crate::rez_reader::RezReader::open_recordings(&out, pool).unwrap();
        assert_eq!(readers.len(), 1);
        let md = readers[0].1.metric_metadata();
        assert!(md.contains_key("cpu_cycles"), "cpu_cycles readable: {md:?}");
        assert!(md.contains_key("ctx_switches"), "ctx_switches readable");
    }
}
