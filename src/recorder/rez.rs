//! The `.rez` per-sampler archive: an uncompressed tar of `manifest.json` plus
//! one `<sampler>.parquet` table per sampler. See the Stage-3 plan header for
//! the format decisions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// `.rez` manifest schema version.
pub const REZ_SCHEMA_VERSION: u32 = 1;
/// Manifest filename inside the tar.
pub const REZ_MANIFEST_NAME: &str = "manifest.json";

/// Top-level `.rez` manifest (`manifest.json`): a bag of label-tagged recordings.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RezManifest {
    pub version: u32,
    pub recordings: Vec<RezRecording>,
}

/// One recording = one endpoint on one host = a label set + its per-sampler tables.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RezRecording {
    /// Filesystem-safe directory holding this recording's parquet tables in the tar.
    pub dir: String,
    /// Arbitrary label set: `source`, `host` (from systeminfo), user `--label k=v`.
    pub labels: BTreeMap<String, String>,
    /// Per-recording metadata: the existing `parquet_metadata` keys
    /// (`systeminfo`, `descriptions`, `sampling_interval_ms`, ...).
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

/// One recording's data to serialize (borrowed tables).
pub struct RecordingData<'a> {
    pub dir: String,
    pub labels: BTreeMap<String, String>,
    pub metadata: BTreeMap<String, String>,
    pub tables: &'a [RezTable],
}

/// A decoded `.rez` archive (round-trip / test surface).
pub struct RezArchive {
    pub manifest: RezManifest,
    /// Decoded tables, one inner `Vec` per `manifest.recordings` entry (parallel order).
    pub tables: Vec<Vec<RezTable>>,
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

/// Write `recordings` to an uncompressed `.rez` tar at `path`. Each recording's
/// tables are nested under `<dir>/<sampler>.parquet`.
pub fn write_archive(path: &Path, recordings: &[RecordingData]) -> Result<(), RezError> {
    let mut manifest_recordings = Vec::with_capacity(recordings.len());
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for rec in recordings {
        let mut index = Vec::with_capacity(rec.tables.len());
        for table in rec.tables {
            let file = format!("{}.parquet", table.sampler);
            let bytes = write_table_parquet(table)?;
            index.push(RezTableIndex {
                sampler: table.sampler.clone(),
                file: file.clone(),
                columns: table.columns.iter().map(|c| c.name.clone()).collect(),
                rows: table.timestamps.len() as u64,
                cadence_ns: cadence_hint(&table.timestamps),
            });
            files.push((format!("{}/{}", rec.dir, file), bytes));
        }
        manifest_recordings.push(RezRecording {
            dir: rec.dir.clone(),
            labels: rec.labels.clone(),
            metadata: rec.metadata.clone(),
            tables: index,
        });
    }
    let manifest = RezManifest {
        version: REZ_SCHEMA_VERSION,
        recordings: manifest_recordings,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;

    let out = std::fs::File::create(path)?;
    let mut builder = tar::Builder::new(out);
    builder.mode(tar::HeaderMode::Deterministic);
    append_tar_entry(&mut builder, REZ_MANIFEST_NAME, &manifest_bytes)?;
    for (name, bytes) in &files {
        append_tar_entry(&mut builder, name, bytes)?;
    }
    builder.into_inner()?.sync_all()?;
    Ok(())
}

/// Read a `.rez` archive back into its manifest + decoded tables (per recording).
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

    let mut all = Vec::with_capacity(manifest.recordings.len());
    for rec in &manifest.recordings {
        let mut tables = Vec::with_capacity(rec.tables.len());
        for idx in &rec.tables {
            let path_in_tar = format!("{}/{}", rec.dir, idx.file);
            let bytes = parquet_bytes
                .remove(&path_in_tar)
                .ok_or_else(|| format!("missing table file {path_in_tar}"))?;
            tables.push(read_table_parquet(idx.sampler.clone(), bytes)?);
        }
        all.push(tables);
    }
    Ok(RezArchive {
        manifest,
        tables: all,
    })
}

use metriken_exposition::{Counter, Gauge, Histogram, Snapshot};

/// A borrowed snapshot entry, tagged by shape.
enum Entry<'a> {
    Counter(&'a Counter),
    Gauge(&'a Gauge),
    Histogram(&'a Histogram),
}

impl Entry<'_> {
    fn name(&self) -> &str {
        match self {
            Entry::Counter(c) => &c.name,
            Entry::Gauge(g) => &g.name,
            Entry::Histogram(h) => &h.name,
        }
    }
    fn metadata(&self) -> &HashMap<String, String> {
        match self {
            Entry::Counter(c) => &c.metadata,
            Entry::Gauge(g) => &g.metadata,
            Entry::Histogram(h) => &h.metadata,
        }
    }
    fn window(&self) -> Option<Window> {
        match self {
            Entry::Counter(c) => c.window,
            Entry::Gauge(g) => g.window,
            Entry::Histogram(h) => h.window,
        }
    }
    /// The `metric_type` string the parquet reader keys on to reconstruct the
    /// column's value shape (counter vs gauge; histograms carry a `:buckets`
    /// suffix, so their `metric_type` is informational).
    fn metric_type(&self) -> &'static str {
        match self {
            Entry::Counter(_) => "counter",
            Entry::Gauge(_) => "gauge",
            Entry::Histogram(_) => "histogram",
        }
    }
}

/// A growing per-sampler table. Columns are sparse: shorter than the row count
/// until padded (a metric absent in some rows gets `None` there).
struct TableBuilder {
    sampler: String,
    timestamps: Vec<u64>,
    order: Vec<String>,
    columns: HashMap<String, RezColumn>,
    last_key: Option<u64>,
}

impl TableBuilder {
    fn new(sampler: String) -> Self {
        Self {
            sampler,
            timestamps: Vec::new(),
            order: Vec::new(),
            columns: HashMap::new(),
            last_key: None,
        }
    }

    fn col_len(col: &RezColumn) -> usize {
        match &col.values {
            RezValues::Counter(v) => v.len(),
            RezValues::Gauge(v) => v.len(),
            RezValues::Histogram(v) => v.len(),
        }
    }

    fn pad(col: &mut RezColumn, to: usize) {
        while Self::col_len(col) < to {
            match &mut col.values {
                RezValues::Counter(v) => v.push(None),
                RezValues::Gauge(v) => v.push(None),
                RezValues::Histogram(v) => v.push(None),
            }
            col.windows.push(None);
        }
    }

    fn push_row(&mut self, snapshot_ts: u64, entries: &[Entry<'_>]) {
        let row = self.timestamps.len();
        self.timestamps.push(snapshot_ts);
        for e in entries {
            let name = e.name().to_string();
            let order = &mut self.order;
            let col = self.columns.entry(name.clone()).or_insert_with(|| {
                order.push(name.clone());
                let values = match e {
                    Entry::Counter(_) => RezValues::Counter(Vec::new()),
                    Entry::Gauge(_) => RezValues::Gauge(Vec::new()),
                    Entry::Histogram(_) => RezValues::Histogram(Vec::new()),
                };
                let mut metadata = e.metadata().clone();
                metadata
                    .entry("metric_type".to_string())
                    .or_insert_with(|| e.metric_type().to_string());
                RezColumn {
                    name,
                    metadata,
                    values,
                    windows: Vec::new(),
                }
            });
            Self::pad(col, row);
            match (e, &mut col.values) {
                (Entry::Counter(c), RezValues::Counter(v)) => v.push(Some(c.value)),
                (Entry::Gauge(g), RezValues::Gauge(v)) => v.push(Some(g.value)),
                (Entry::Histogram(h), RezValues::Histogram(v)) => v.push(Some(h.value.clone())),
                _ => {}
            }
            col.windows.push(e.window());
        }
    }

    fn finish(mut self) -> RezTable {
        let rows = self.timestamps.len();
        let columns = self
            .order
            .iter()
            .map(|name| {
                let mut col = self.columns.remove(name).unwrap();
                Self::pad(&mut col, rows);
                col
            })
            .collect();
        RezTable {
            sampler: self.sampler,
            timestamps: self.timestamps,
            columns,
        }
    }
}

/// Accumulates scraped snapshots into per-sampler tables, deduping by each
/// sampler's representative acquisition window.
pub struct RezRecorder {
    tables: BTreeMap<String, TableBuilder>,
    metadata: BTreeMap<String, String>,
    labels: BTreeMap<String, String>,
    dir: String,
}

impl RezRecorder {
    pub fn new(
        metadata: BTreeMap<String, String>,
        labels: BTreeMap<String, String>,
        dir: String,
    ) -> Self {
        Self {
            tables: BTreeMap::new(),
            metadata,
            labels,
            dir,
        }
    }

    /// Partition `snapshot`'s metrics by their `sampler` label; for each sampler
    /// append a row iff its representative window (max `end_ns` among windowed
    /// metrics) advanced, else key on `snapshot_ts` (windowless → per-poll row).
    pub fn ingest(&mut self, snapshot: &Snapshot, snapshot_ts: u64) {
        let (counters, gauges, histograms) = match snapshot {
            Snapshot::V1(s) => (&s.counters, &s.gauges, &s.histograms),
            Snapshot::V2(s) => (&s.counters, &s.gauges, &s.histograms),
        };

        let mut groups: BTreeMap<&str, Vec<Entry<'_>>> = BTreeMap::new();
        for c in counters {
            let s = c
                .metadata
                .get("sampler")
                .map(String::as_str)
                .unwrap_or("unattributed");
            groups.entry(s).or_default().push(Entry::Counter(c));
        }
        for g in gauges {
            let s = g
                .metadata
                .get("sampler")
                .map(String::as_str)
                .unwrap_or("unattributed");
            groups.entry(s).or_default().push(Entry::Gauge(g));
        }
        for h in histograms {
            let s = h
                .metadata
                .get("sampler")
                .map(String::as_str)
                .unwrap_or("unattributed");
            groups.entry(s).or_default().push(Entry::Histogram(h));
        }

        for (sampler, entries) in groups {
            let rep_end = entries
                .iter()
                .filter_map(|e| e.window())
                .map(|w| w.end_ns)
                .max();
            let key = rep_end.unwrap_or(snapshot_ts);
            let table = self
                .tables
                .entry(sampler.to_string())
                .or_insert_with(|| TableBuilder::new(sampler.to_string()));
            if let Some(last) = table.last_key {
                if key <= last {
                    continue; // window unchanged → same observation → skip
                }
            }
            table.last_key = Some(key);
            table.push_row(snapshot_ts, &entries);
        }
    }

    /// Test/inspection helper: the current (unpadded) table builder view.
    #[cfg(test)]
    pub fn table(&self, sampler: &str) -> Option<&TableBuilder> {
        self.tables.get(sampler)
    }

    /// Consume into finalized per-sampler tables.
    pub fn finalize_tables(self) -> Vec<RezTable> {
        self.tables
            .into_values()
            .map(TableBuilder::finish)
            .collect()
    }

    /// Finalize and write the single-recording `.rez` archive at `path`.
    pub fn finalize(self, path: &Path) -> Result<(), RezError> {
        let dir = self.dir.clone();
        let labels = self.labels.clone();
        let metadata = self.metadata.clone();
        let tables = self.finalize_tables();
        write_archive(
            path,
            &[RecordingData {
                dir,
                labels,
                metadata,
                tables: &tables,
            }],
        )
    }
}

/// Extract the `hostname` string from a systeminfo JSON blob, if present.
pub fn host_from_systeminfo(systeminfo_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(systeminfo_json)
        .ok()?
        .get("hostname")?
        .as_str()
        .map(|s| s.to_string())
}

/// The recording's label set: `source`, `host` (from systeminfo hostname, when
/// available), then user `--label k=v` applied last (last-wins, so a user
/// `--label host=...` overrides the auto value).
pub fn build_labels(
    source: &str,
    systeminfo_json: Option<&str>,
    user_labels: &[(String, String)],
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert("source".to_string(), source.to_string());
    if let Some(host) = systeminfo_json.and_then(host_from_systeminfo) {
        labels.insert("host".to_string(), host);
    }
    for (k, v) in user_labels {
        labels.insert(k.clone(), v.clone());
    }
    labels
}

/// Filesystem-safe directory name for a single recording, derived from its
/// `source` label (falls back to `"recording"`). The manifest — not the dir —
/// is authoritative for labels; this is only a human-readable tar path.
pub fn recording_dir_slug(labels: &BTreeMap<String, String>) -> String {
    let base = labels
        .get("source")
        .map(String::as_str)
        .unwrap_or("recording");
    let slug: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if slug.is_empty() {
        "recording".to_string()
    } else {
        slug
    }
}

/// True when the recording should be written as a `.rez` archive: either the
/// output path ends in `.rez` or `--format rez` was given.
pub fn wants_rez(output: &Path, format: crate::Format) -> bool {
    format == crate::Format::Rez || output.extension().and_then(|e| e.to_str()) == Some("rez")
}

#[cfg(test)]
pub(crate) mod recorder_tests_support {
    pub use super::recorder_tests::{counter, snap};
}

#[cfg(test)]
mod recorder_tests {
    use super::*;
    use metriken::Window;
    use metriken_exposition::{Counter, Snapshot, SnapshotV2};
    use std::collections::HashMap;
    use std::time::SystemTime;

    fn cmeta(metric: &str, sampler: &str) -> HashMap<String, String> {
        [
            ("metric".to_string(), metric.to_string()),
            ("sampler".to_string(), sampler.to_string()),
        ]
        .into_iter()
        .collect()
    }

    pub fn snap(ts: u64, counters: Vec<Counter>) -> Snapshot {
        Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
            duration: std::time::Duration::ZERO,
            metadata: HashMap::new(),
            counters,
            gauges: Vec::new(),
            histograms: Vec::new(),
        })
    }

    pub fn counter(name: &str, sampler: &str, value: u64, window: Option<Window>) -> Counter {
        Counter {
            name: name.to_string(),
            value,
            metadata: cmeta(name, sampler),
            window,
        }
    }

    #[test]
    fn windowless_sampler_writes_one_row_per_poll() {
        let mut r = RezRecorder::new(BTreeMap::new(), BTreeMap::new(), "test".to_string());
        for i in 0..3u64 {
            let ts = 1_000 + i;
            r.ingest(&snap(ts, vec![counter("0", "cpu_perf", i, None)]), ts);
        }
        let t = r.table("cpu_perf").unwrap();
        assert_eq!(t.timestamps.len(), 3);
    }

    #[test]
    fn unchanged_window_dedups_to_one_row() {
        let mut r = RezRecorder::new(BTreeMap::new(), BTreeMap::new(), "test".to_string());
        let w = Window::new(900, 1_000);
        for i in 0..3u64 {
            r.ingest(
                &snap(1_000 + i, vec![counter("0", "drivehealth", 5, Some(w))]),
                1_000 + i,
            );
        }
        assert_eq!(r.table("drivehealth").unwrap().timestamps.len(), 1);
    }

    #[test]
    fn advancing_window_writes_one_row_per_advance() {
        let mut r = RezRecorder::new(BTreeMap::new(), BTreeMap::new(), "test".to_string());
        for i in 0..3u64 {
            let end = 1_000 + i * 100;
            r.ingest(
                &snap(
                    2_000 + i,
                    vec![counter(
                        "0",
                        "cpu_usage",
                        i,
                        Some(Window::new(end - 50, end)),
                    )],
                ),
                2_000 + i,
            );
        }
        assert_eq!(r.table("cpu_usage").unwrap().timestamps.len(), 3);
    }

    #[test]
    fn mixed_sampler_advances_on_windowed_and_carries_packed_column() {
        let mut r = RezRecorder::new(BTreeMap::new(), BTreeMap::new(), "test".to_string());
        // metric "0" windowed (advances), metric "1" packed/windowless, same sampler.
        for i in 0..2u64 {
            let end = 1_000 + i * 100;
            r.ingest(
                &snap(
                    3_000 + i,
                    vec![
                        counter("0", "cpu_usage", i, Some(Window::new(end - 50, end))),
                        counter("1", "cpu_usage", 42 + i, None),
                    ],
                ),
                3_000 + i,
            );
        }
        let t = r.table("cpu_usage").unwrap();
        assert_eq!(t.timestamps.len(), 2);
        let packed = t
            .columns
            .values()
            .find(|c| c.name == "1")
            .expect("packed column present");
        match &packed.values {
            RezValues::Counter(v) => assert_eq!(v, &vec![Some(42), Some(43)]),
            _ => panic!("expected counter"),
        }
    }

    #[test]
    fn two_samplers_split_into_two_tables() {
        let mut r = RezRecorder::new(BTreeMap::new(), BTreeMap::new(), "test".to_string());
        r.ingest(
            &snap(
                1_000,
                vec![
                    counter("0", "cpu_usage", 1, Some(Window::new(900, 1_000))),
                    counter("9", "blockio_latency", 2, Some(Window::new(900, 1_000))),
                ],
            ),
            1_000,
        );
        let tables = r.finalize_tables();
        assert_eq!(tables.len(), 2);
        assert!(tables.iter().any(|t| t.sampler == "cpu_usage"));
        assert!(tables.iter().any(|t| t.sampler == "blockio_latency"));
    }
}

#[cfg(test)]
mod manifest_tests {
    use super::*;

    #[test]
    fn manifest_json_round_trips() {
        let m = RezManifest {
            version: REZ_SCHEMA_VERSION,
            recordings: vec![RezRecording {
                dir: "rezolus".to_string(),
                labels: [
                    ("source".to_string(), "rezolus".to_string()),
                    ("host".to_string(), "node0".to_string()),
                ]
                .into_iter()
                .collect(),
                metadata: [("sampling_interval_ms".to_string(), "1000".to_string())]
                    .into_iter()
                    .collect(),
                tables: vec![RezTableIndex {
                    sampler: "cpu_usage".to_string(),
                    file: "cpu_usage.parquet".to_string(),
                    columns: vec!["5".to_string()],
                    rows: 3,
                    cadence_ns: Some(1_000_000_000),
                }],
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
        let labels: BTreeMap<String, String> = [
            ("source".to_string(), "rezolus".to_string()),
            ("host".to_string(), "node0".to_string()),
        ]
        .into_iter()
        .collect();
        let metadata: BTreeMap<String, String> =
            [("sampling_interval_ms".to_string(), "1000".to_string())]
                .into_iter()
                .collect();

        let tables = [a.clone(), b.clone()];
        write_archive(
            &out,
            &[RecordingData {
                dir: "rezolus".to_string(),
                labels: labels.clone(),
                metadata: metadata.clone(),
                tables: &tables,
            }],
        )
        .unwrap();
        let archive = read_archive(&out).unwrap();

        assert_eq!(archive.manifest.version, REZ_SCHEMA_VERSION);
        assert_eq!(archive.manifest.recordings.len(), 1);
        let rec = &archive.manifest.recordings[0];
        assert_eq!(rec.dir, "rezolus");
        assert_eq!(rec.labels, labels);
        assert_eq!(rec.metadata, metadata);
        assert_eq!(rec.tables.len(), 2);
        assert_eq!(archive.tables.len(), 1);
        assert_eq!(archive.tables[0].len(), 2);

        let cpu = archive.tables[0]
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
        let bio = archive.tables[0]
            .iter()
            .find(|t| t.sampler == "blockio_latency")
            .unwrap();
        assert_eq!(bio.timestamps, vec![1_000]);
        assert_eq!(bio.columns[0].windows, vec![None]);

        let cpu_idx = rec
            .tables
            .iter()
            .find(|t| t.sampler == "cpu_usage")
            .unwrap();
        assert_eq!(cpu_idx.file, "cpu_usage.parquet");
        assert_eq!(cpu_idx.rows, 2);
        assert_eq!(cpu_idx.cadence_ns, Some(1_000));
    }

    // Two recordings with distinct dirs must round-trip independently: the tar
    // nests each under its own <dir>/, and read_archive returns tables parallel
    // to manifest.recordings order. This is the multi-recording path Phase C
    // (`parquet combine`) will exercise; the writer/reader already support it.
    #[test]
    fn archive_round_trips_multiple_recordings() {
        let baseline = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: vec![1_000, 2_000],
            columns: vec![counter_col("0", vec![Some(1), Some(2)], vec![None, None])],
        };
        let experiment = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: vec![1_000, 2_000],
            columns: vec![counter_col("0", vec![Some(10), Some(20)], vec![None, None])],
        };
        let labels = |arm: &str| -> BTreeMap<String, String> {
            [
                ("source".to_string(), "rezolus".to_string()),
                ("arm".to_string(), arm.to_string()),
            ]
            .into_iter()
            .collect()
        };
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("ab.rez");
        let base_tables = [baseline];
        let exp_tables = [experiment];

        write_archive(
            &out,
            &[
                RecordingData {
                    dir: "redis".to_string(),
                    labels: labels("redis"),
                    metadata: BTreeMap::new(),
                    tables: &base_tables,
                },
                RecordingData {
                    dir: "valkey".to_string(),
                    labels: labels("valkey"),
                    metadata: BTreeMap::new(),
                    tables: &exp_tables,
                },
            ],
        )
        .unwrap();
        let archive = read_archive(&out).unwrap();

        // Two recordings, distinct dirs, tables parallel to recordings order.
        assert_eq!(archive.manifest.recordings.len(), 2);
        assert_eq!(archive.tables.len(), 2);
        assert_eq!(archive.manifest.recordings[0].dir, "redis");
        assert_eq!(archive.manifest.recordings[1].dir, "valkey");
        assert_eq!(
            archive.manifest.recordings[0]
                .labels
                .get("arm")
                .map(String::as_str),
            Some("redis")
        );
        assert_eq!(
            archive.manifest.recordings[1]
                .labels
                .get("arm")
                .map(String::as_str),
            Some("valkey")
        );
        // Same sampler name in both recordings resolves to each recording's own
        // values (no cross-recording clobber despite the shared file basename).
        match &archive.tables[0][0].columns[0].values {
            RezValues::Counter(v) => assert_eq!(v, &vec![Some(1), Some(2)]),
            _ => panic!("expected counter"),
        }
        match &archive.tables[1][0].columns[0].values {
            RezValues::Counter(v) => assert_eq!(v, &vec![Some(10), Some(20)]),
            _ => panic!("expected counter"),
        }
    }
}

#[cfg(test)]
mod finalize_tests {
    use super::recorder_tests_support::*;
    use super::*;
    use metriken::Window;
    use metriken_exposition::{Gauge, Snapshot, SnapshotV2};
    use std::collections::HashMap;
    use std::time::SystemTime;

    #[test]
    fn recorder_finalize_writes_readable_archive() {
        let mut r = RezRecorder::new(
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            "rezolus".to_string(),
        );
        // drivehealth: same window over 3 polls → 1 row.
        let w = Window::new(900, 1_000);
        for i in 0..3u64 {
            r.ingest(
                &snap(1_000 + i, vec![counter("0", "drivehealth", 5, Some(w))]),
                1_000 + i,
            );
        }
        // cpu_perf: windowless → 3 rows.
        for i in 0..3u64 {
            r.ingest(
                &snap(2_000 + i, vec![counter("1", "cpu_perf", i, None)]),
                2_000 + i,
            );
        }

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("rec.rez");
        r.finalize(&out).unwrap();

        let archive = read_archive(&out).unwrap();
        let rec = &archive.manifest.recordings[0];
        assert_eq!(
            rec.labels.get("source").map(String::as_str),
            Some("rezolus")
        );
        assert_eq!(
            rec.metadata.get("source").map(String::as_str),
            Some("rezolus")
        );
        let dh = archive.tables[0]
            .iter()
            .find(|t| t.sampler == "drivehealth")
            .unwrap();
        assert_eq!(dh.timestamps.len(), 1);
        assert_eq!(dh.columns[0].windows, vec![Some(Window::new(900, 1_000))]);
        let perf = archive.tables[0]
            .iter()
            .find(|t| t.sampler == "cpu_perf")
            .unwrap();
        assert_eq!(perf.timestamps.len(), 3);
        assert_eq!(perf.columns[0].windows, vec![None, None, None]);
    }

    // Added coverage: a GAUGE metric must round-trip through ingest→finalize→read
    // (the recorder sets metric_type="gauge", which the parquet reader keys on).
    #[test]
    fn recorder_round_trips_a_gauge_column() {
        let mut r = RezRecorder::new(
            BTreeMap::new(),
            [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            "rezolus".to_string(),
        );
        let w = Window::new(1_900, 2_000);
        let g = Gauge {
            name: "0".to_string(),
            value: -7,
            metadata: [
                ("metric".to_string(), "mem_free".to_string()),
                ("sampler".to_string(), "memory_meminfo".to_string()),
            ]
            .into_iter()
            .collect::<HashMap<_, _>>(),
            window: Some(w),
        };
        let s = Snapshot::V2(SnapshotV2 {
            systemtime: SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(2_000),
            duration: std::time::Duration::ZERO,
            metadata: HashMap::new(),
            counters: Vec::new(),
            gauges: vec![g],
            histograms: Vec::new(),
        });
        r.ingest(&s, 2_000);

        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("g.rez");
        r.finalize(&out).unwrap();
        let archive = read_archive(&out).unwrap();
        let t = archive.tables[0]
            .iter()
            .find(|t| t.sampler == "memory_meminfo")
            .unwrap();
        assert_eq!(t.columns.len(), 1);
        match &t.columns[0].values {
            RezValues::Gauge(v) => assert_eq!(v, &vec![Some(-7)]),
            other => panic!("expected gauge column, got {other:?}"),
        }
        assert_eq!(t.columns[0].windows, vec![Some(w)]);
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use crate::Format;
    use std::path::Path;

    #[test]
    fn extension_or_format_selects_rez() {
        assert!(wants_rez(Path::new("out.rez"), Format::Parquet));
        assert!(wants_rez(Path::new("out.parquet"), Format::Rez));
        assert!(!wants_rez(Path::new("out.parquet"), Format::Parquet));
        assert!(!wants_rez(Path::new("out.raw"), Format::Raw));
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;

    #[test]
    fn host_from_systeminfo_extracts_hostname() {
        let json = r#"{"hostname":"node7","cpus":64}"#;
        assert_eq!(host_from_systeminfo(json), Some("node7".to_string()));
    }

    #[test]
    fn host_from_systeminfo_missing_or_invalid_is_none() {
        assert_eq!(host_from_systeminfo(r#"{"cpus":64}"#), None);
        assert_eq!(host_from_systeminfo("not json"), None);
        assert_eq!(host_from_systeminfo(r#"{"hostname":null}"#), None);
    }

    #[test]
    fn build_labels_populates_source_and_host() {
        let labels = build_labels("rezolus", Some(r#"{"hostname":"node7"}"#), &[]);
        assert_eq!(labels.get("source").map(String::as_str), Some("rezolus"));
        assert_eq!(labels.get("host").map(String::as_str), Some("node7"));
    }

    #[test]
    fn build_labels_no_host_when_systeminfo_absent() {
        let labels = build_labels("rezolus", None, &[]);
        assert_eq!(labels.get("source").map(String::as_str), Some("rezolus"));
        assert!(!labels.contains_key("host"));
    }

    #[test]
    fn build_labels_user_labels_merge_and_override() {
        let user = vec![
            ("arm".to_string(), "redis".to_string()),
            ("host".to_string(), "friendly".to_string()), // user overrides auto host
        ];
        let labels = build_labels("rezolus", Some(r#"{"hostname":"node7"}"#), &user);
        assert_eq!(labels.get("arm").map(String::as_str), Some("redis"));
        assert_eq!(labels.get("host").map(String::as_str), Some("friendly"));
    }

    #[test]
    fn recording_dir_slug_sanitizes_source() {
        let labels: BTreeMap<String, String> = [("source".to_string(), "llm-perf".to_string())]
            .into_iter()
            .collect();
        assert_eq!(recording_dir_slug(&labels), "llm-perf");
    }

    #[test]
    fn recording_dir_slug_replaces_unsafe_chars_and_defaults() {
        let labels: BTreeMap<String, String> = [("source".to_string(), "a/b c".to_string())]
            .into_iter()
            .collect();
        assert_eq!(recording_dir_slug(&labels), "a-b-c");
        assert_eq!(recording_dir_slug(&BTreeMap::new()), "recording");
    }
}
