//! The `.rez` per-sampler archive: an uncompressed tar of `manifest.json` plus
//! one `<sampler>.parquet` table per sampler. See the Stage-3 plan header for
//! the format decisions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// `.rez` manifest schema version.
pub const REZ_SCHEMA_VERSION: u32 = 1;
/// Manifest filename inside the tar.
pub const REZ_MANIFEST_NAME: &str = "manifest.json";

/// Top-level `.rez` manifest (`manifest.json`).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RezManifest {
    pub version: u32,
    /// File-level metadata: the existing `parquet_metadata` keys
    /// (`source`, `systeminfo`, `sampling_interval_ms`, `descriptions`, ...).
    pub metadata: BTreeMap<String, String>,
    pub tables: Vec<RezTableIndex>,
}

/// One entry in the manifest's table index.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RezTableIndex {
    pub sampler: String,
    pub file: String,
    pub columns: Vec<String>,
    pub rows: u64,
    /// Observed mean row interval (ns); `None` when fewer than 2 rows.
    pub cadence_ns: Option<u64>,
}

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, Int64Array, ListArray, ListBuilder, UInt64Array, UInt64Builder,
};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use metriken::Window;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;

/// Per-metric column values for a table (row-aligned with the table's timestamps).
#[derive(Debug, Clone, PartialEq)]
pub enum RezValues {
    Counter(Vec<Option<u64>>),
    Gauge(Vec<Option<i64>>),
    Histogram(Vec<Option<histogram::Histogram>>),
}

/// One metric column plus its per-row acquisition windows.
#[derive(Debug, Clone)]
pub struct RezColumn {
    /// Column key (the snapshot entry's numeric-id name, e.g. `"5"` / `"5x3"`).
    pub name: String,
    /// Metric identity + annotations (`metric`, `sampler`, labels, `metric_type`).
    pub metadata: HashMap<String, String>,
    pub values: RezValues,
    pub windows: Vec<Option<Window>>,
}

/// One sampler's table: a timestamp column plus its metric/window columns.
#[derive(Debug, Clone)]
pub struct RezTable {
    pub sampler: String,
    pub timestamps: Vec<u64>,
    pub columns: Vec<RezColumn>,
}

type RezError = Box<dyn std::error::Error>;

/// Mean row interval hint; `None` when fewer than 2 rows.
pub fn cadence_hint(timestamps: &[u64]) -> Option<u64> {
    if timestamps.len() < 2 {
        return None;
    }
    let span = timestamps.last().unwrap().saturating_sub(timestamps[0]);
    Some(span / (timestamps.len() as u64 - 1))
}

fn window_offset_columns(
    windows: &[Option<Window>],
    ts: &[u64],
) -> (Vec<Option<i64>>, Vec<Option<u64>>) {
    let mut begin = Vec::with_capacity(windows.len());
    let mut width = Vec::with_capacity(windows.len());
    for (w, &t) in windows.iter().zip(ts.iter()) {
        match w {
            Some(win) => {
                begin.push(Some(win.begin_ns as i64 - t as i64));
                width.push(Some(win.width_ns()));
            }
            None => {
                begin.push(None);
                width.push(None);
            }
        }
    }
    (begin, width)
}

fn build_histogram_list(values: &[Option<histogram::Histogram>]) -> ArrayRef {
    let mut b = ListBuilder::new(UInt64Builder::new());
    for v in values {
        match v {
            Some(h) => {
                for &c in h.as_slice() {
                    b.values().append_value(c);
                }
                b.append(true);
            }
            None => b.append(false),
        }
    }
    Arc::new(b.finish())
}

fn table_to_batch(table: &RezTable) -> Result<(Arc<Schema>, RecordBatch), RezError> {
    let mut fields: Vec<Field> = Vec::new();
    let mut arrays: Vec<ArrayRef> = Vec::new();

    fields.push(
        Field::new("timestamp", DataType::UInt64, false).with_metadata(HashMap::from([
            ("metric_type".to_string(), "timestamp".to_string()),
            ("unit".to_string(), "nanoseconds".to_string()),
        ])),
    );
    arrays.push(Arc::new(UInt64Array::from(table.timestamps.clone())));

    for col in &table.columns {
        match &col.values {
            RezValues::Counter(v) => {
                fields.push(
                    Field::new(&col.name, DataType::UInt64, true)
                        .with_metadata(col.metadata.clone()),
                );
                arrays.push(Arc::new(UInt64Array::from(v.clone())));
            }
            RezValues::Gauge(v) => {
                fields.push(
                    Field::new(&col.name, DataType::Int64, true)
                        .with_metadata(col.metadata.clone()),
                );
                arrays.push(Arc::new(Int64Array::from(v.clone())));
            }
            RezValues::Histogram(v) => {
                let arr = build_histogram_list(v);
                fields.push(
                    Field::new(
                        format!("{}:buckets", col.name),
                        arr.data_type().clone(),
                        true,
                    )
                    .with_metadata(col.metadata.clone()),
                );
                arrays.push(arr);
            }
        }

        let (begin, width) = window_offset_columns(&col.windows, &table.timestamps);
        fields.push(Field::new(
            format!("{}:window_begin", col.name),
            DataType::Int64,
            true,
        ));
        arrays.push(Arc::new(Int64Array::from(begin)));
        fields.push(Field::new(
            format!("{}:window_width", col.name),
            DataType::UInt64,
            true,
        ));
        arrays.push(Arc::new(UInt64Array::from(width)));
    }

    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(schema.clone(), arrays)?;
    Ok((schema, batch))
}

/// Serialize one table to parquet bytes.
pub fn write_table_parquet(table: &RezTable) -> Result<Vec<u8>, RezError> {
    let (schema, batch) = table_to_batch(table)?;
    let mut buf: Vec<u8> = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut buf, schema, None)?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(buf)
}

fn u64_col(a: &ArrayRef) -> &UInt64Array {
    a.as_any()
        .downcast_ref::<UInt64Array>()
        .expect("UInt64 column")
}

/// Deserialize one table from parquet bytes.
pub fn read_table_parquet(sampler: String, bytes: Vec<u8>) -> Result<RezTable, RezError> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))?.build()?;

    let mut timestamps: Vec<u64> = Vec::new();
    let mut order: Vec<String> = Vec::new();
    let mut values: HashMap<String, RezValues> = HashMap::new();
    let mut metas: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut begins: HashMap<String, Vec<Option<i64>>> = HashMap::new();
    let mut widths: HashMap<String, Vec<Option<u64>>> = HashMap::new();

    for batch in reader {
        let batch = batch?;
        let schema = batch.schema();
        for i in 0..batch.num_columns() {
            let field = schema.field(i);
            let name = field.name();
            let col = batch.column(i);
            if name == "timestamp" {
                let a = u64_col(col);
                timestamps.extend((0..a.len()).map(|r| a.value(r)));
            } else if let Some(base) = name.strip_suffix(":window_begin") {
                let a = col
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .expect("i64 window_begin");
                begins
                    .entry(base.to_string())
                    .or_default()
                    .extend((0..a.len()).map(|r| (!a.is_null(r)).then(|| a.value(r))));
            } else if let Some(base) = name.strip_suffix(":window_width") {
                let a = u64_col(col);
                widths
                    .entry(base.to_string())
                    .or_default()
                    .extend((0..a.len()).map(|r| (!a.is_null(r)).then(|| a.value(r))));
            } else if let Some(base) = name.strip_suffix(":buckets") {
                let meta = field.metadata().clone();
                let gp: u8 = meta
                    .get("grouping_power")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let mvp: u8 = meta
                    .get("max_value_power")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let list = col
                    .as_any()
                    .downcast_ref::<ListArray>()
                    .expect("list histogram");
                let entry = match values.entry(base.to_string()) {
                    std::collections::hash_map::Entry::Vacant(v) => {
                        order.push(base.to_string());
                        metas.insert(base.to_string(), meta);
                        v.insert(RezValues::Histogram(Vec::new()))
                    }
                    std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                };
                if let RezValues::Histogram(hs) = entry {
                    for r in 0..list.len() {
                        if list.is_null(r) {
                            hs.push(None);
                        } else {
                            let vals = list.value(r);
                            let a = u64_col(&vals);
                            let buckets: Vec<u64> = (0..a.len()).map(|k| a.value(k)).collect();
                            hs.push(Some(histogram::Histogram::from_buckets(gp, mvp, buckets)?));
                        }
                    }
                }
            } else {
                // A metric value column: counter (UInt64) or gauge (Int64).
                let meta = field.metadata().clone();
                let is_gauge = meta.get("metric_type").map(String::as_str) == Some("gauge");
                let entry = match values.entry(name.to_string()) {
                    std::collections::hash_map::Entry::Vacant(v) => {
                        order.push(name.to_string());
                        metas.insert(name.to_string(), meta);
                        v.insert(if is_gauge {
                            RezValues::Gauge(Vec::new())
                        } else {
                            RezValues::Counter(Vec::new())
                        })
                    }
                    std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                };
                match entry {
                    RezValues::Counter(vs) => {
                        let a = u64_col(col);
                        vs.extend((0..a.len()).map(|r| (!a.is_null(r)).then(|| a.value(r))));
                    }
                    RezValues::Gauge(vs) => {
                        let a = col
                            .as_any()
                            .downcast_ref::<Int64Array>()
                            .expect("i64 gauge");
                        vs.extend((0..a.len()).map(|r| (!a.is_null(r)).then(|| a.value(r))));
                    }
                    RezValues::Histogram(_) => {}
                }
            }
        }
    }

    let columns = order
        .into_iter()
        .map(|base| {
            let begin = begins.remove(&base).unwrap_or_default();
            let width = widths.remove(&base).unwrap_or_default();
            let windows = (0..timestamps.len())
                .map(|r| {
                    match (
                        begin.get(r).copied().flatten(),
                        width.get(r).copied().flatten(),
                    ) {
                        (Some(b), Some(w)) => {
                            let begin_ns = (timestamps[r] as i64 + b) as u64;
                            Some(Window::new(begin_ns, begin_ns + w))
                        }
                        _ => None,
                    }
                })
                .collect();
            RezColumn {
                metadata: metas.remove(&base).unwrap_or_default(),
                values: values.remove(&base).unwrap(),
                windows,
                name: base,
            }
        })
        .collect();

    Ok(RezTable {
        sampler,
        timestamps,
        columns,
    })
}

use std::io::Read;
use std::path::Path;

/// A decoded `.rez` archive (round-trip / test surface).
pub struct RezArchive {
    pub manifest: RezManifest,
    pub tables: Vec<RezTable>,
}

fn append_tar_entry<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    name: &str,
    bytes: &[u8],
) -> Result<(), RezError> {
    let mut header = tar::Header::new_gnu();
    header.set_path(name)?;
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append(&header, bytes)?;
    Ok(())
}

/// Write `tables` + `metadata` to an uncompressed `.rez` tar at `path`.
pub fn write_archive(
    path: &Path,
    tables: &[RezTable],
    metadata: BTreeMap<String, String>,
) -> Result<(), RezError> {
    let mut index = Vec::with_capacity(tables.len());
    let mut table_files: Vec<(String, Vec<u8>)> = Vec::with_capacity(tables.len());
    for table in tables {
        let file = format!("{}.parquet", table.sampler);
        let bytes = write_table_parquet(table)?;
        index.push(RezTableIndex {
            sampler: table.sampler.clone(),
            file: file.clone(),
            columns: table.columns.iter().map(|c| c.name.clone()).collect(),
            rows: table.timestamps.len() as u64,
            cadence_ns: cadence_hint(&table.timestamps),
        });
        table_files.push((file, bytes));
    }
    let manifest = RezManifest {
        version: REZ_SCHEMA_VERSION,
        metadata,
        tables: index,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;

    let out = std::fs::File::create(path)?;
    let mut builder = tar::Builder::new(out);
    builder.mode(tar::HeaderMode::Deterministic);
    append_tar_entry(&mut builder, REZ_MANIFEST_NAME, &manifest_bytes)?;
    for (name, bytes) in &table_files {
        append_tar_entry(&mut builder, name, bytes)?;
    }
    builder.into_inner()?.sync_all()?;
    Ok(())
}

/// Read a `.rez` archive back into its manifest + decoded tables.
pub fn read_archive(path: &Path) -> Result<RezArchive, RezError> {
    let file = std::fs::File::open(path)?;
    let mut archive = tar::Archive::new(file);

    let mut manifest: Option<RezManifest> = None;
    let mut parquet_bytes: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path()?.to_string_lossy().into_owned();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        if name == REZ_MANIFEST_NAME {
            manifest = Some(serde_json::from_slice(&buf)?);
        } else if name.ends_with(".parquet") {
            parquet_bytes.insert(name, buf);
        }
    }
    let manifest = manifest.ok_or("missing manifest.json")?;

    let mut tables = Vec::with_capacity(manifest.tables.len());
    for idx in &manifest.tables {
        let bytes = parquet_bytes
            .remove(&idx.file)
            .ok_or_else(|| format!("missing table file {}", idx.file))?;
        tables.push(read_table_parquet(idx.sampler.clone(), bytes)?);
    }
    Ok(RezArchive { manifest, tables })
}

#[cfg(test)]
mod manifest_tests {
    use super::*;

    #[test]
    fn manifest_json_round_trips() {
        let m = RezManifest {
            version: REZ_SCHEMA_VERSION,
            metadata: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            tables: vec![RezTableIndex {
                sampler: "cpu_usage".to_string(),
                file: "cpu_usage.parquet".to_string(),
                columns: vec!["5".to_string()],
                rows: 3,
                cadence_ns: Some(1_000_000_000),
            }],
        };
        let bytes = serde_json::to_vec(&m).unwrap();
        let back: RezManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(m, back);
        assert_eq!(REZ_MANIFEST_NAME, "manifest.json");
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;
    use metriken::Window;
    use std::collections::HashMap;

    fn meta(metric: &str, mtype: &str) -> HashMap<String, String> {
        [
            ("metric".to_string(), metric.to_string()),
            ("sampler".to_string(), "s".to_string()),
            ("metric_type".to_string(), mtype.to_string()),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn single_table_parquet_round_trips_values_and_windows() {
        let ts = vec![1_000u64, 2_000u64];
        let table = RezTable {
            sampler: "s".to_string(),
            timestamps: ts.clone(),
            columns: vec![
                RezColumn {
                    name: "0".to_string(),
                    metadata: meta("c", "counter"),
                    values: RezValues::Counter(vec![Some(10), Some(20)]),
                    windows: vec![
                        Some(Window::new(900, 1_000)),
                        Some(Window::new(1_900, 2_050)),
                    ],
                },
                RezColumn {
                    name: "1".to_string(),
                    metadata: meta("g", "gauge"),
                    values: RezValues::Gauge(vec![Some(-5), None]),
                    windows: vec![None, None],
                },
                RezColumn {
                    name: "2".to_string(),
                    metadata: {
                        let mut m = meta("h", "histogram");
                        m.insert("grouping_power".to_string(), "1".to_string());
                        m.insert("max_value_power".to_string(), "3".to_string());
                        m
                    },
                    values: RezValues::Histogram(vec![
                        Some(
                            histogram::Histogram::from_buckets(1, 3, vec![0, 1, 1, 0, 0, 0])
                                .unwrap(),
                        ),
                        None,
                    ]),
                    windows: vec![Some(Window::new(800, 1_000)), None],
                },
            ],
        };

        let bytes = write_table_parquet(&table).unwrap();
        let back = read_table_parquet("s".to_string(), bytes).unwrap();

        assert_eq!(back.timestamps, ts);
        assert_eq!(back.columns.len(), 3);
        // counter values + per-row windows preserved
        match &back.columns[0].values {
            RezValues::Counter(v) => assert_eq!(v, &vec![Some(10), Some(20)]),
            _ => panic!("expected counter"),
        }
        assert_eq!(
            back.columns[0].windows,
            vec![
                Some(Window::new(900, 1_000)),
                Some(Window::new(1_900, 2_050))
            ]
        );
        // gauge nulls + null windows preserved
        match &back.columns[1].values {
            RezValues::Gauge(v) => assert_eq!(v, &vec![Some(-5), None]),
            _ => panic!("expected gauge"),
        }
        assert_eq!(back.columns[1].windows, vec![None, None]);
        // histogram buckets preserved
        match &back.columns[2].values {
            RezValues::Histogram(v) => {
                assert_eq!(v[0].as_ref().unwrap().as_slice(), &[0, 1, 1, 0, 0, 0]);
                assert!(v[1].is_none());
            }
            _ => panic!("expected histogram"),
        }
    }
}

#[cfg(test)]
mod archive_tests {
    use super::*;
    use metriken::Window;
    use std::collections::HashMap;

    fn counter_col(name: &str, vals: Vec<Option<u64>>, wins: Vec<Option<Window>>) -> RezColumn {
        RezColumn {
            name: name.to_string(),
            metadata: [
                ("metric".to_string(), name.to_string()),
                ("metric_type".to_string(), "counter".to_string()),
            ]
            .into_iter()
            .collect::<HashMap<_, _>>(),
            values: RezValues::Counter(vals),
            windows: wins,
        }
    }

    #[test]
    fn archive_round_trips_multiple_tables() {
        let a = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: vec![1_000, 2_000],
            columns: vec![counter_col(
                "0",
                vec![Some(1), Some(2)],
                vec![
                    Some(Window::new(500, 1_000)),
                    Some(Window::new(1_400, 2_000)),
                ],
            )],
        };
        let b = RezTable {
            sampler: "blockio_latency".to_string(),
            timestamps: vec![1_000],
            columns: vec![counter_col("9", vec![Some(7)], vec![None])],
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("rec.rez");
        let metadata: BTreeMap<String, String> =
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect();

        write_archive(&out, &[a.clone(), b.clone()], metadata.clone()).unwrap();
        let archive = read_archive(&out).unwrap();

        assert_eq!(archive.manifest.version, REZ_SCHEMA_VERSION);
        assert_eq!(archive.manifest.metadata, metadata);
        assert_eq!(archive.manifest.tables.len(), 2);
        assert_eq!(archive.tables.len(), 2);

        let cpu = archive
            .tables
            .iter()
            .find(|t| t.sampler == "cpu_usage")
            .unwrap();
        assert_eq!(cpu.timestamps, vec![1_000, 2_000]);
        assert_eq!(
            cpu.columns[0].windows,
            vec![
                Some(Window::new(500, 1_000)),
                Some(Window::new(1_400, 2_000))
            ]
        );
        let bio = archive
            .tables
            .iter()
            .find(|t| t.sampler == "blockio_latency")
            .unwrap();
        assert_eq!(bio.timestamps, vec![1_000]);
        assert_eq!(bio.columns[0].windows, vec![None]);

        let cpu_idx = archive
            .manifest
            .tables
            .iter()
            .find(|t| t.sampler == "cpu_usage")
            .unwrap();
        assert_eq!(cpu_idx.file, "cpu_usage.parquet");
        assert_eq!(cpu_idx.rows, 2);
        assert_eq!(cpu_idx.cadence_ns, Some(1_000));
    }
}
