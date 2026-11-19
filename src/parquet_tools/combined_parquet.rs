//! Combined parquet files, which 5.x `recording combine` wrote without
//! `--ab` and the 5.x recorder wrote for several endpoints, converted by
//! `recording upgrade` into one source per input file (a column carrying
//! an exporter's own `source` forms a source of that name; see [`parts`]).
//!
//! A combined file holds its inputs' columns side by side, each labelled
//! with its input's `source` and its `node` (a rezolus input) or `instance`
//! (any other), and each input's metadata under
//! `per_source_metadata.<source>.<node|instance>`. Each input becomes a
//! source labelled `source` and `node` or `instance`, holding the
//! timestamp and duration columns and that input's columns, with that
//! input's metadata at the top level. The first rezolus input's source
//! is written first, and the others follow in the file's column order.
//! The readers then keep a KPI template to its own input's series, as
//! they do for any archive of several recordings; converted as one
//! source, a template that selects by metric name would read every
//! input's series of that name.
//!
//! An input that was itself a combined file is split only in part: its
//! rezolus columns keep their `node` (and the outer `instance` 5.x combine
//! gave them, as a series label) and form a source per node, and its other
//! columns, which 5.x combine gave the outer `instance`, are grouped as
//! [`parts`] groups columns that share a `source` and `instance`. A column
//! whose `source` is an exporter's own label forms a source of that name;
//! when all of an input's columns carry one (an exporter that labels every
//! series, or a 5.x recorder before 5.21, which labelled an endpoint's
//! columns with the endpoint's name), the input's `per_source_metadata` is
//! not carried, and the summary that [`convert`] returns names the entry.

use std::collections::BTreeMap;
use std::path::Path;

use crate::parquet_metadata::KEY_SELECTION;
use ::dashboard::keys::{
    KEY_DESCRIPTIONS, KEY_PER_SOURCE_METADATA, KEY_SERVICE_QUERIES, KEY_SOURCE, KEY_SYSTEMINFO,
    KEY_VERSION,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

/// The source 5.x labelled by `node`; every other is labelled by `instance`.
const REZOLUS: &str = "rezolus";

/// The labels 5.x combine gave a rezolus input's columns and any other
/// input's; the top-level keys a 5.x single-source file carried them as.
const KEY_NODE: &str = "node";
const KEY_INSTANCE: &str = "instance";

/// The keys written under `per_source_metadata.<source>` itself, beside
/// the entries combine added: the 5.x recorder's
/// (`build_per_source_metadata`), and `version`, which combine builds
/// before 5.10 wrote there with no entries. Each part of that source takes
/// them, below its entry's own keys.
const GROUP_KEYS: &[&str] = &[
    "first_sample_ns",
    "last_sample_ns",
    "role",
    "sampler_status",
    "version",
];

/// The key/value entry the parquet writer stores the Arrow schema under,
/// which the writer of each part writes for itself.
const ARROW_SCHEMA_KEY: &str = "ARROW:schema";

/// Top-level keys that are not copied to every part: [`part_metadata`]
/// writes each from the input's `per_source_metadata` entry or by its own
/// rule, and writes none of them when neither supplies one.
const PER_INPUT_KEYS: &[&str] = &[
    KEY_SOURCE,
    KEY_PER_SOURCE_METADATA,
    KEY_VERSION,
    KEY_NODE,
    KEY_INSTANCE,
    KEY_DESCRIPTIONS,
    KEY_SERVICE_QUERIES,
    KEY_SYSTEMINFO,
    KEY_SELECTION,
    ARROW_SCHEMA_KEY,
];

/// Whether the parquet file at `path` is a combined file: its `source` is a
/// JSON array of source names and every metric column carries a `source`
/// label. A file the 5.x recorder (5.21 and later) wrote for one endpoint
/// given an array `source` has unlabelled columns and is converted as one
/// source.
pub(crate) fn is_combined(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(builder) = ParquetRecordBatchReaderBuilder::try_new(file) else {
        return false;
    };
    let array_source = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .and_then(|kv| kv.iter().find(|e| e.key == KEY_SOURCE))
        .and_then(|e| e.value.as_deref())
        .is_some_and(|v| serde_json::from_str::<Vec<String>>(v).is_ok());
    let mut metrics = builder
        .schema()
        .fields()
        .iter()
        .filter(|f| !matches!(f.name().as_str(), "timestamp" | "duration"))
        .peekable();
    array_source
        && metrics.peek().is_some()
        && metrics.all(|f| f.metadata().get(KEY_SOURCE).is_some_and(|s| !s.is_empty()))
}

/// One input of a combined file: its source, its `node` or `instance`
/// label, and the positions of its columns.
struct Part {
    source: String,
    entry: Option<(&'static str, String)>,
    columns: Vec<usize>,
}

impl Part {
    /// The key of this input's entry under its source in
    /// `per_source_metadata`: its `node` or `instance`, else the value 5.x
    /// used for an input without one.
    fn entry_key(&self) -> &str {
        match &self.entry {
            Some((_, value)) => value,
            None if self.source == REZOLUS => "unknown",
            None => "0",
        }
    }
}

/// The inputs of a combined file with `schema`, in the order of their
/// first column. A column belongs to the input its `source` and `node` (for
/// rezolus) or `instance` labels name. A column whose `source` is an
/// exporter's own label, which 5.x combine kept in place of its input's,
/// forms an input of that name: nothing in the file says which input it
/// came from, and a single-file service input has `instance` 0 unless the
/// file set one, so joining it to a neighbour could put two services'
/// series in one recording.
///
/// Columns that share a `source` and `node` or `instance` can still come
/// from several inputs: two exporters' columns with one foreign `source`,
/// or the inputs of a re-combined file, which 5.x combine gave one
/// `instance`. 5.x combine wrote each input's columns together, and one
/// input's columns have distinct names after the last `::` in each
/// (combine's `<node|instance>::` prefixes come before it). When the latest
/// input with those labels already holds a column with the same name after
/// its last `::` and the same field metadata (other than `source` and the
/// `node` or `instance`), the column starts another input with the same
/// labels, rather than replacing that series. Columns that only one of
/// those inputs had join the latest. The name is part of the comparison
/// because a 5.x recorder from 5.10 to 5.20 overwrote an exporter's own
/// `source` and `endpoint` labels, so several columns of one input can
/// have equal field metadata.
fn parts(schema: &arrow::datatypes::Schema) -> Vec<Part> {
    let mut parts: Vec<Part> = Vec::new();
    // Each part's columns, as `parts` compares them: the name without its
    // prefixes, and the field metadata other than the part's labels.
    let mut series: Vec<Vec<(&str, BTreeMap<&str, &str>)>> = Vec::new();
    for (i, field) in schema.fields().iter().enumerate() {
        if matches!(field.name().as_str(), "timestamp" | "duration") {
            continue;
        }
        let meta = field.metadata();
        let source = meta.get(KEY_SOURCE).cloned().unwrap_or_default();
        // A non-rezolus column without an `instance` whose name starts
        // with `<node>::` is a rezolus input's that 5.x combine labelled by
        // node while it kept the input's foreign `source` (a recorder
        // before 5.21 labelled agents with the endpoint's name).
        let key = if source == REZOLUS
            || (!meta.contains_key(KEY_INSTANCE)
                && meta
                    .get(KEY_NODE)
                    .is_some_and(|n| field.name().starts_with(&format!("{n}::"))))
        {
            KEY_NODE
        } else {
            KEY_INSTANCE
        };
        let entry = meta.get(key).map(|v| (key, v.clone()));
        let bare = field.name().rsplit("::").next().unwrap_or_default();
        let identity = (
            bare,
            meta.iter()
                .filter(|(k, _)| *k != KEY_SOURCE && *k != key)
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect::<BTreeMap<&str, &str>>(),
        );
        let latest = parts
            .iter()
            .rposition(|p| p.source == source && p.entry == entry)
            .filter(|&p| !series[p].contains(&identity));
        match latest {
            Some(p) => {
                parts[p].columns.push(i);
                series[p].push(identity);
            }
            None => {
                parts.push(Part {
                    source,
                    entry,
                    columns: vec![i],
                });
                series.push(vec![identity]);
            }
        }
    }
    parts
}

/// The metadata of `part`'s source:
///
/// - the file's top-level keys outside [`PER_INPUT_KEYS`], `events` among
///   them, so every recording carries the file's events;
/// - `source`, the part's;
/// - its source group's [`GROUP_KEYS`], then its `per_source_metadata`
///   entry's keys;
/// - its source group's `descriptions`, else the file's;
/// - a top-level `service_queries` whose `service_name` is its source, or,
///   on the primary part, one that names no part's source or has no
///   `service_name`;
/// - on the primary part (the first rezolus part, else the first), the
///   file's `systeminfo` when its entry carries none, and the `selection`.
fn part_metadata(
    top: &BTreeMap<String, String>,
    parts: &[Part],
    part: &Part,
    primary: bool,
) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = top
        .iter()
        .filter(|(k, _)| !PER_INPUT_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    out.insert(KEY_SOURCE.to_string(), part.source.clone());

    let as_text = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let per_source = top
        .get(KEY_PER_SOURCE_METADATA)
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok());
    let group = per_source.as_ref().and_then(|p| p.get(&part.source));
    for key in GROUP_KEYS {
        if let Some(v) = group.and_then(|g| g.get(*key)) {
            out.insert(key.to_string(), as_text(v));
        }
    }
    if let Some(entry) = group
        .and_then(|g| g.get(part.entry_key()))
        .and_then(|e| e.as_object())
    {
        for (k, v) in entry {
            out.insert(k.clone(), as_text(v));
        }
    }
    match group.and_then(|g| g.get(KEY_DESCRIPTIONS)) {
        Some(descriptions) => {
            out.insert(KEY_DESCRIPTIONS.to_string(), as_text(descriptions));
        }
        None => {
            if let Some(descriptions) = top.get(KEY_DESCRIPTIONS) {
                out.insert(KEY_DESCRIPTIONS.to_string(), descriptions.clone());
            }
        }
    }
    if let Some(json) = top.get(KEY_SERVICE_QUERIES) {
        let named = serde_json::from_str::<serde_json::Value>(json)
            .ok()
            .and_then(|ext| ext.get("service_name")?.as_str().map(str::to_string));
        let mine = match &named {
            Some(name) if *name == part.source => true,
            Some(name) => primary && !parts.iter().any(|p| p.source == *name),
            None => primary,
        };
        if mine {
            out.entry(KEY_SERVICE_QUERIES.to_string())
                .or_insert_with(|| json.clone());
        }
    }

    if primary {
        if let Some(systeminfo) = top.get(KEY_SYSTEMINFO) {
            out.entry(KEY_SYSTEMINFO.to_string())
                .or_insert_with(|| systeminfo.clone());
        }
        if let Some(selection) = top.get(KEY_SELECTION) {
            out.insert(KEY_SELECTION.to_string(), selection.clone());
        }
    }
    out
}

/// Convert the combined parquet file at `input` into a new dendro archive
/// at `output`, one source per input file. Returns what was written.
pub(crate) fn convert(input: &Path, output: &Path) -> Result<String, Box<dyn std::error::Error>> {
    use arrow::array::RecordBatchReader;
    use arrow::datatypes::{Field, Schema};
    use parquet::arrow::{ArrowWriter, ProjectionMask};
    use parquet::file::metadata::KeyValue;
    use parquet::file::properties::WriterProperties;
    use std::sync::Arc;

    let open = || -> Result<_, Box<dyn std::error::Error>> {
        Ok(ParquetRecordBatchReaderBuilder::try_new(
            std::fs::File::open(input)?,
        )?)
    };
    let builder = open()?;
    let schema = builder.schema().clone();
    let top: BTreeMap<String, String> = builder
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .map(|kv| {
            kv.iter()
                .map(|e| (e.key.clone(), e.value.clone().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    drop(builder);
    let sources: Vec<String> = top
        .get(KEY_SOURCE)
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or_default();
    let mut parts = parts(&schema);
    if parts.is_empty() {
        return Err(format!(
            "{}: the combined file holds no metric columns",
            input.display()
        )
        .into());
    }
    let shared: Vec<usize> = ["timestamp", "duration"]
        .iter()
        .filter_map(|name| schema.index_of(name).ok())
        .collect();
    // The primary part first: a combined view shows its first recording's
    // metadata, and the primary's is what the file showed at its top level.
    let primary = parts.iter().position(|p| p.source == REZOLUS).unwrap_or(0);
    let first = parts.remove(primary);
    parts.insert(0, first);

    let dir = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut out = dendro::archive::ArchiveMut::create(output)?;
    let (mut rows, mut segments) = (0u64, 0u64);
    let mut names = Vec::with_capacity(parts.len());
    for (index, part) in parts.iter().enumerate() {
        // The part as a parquet file of its own, written beside the output
        // and streamed through.
        let staged = tempfile::Builder::new()
            .prefix(".combined-part.")
            .suffix(".parquet")
            .tempfile_in(dir)?;
        let builder = open()?;
        let mut columns: Vec<usize> = shared.iter().chain(&part.columns).copied().collect();
        columns.sort_unstable();
        let mask = ProjectionMask::roots(builder.parquet_schema(), columns);
        let reader = builder.with_projection(mask).build()?;
        let projected = reader.schema();
        let fields: Vec<Field> = projected
            .fields()
            .iter()
            .map(|f| {
                let mut meta = f.metadata().clone();
                // The labels the recording carries, which every column of
                // the part has with the recording's values (`parts`) and
                // would otherwise repeat on its series. A service column's
                // own `node` is kept.
                meta.retain(|k, _| {
                    k != KEY_SOURCE && part.entry.as_ref().is_none_or(|(key, _)| k != key)
                });
                f.as_ref().clone().with_metadata(meta)
            })
            .collect();
        let unlabelled = Arc::new(Schema::new_with_metadata(
            fields,
            projected.metadata().clone(),
        ));
        let kv = part_metadata(&top, &parts, part, index == 0)
            .into_iter()
            .map(|(k, v)| KeyValue::new(k, v))
            .collect();
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(kv))
            .build();
        let mut writer =
            ArrowWriter::try_new(staged.reopen()?, Arc::clone(&unlabelled), Some(props))?;
        for batch in reader {
            let batch = batch?;
            writer.write(&arrow::record_batch::RecordBatch::try_new(
                Arc::clone(&unlabelled),
                batch.columns().to_vec(),
            )?)?;
        }
        writer.close()?;

        let mut converter = metriken_storage::to_dendro::ParquetToArchive::new()
            .label(KEY_SOURCE, part.source.clone());
        let mut name = format!("{KEY_SOURCE}={}", part.source);
        if let Some((key, value)) = &part.entry {
            converter = converter.label(*key, value.clone());
            name.push_str(&format!(" {key}={value}"));
        }
        let written = converter
            .convert_into(staged.reopen()?, &mut out)
            .map_err(|e| format!("{}: {name}: {e}", input.display()))?;
        rows += written.rows;
        segments += written.segments as u64;
        names.push(name);
    }
    drop(out);
    // What the file describes that no part carries, so its metadata is
    // not used: each `per_source_metadata` entry no part's `source` and
    // `node` or `instance` names (as `<source>/<entry>`), and each source of
    // the file's `source` with neither a part nor an entry. Its columns
    // carry an exporter's own `source` (see `parts`), or it had none.
    let per_source = top
        .get(KEY_PER_SOURCE_METADATA)
        .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok());
    let mut missing: Vec<String> = Vec::new();
    for name in &sources {
        let entries: Vec<&String> = per_source
            .as_ref()
            .and_then(|p| p.get(name))
            .and_then(|g| g.as_object())
            .map(|g| {
                g.iter()
                    .filter(|(k, v)| {
                        v.is_object() && *k != KEY_DESCRIPTIONS && !GROUP_KEYS.contains(&k.as_str())
                    })
                    .map(|(k, _)| k)
                    .collect()
            })
            .unwrap_or_default();
        if entries.is_empty() {
            if !parts.iter().any(|p| p.source == *name) {
                missing.push(name.clone());
            }
            continue;
        }
        for entry in entries {
            if !parts
                .iter()
                .any(|p| p.source == *name && p.entry_key() == entry.as_str())
            {
                missing.push(format!("{name}/{entry}"));
            }
        }
    }
    let mut summary = format!(
        "{} source(s) ({}), {segments} segment(s), {rows} row(s)",
        parts.len(),
        names.join(", ")
    );
    if !missing.is_empty() {
        summary.push_str(&format!(
            "; no recording matches {}, so that metadata is not carried",
            missing.join(" or ")
        ));
    }
    Ok(summary)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::path::{Path, PathBuf};

    const BASE: u64 = 1_700_000_000_000_000_000;
    const ROWS: usize = 5;

    /// Field metadata from `pairs`.
    fn meta(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// A parquet file at `path` of [`ROWS`] rows at 1 s steps from
    /// [`BASE`]: the timestamp and duration columns, then one Int64 gauge
    /// column per `(name, metadata, value)`, and `kv` as its key/value
    /// metadata.
    fn write_parquet(
        path: &Path,
        gauges: &[(&str, HashMap<String, String>, i64)],
        kv: &[(&str, String)],
    ) {
        use arrow::array::{ArrayRef, Int64Array, UInt64Array};
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use parquet::file::metadata::KeyValue;
        use parquet::file::properties::WriterProperties;
        use std::sync::Arc;

        let mut fields = vec![
            Field::new("timestamp", DataType::UInt64, false).with_metadata(meta(&[
                ("metric_type", "timestamp"),
                ("unit", "nanoseconds"),
            ])),
            Field::new("duration", DataType::UInt64, true).with_metadata(meta(&[
                ("metric_type", "duration"),
                ("unit", "nanoseconds"),
            ])),
        ];
        let mut columns: Vec<ArrayRef> = vec![
            Arc::new(UInt64Array::from(
                (0..ROWS as u64)
                    .map(|i| BASE + i * 1_000_000_000)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(vec![1_000_000_000u64; ROWS])),
        ];
        for (name, metadata, value) in gauges {
            fields.push(Field::new(*name, DataType::Int64, true).with_metadata(metadata.clone()));
            columns.push(Arc::new(Int64Array::from(vec![*value; ROWS])));
        }
        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new(schema.clone(), columns).unwrap();
        let kv = kv
            .iter()
            .map(|(k, v)| KeyValue::new(k.to_string(), v.clone()))
            .collect();
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(kv))
            .build();
        let mut writer =
            ArrowWriter::try_new(std::fs::File::create(path).unwrap(), schema, Some(props))
                .unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
    }

    /// A combined parquet in the shape 5.x `recording combine` and the 5.x
    /// recorder wrote: one
    /// `<node|instance>::<metric>` column per input column, labelled with
    /// its input's `source` and its `node` (rezolus) or `instance` (any
    /// other source, `0` unless the input carried one), on the inputs'
    /// common timestamps; `source` the JSON array of the inputs' sources,
    /// and each input's metadata under
    /// `per_source_metadata.<source>.<node|instance>`, beside the keys the
    /// recorder wrote for the source.
    ///
    /// Its inputs, in column order, are two vllm services, `vllm-decode`
    /// and `vllm-prefill`, both instance 0, so both columns are named
    /// `0::vllm:num_requests_running`; and rezolus agents on `web01` and
    /// `web02`. web02's columns start with `memory_total`, which web01 does
    /// not have. The rezolus group carries the recorder's `sampler_status`
    /// object, which is not an entry. The
    /// services' `vllm:num_requests_running` reads 10 and 1. Each service
    /// carries a KPI template without `source=` whose check passes only on
    /// its own service's series: `above 10.5` for decode and `above 5` for
    /// prefill, which their sum, 11, violates. No 5.x writer put a template
    /// in a `per_source_metadata` entry; the entries carry them here so the
    /// check reads the split, and [`part_metadata`]'s tests cover the
    /// top-level template 5.x `annotate` wrote. A top-level template names a
    /// service no input is, and has no check.
    ///
    /// Decode's columns also hold `vllm:gpu_cache_usage` from two GPUs, told
    /// apart by the exporter's own `node` label (`gpu-a` and `gpu-b`), and
    /// `disk_bytes`, whose exporter labelled it `source="disk"`, which 5.x
    /// combine kept.
    ///
    /// [`part_metadata`]: super::part_metadata
    pub(crate) fn combined_parquet(dir: &Path) -> PathBuf {
        let vllm = |source: &str| {
            meta(&[
                ("metric_type", "gauge"),
                ("metric", "vllm:num_requests_running"),
                ("source", source),
                ("instance", "0"),
            ])
        };
        let decode = |metric: &str, extra: &[(&str, &str)]| {
            let mut m = meta(&[
                ("metric_type", "gauge"),
                ("metric", metric),
                ("source", "vllm-decode"),
                ("instance", "0"),
            ]);
            for (k, v) in extra {
                m.insert(k.to_string(), v.to_string());
            }
            m
        };
        let gauges = [
            ("0::vllm:num_requests_running", vllm("vllm-decode"), 10),
            (
                "0::vllm:gpu_cache_usage",
                decode("vllm:gpu_cache_usage", &[("node", "gpu-a")]),
                1,
            ),
            (
                "0::vllm:gpu_cache_usage",
                decode("vllm:gpu_cache_usage", &[("node", "gpu-b")]),
                2,
            ),
            (
                "0::disk_bytes",
                decode("disk_bytes", &[("source", "disk")]),
                7,
            ),
            ("0::vllm:num_requests_running", vllm("vllm-prefill"), 1),
            (
                "web01::cpu_cores",
                meta(&[
                    ("metric_type", "gauge"),
                    ("metric", "cpu_cores"),
                    ("source", "rezolus"),
                    ("node", "web01"),
                ]),
                8,
            ),
            (
                "web02::memory_total",
                meta(&[
                    ("metric_type", "gauge"),
                    ("metric", "memory_total"),
                    ("source", "rezolus"),
                    ("node", "web02"),
                ]),
                64,
            ),
            (
                "web02::cpu_cores",
                meta(&[
                    ("metric_type", "gauge"),
                    ("metric", "cpu_cores"),
                    ("source", "rezolus"),
                    ("node", "web02"),
                ]),
                16,
            ),
        ];
        let kpi = |service: &str, above: f64| {
            serde_json::json!({
                "service_name": service,
                "kpis": [{
                    "role": "throughput",
                    "title": "Requests running",
                    "query": "sum(vllm:num_requests_running)",
                    "type": "gauge",
                    "check": {"above": above},
                }],
            })
        };
        let per_source = serde_json::json!({
            "rezolus": {
                "sampler_status": {"cpu": "ok"},
                "web01": {"version": "5.28.0", "node": "web01", "systeminfo": {"hostname": "web01", "cores": 8}},
                "web02": {"version": "5.28.0", "node": "web02"},
            },
            "vllm-decode": {
                "role": "service",
                "first_sample_ns": BASE,
                "0": {"version": "0.9.0", "instance": "0", "service_queries": kpi("vllm-decode", 10.5)},
            },
            "vllm-prefill": {
                "role": "service",
                "0": {"version": "0.9.1", "instance": "0", "service_queries": kpi("vllm-prefill", 5.0)},
                "descriptions": {"vllm:num_requests_running": "prefill requests"},
            },
        });
        let custom = serde_json::json!({
            "service_name": "custom",
            "kpis": [{"role": "load", "title": "Cores", "query": "cpu_cores", "type": "gauge"}],
        });
        let events = serde_json::json!({"events": [
            {"timestamp": BASE + 1_000_000_000, "description": "decode restart", "source": "vllm-decode", "instance": "0"},
            {"timestamp": BASE + 2_000_000_000, "description": "run note"},
        ]});
        let path = dir.join("combined.parquet");
        write_parquet(
            &path,
            &gauges,
            &[
                (
                    "source",
                    r#"["rezolus","vllm-decode","vllm-prefill"]"#.to_string(),
                ),
                ("sampling_interval_ms", "1000".to_string()),
                ("systeminfo", r#"{"hostname":"web01"}"#.to_string()),
                (
                    "descriptions",
                    r#"{"cpu_cores":"online cores"}"#.to_string(),
                ),
                ("per_source_metadata", per_source.to_string()),
                ("service_queries", custom.to_string()),
                ("events", events.to_string()),
            ],
        );
        path
    }

    /// Each service's KPI template, which selects by metric name alone,
    /// reads its own service's series in the upgraded archive: `recording
    /// check` passes both, where reading the services' sum fails both.
    #[test]
    fn an_upgraded_combined_parquet_keeps_each_services_kpis_to_its_series() {
        let dir = tempfile::tempdir().unwrap();
        let input = combined_parquet(dir.path());
        let output = dir.path().join("combined.dendro");
        super::super::legacy::convert(&input, &output).unwrap();

        let run = super::super::check::run_checks(
            &output,
            &crate::parquet_tools::select::RecordingSelector::default(),
            None,
            &::dashboard::TemplateRegistry::empty(),
            "rezolus",
        )
        .unwrap();
        let lines = serde_json::to_string_pretty(&run.results).unwrap();
        assert_eq!(run.results.len(), 2, "{lines}");
        let s = run.summary();
        assert_eq!((s.pass, s.fail), (2, 0), "{lines}");
    }

    /// Each input is a recording labelled with its `source` and its `node`
    /// or `instance`, the rezolus input first and the others in the file's
    /// column order. A recording holds its own series: an exporter's own
    /// `node` label keeps two series apart, a column carrying an exporter's
    /// own `source` is a recording of that name, and no series repeats its
    /// recording's labels. Its metadata is its source group's recorder keys
    /// and its entry's keys, its source's `descriptions` (else the file's),
    /// the file's events, and on the first rezolus input the file's
    /// `systeminfo` and the top-level template that names no input. The
    /// viewer opens the five as one combined view.
    #[test]
    fn an_upgraded_combined_parquet_is_one_recording_per_input() {
        use metriken_query::MetricsSource;

        let dir = tempfile::tempdir().unwrap();
        let input = combined_parquet(dir.path());
        let output = dir.path().join("combined.dendro");
        let summary = super::super::legacy::convert(&input, &output).unwrap();
        assert!(
            summary.starts_with(
                "5 source(s) (source=rezolus node=web01, source=vllm-decode instance=0, \
                 source=disk instance=0, source=vllm-prefill instance=0, \
                 source=rezolus node=web02), "
            ),
            "{summary}"
        );

        let pool = metriken_storage::BufferPool::new(64 * 1024 * 1024);
        let recordings = ::rez::reader::RezReader::open_recordings(&output, pool).unwrap();
        let labels: Vec<Vec<(String, String)>> = recordings
            .iter()
            .map(|(l, _)| l.clone().into_iter().collect())
            .collect();
        let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
        assert_eq!(
            labels,
            [
                vec![pair("node", "web01"), pair("source", "rezolus")],
                vec![pair("instance", "0"), pair("source", "vllm-decode")],
                vec![pair("instance", "0"), pair("source", "disk")],
                vec![pair("instance", "0"), pair("source", "vllm-prefill")],
                vec![pair("node", "web02"), pair("source", "rezolus")],
            ]
        );
        assert!(!summary.contains("no recording"), "{summary}");
        assert!(crate::viewer::opens_combined(&recordings, false));

        let mut names: Vec<Vec<String>> = recordings.iter().map(|(_, r)| r.gauge_names()).collect();
        names.iter_mut().for_each(|n| n.sort());
        assert_eq!(
            names,
            [
                vec!["cpu_cores".to_string()],
                vec![
                    "vllm:gpu_cache_usage".to_string(),
                    "vllm:num_requests_running".to_string(),
                ],
                vec!["disk_bytes".to_string()],
                vec!["vllm:num_requests_running".to_string()],
                vec!["cpu_cores".to_string(), "memory_total".to_string()],
            ]
        );
        let series = |i: usize, name: &str| recordings[i].1.gauge_labels(name);
        let mut gpus: Vec<String> = series(1, "vllm:gpu_cache_usage")
            .iter()
            .map(|l| l.get("node").cloned().unwrap_or_default())
            .collect();
        gpus.sort();
        assert_eq!(gpus, ["gpu-a", "gpu-b"]);
        for (i, (_, reader)) in recordings.iter().enumerate() {
            for name in reader.gauge_names() {
                for l in reader.gauge_labels(&name) {
                    assert!(
                        l.keys().all(|k| !k.starts_with("exported_")),
                        "recording {i} {name}: {l:?}"
                    );
                }
            }
        }

        let meta: Vec<_> = recordings.iter().map(|(_, r)| r.file_metadata()).collect();
        let get = |i: usize, k: &str| meta[i].get(k).cloned();
        assert_eq!(get(0, "source").as_deref(), Some("rezolus"));
        assert_eq!(get(0, "version").as_deref(), Some("5.28.0"));
        assert_eq!(
            get(0, "systeminfo").as_deref(),
            Some(r#"{"cores":8,"hostname":"web01"}"#),
            "the entry's systeminfo, not the file's"
        );
        assert_eq!(get(1, "source").as_deref(), Some("vllm-decode"));
        assert_eq!(get(1, "version").as_deref(), Some("0.9.0"));
        assert_eq!(get(1, "role").as_deref(), Some("service"));
        assert_eq!(get(1, "first_sample_ns"), Some(BASE.to_string()));
        assert!(get(1, "systeminfo").is_none());
        assert_eq!(get(2, "source").as_deref(), Some("disk"));
        assert!(get(2, "version").is_none());
        assert_eq!(get(3, "source").as_deref(), Some("vllm-prefill"));
        assert_eq!(get(3, "version").as_deref(), Some("0.9.1"));
        assert!(get(3, "first_sample_ns").is_none());
        assert!(get(3, "descriptions").unwrap().contains("prefill requests"));
        assert!(get(1, "descriptions").unwrap().contains("online cores"));
        assert!(get(0, "descriptions").unwrap().contains("online cores"));
        for (i, service) in [(0, "custom"), (1, "vllm-decode"), (3, "vllm-prefill")] {
            let ext: ::dashboard::ServiceExtension =
                serde_json::from_str(&get(i, "service_queries").unwrap()).unwrap();
            assert_eq!(ext.service_name, service);
        }
        assert!(get(4, "systeminfo").is_none());
        for i in 0..5 {
            assert_eq!(get(i, "sampling_interval_ms").as_deref(), Some("1000"));
            assert!(get(i, "per_source_metadata").is_none());
            let events: serde_json::Value =
                serde_json::from_str(&get(i, "events").unwrap()).unwrap();
            let described: Vec<&str> = events["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["description"].as_str().unwrap())
                .collect();
            assert_eq!(described, ["decode restart", "run note"], "recording {i}");
        }
    }

    /// An input none of whose columns carries its own `source` (its
    /// exporter labelled every series) is not a recording of that name:
    /// its columns form one of the exporter's name, and the summary says
    /// the input's metadata is not carried.
    #[test]
    fn an_input_whose_columns_all_carry_another_source_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("combined.parquet");
        let column = |metric: &str, source: &str| {
            meta(&[
                ("metric_type", "gauge"),
                ("metric", metric),
                ("source", source),
                ("instance", "0"),
            ])
        };
        write_parquet(
            &input,
            &[
                ("0::requests", column("requests", "vllm"), 1),
                ("0::queue", column("queue", "exp"), 2),
            ],
            &[
                ("source", r#"["sglang","vllm"]"#.to_string()),
                ("sampling_interval_ms", "1000".to_string()),
            ],
        );
        let output = dir.path().join("combined.dendro");
        let summary = super::super::legacy::convert(&input, &output).unwrap();
        assert!(
            summary.starts_with("2 source(s) (source=vllm instance=0, source=exp instance=0)"),
            "{summary}"
        );
        assert!(
            summary.ends_with("; no recording matches sglang, so that metadata is not carried"),
            "{summary}"
        );
    }

    /// Two inputs whose columns all carry one exporter `source` and
    /// `instance` 0 (5.x recorders before 5.21 labelled a Prometheus
    /// endpoint's columns with the endpoint's name) are two recordings with
    /// those labels, each with its own series, not one recording holding
    /// one of the two. A series only the second input has stays with it.
    #[test]
    fn two_inputs_with_one_foreign_source_keep_their_series() {
        use metriken_query::MetricsSource;

        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("combined.parquet");
        let column = |metric: &str| {
            meta(&[
                ("metric_type", "gauge"),
                ("metric", metric),
                ("source", "localhost-8000"),
                ("instance", "0"),
            ])
        };
        write_parquet(
            &input,
            &[
                ("0::running", column("running"), 7),
                ("0::running", column("running"), 3),
                ("0::waiting", column("waiting"), 1),
            ],
            &[
                ("source", r#"["vllm-decode","vllm-prefill"]"#.to_string()),
                ("sampling_interval_ms", "1000".to_string()),
            ],
        );
        let output = dir.path().join("combined.dendro");
        let summary = super::super::legacy::convert(&input, &output).unwrap();
        assert!(
            summary.starts_with(
                "2 source(s) (source=localhost-8000 instance=0, \
                 source=localhost-8000 instance=0)"
            ),
            "{summary}"
        );
        assert!(
            summary.ends_with(
                "; no recording matches vllm-decode or vllm-prefill, so that metadata is not carried"
            ),
            "{summary}"
        );

        let pool = metriken_storage::BufferPool::new(64 * 1024 * 1024);
        let recordings = ::rez::reader::RezReader::open_recordings(&output, pool).unwrap();
        let running: Vec<f64> = recordings
            .iter()
            .map(|(_, r)| {
                let (start, end) = r.time_range().unwrap();
                match r.query_range("running", start, end, 1.0).unwrap() {
                    metriken_query::QueryResult::Matrix { result, .. } => {
                        assert_eq!(result.len(), 1);
                        result[0].values[0].1
                    }
                    other => panic!("{other:?}"),
                }
            })
            .collect();
        assert_eq!(running, [7.0, 3.0]);
        let mut names: Vec<Vec<String>> = recordings.iter().map(|(_, r)| r.gauge_names()).collect();
        names.iter_mut().for_each(|n| n.sort());
        assert_eq!(names, [vec!["running"], vec!["running", "waiting"]]);
    }

    /// A 5.x recorder from 5.10 to 5.20 overwrote an exporter's `source` and
    /// `endpoint` labels, so one input can hold several columns with equal
    /// field metadata and distinct names; they stay one recording.
    #[test]
    fn one_inputs_columns_with_equal_metadata_are_one_recording() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("combined.parquet");
        let column = || {
            meta(&[
                ("metric_type", "counter"),
                ("metric", "vllm_prompt_tokens_by_source_total"),
                ("source", "vllm"),
                ("instance", "0"),
            ])
        };
        write_parquet(
            &input,
            &[
                ("0::46", column(), 1),
                ("0::47", column(), 2),
                ("0::48", column(), 3),
            ],
            &[
                ("source", r#"["vllm"]"#.to_string()),
                ("sampling_interval_ms", "1000".to_string()),
            ],
        );
        let output = dir.path().join("combined.dendro");
        let summary = super::super::legacy::convert(&input, &output).unwrap();
        assert!(
            summary.starts_with("1 source(s) (source=vllm instance=0)"),
            "{summary}"
        );
    }

    /// A 5.x recorder before 5.21 labelled two rezolus agents' columns with
    /// the endpoint's name and `node`, and no `instance`: each agent is a
    /// recording of its own, and the summary names the `rezolus` entries
    /// and the source no column carries.
    #[test]
    fn agents_labelled_with_an_endpoint_name_are_split_by_node() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("combined.parquet");
        let column = |source: &str, node: &str| {
            meta(&[
                ("metric_type", "gauge"),
                ("metric", "cpu_cores"),
                ("source", source),
                ("node", node),
            ])
        };
        let per_source = serde_json::json!({
            "rezolus": {
                "decode": {"version": "5.11.0", "node": "decode"},
                "prefill": {"version": "5.11.0", "node": "prefill"},
                "router": {"version": "5.11.0", "node": "router"},
            },
        });
        write_parquet(
            &input,
            &[
                ("decode::cpu_cores", column("0.0.0.0-4241", "decode"), 16),
                ("prefill::cpu_cores", column("0.0.0.0-4241", "prefill"), 16),
                ("router::cpu_cores", column("rezolus", "router"), 4),
            ],
            &[
                ("source", r#"["rezolus","sglang"]"#.to_string()),
                ("sampling_interval_ms", "1000".to_string()),
                ("per_source_metadata", per_source.to_string()),
            ],
        );
        let output = dir.path().join("combined.dendro");
        let summary = super::super::legacy::convert(&input, &output).unwrap();
        assert!(
            summary.starts_with(
                "3 source(s) (source=rezolus node=router, source=0.0.0.0-4241 node=decode, \
                 source=0.0.0.0-4241 node=prefill)"
            ),
            "{summary}"
        );
        assert!(
            summary.ends_with(
                "; no recording matches rezolus/decode or rezolus/prefill or sglang, \
                 so that metadata is not carried"
            ),
            "{summary}"
        );
    }

    /// Combine before 5.10 labelled each column with its input's `source`
    /// alone, with no `node`, `instance` or prefix. A service column whose
    /// exporter labelled it with a `node` stays in its service's recording,
    /// also when its name starts with the `node` value (`gpu_util`, node
    /// `gpu`) without combine's `::`.
    #[test]
    fn a_service_columns_own_node_label_does_not_split_its_service() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("combined.parquet");
        let column = |metric: &str, source: &str, node: Option<&str>| {
            let mut m = meta(&[
                ("metric_type", "gauge"),
                ("metric", metric),
                ("source", source),
            ]);
            if let Some(node) = node {
                m.insert("node".to_string(), node.to_string());
            }
            m
        };
        write_parquet(
            &input,
            &[
                ("cpu_cores", column("cpu_cores", "rezolus", None), 8),
                ("running", column("running", "vllm", None), 3),
                ("gpu_util", column("gpu_util", "vllm", Some("gpu")), 50),
                ("gpu_util", column("gpu_util", "vllm", Some("gpu-b")), 60),
            ],
            &[
                ("source", r#"["rezolus","vllm"]"#.to_string()),
                ("sampling_interval_ms", "1000".to_string()),
            ],
        );
        let output = dir.path().join("combined.dendro");
        let summary = super::super::legacy::convert(&input, &output).unwrap();
        assert!(
            summary.starts_with("2 source(s) (source=rezolus, source=vllm)"),
            "{summary}"
        );
    }

    /// A re-combined file's inputs, `0::0::m` and `0::1::m`, which 5.x
    /// combine gave one `instance`, are two recordings: the names differ
    /// only in combine's prefixes.
    #[test]
    fn a_re_combined_files_inputs_are_two_recordings() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("combined.parquet");
        let column = || {
            meta(&[
                ("metric_type", "gauge"),
                ("metric", "m"),
                ("source", "vllm"),
                ("instance", "0"),
            ])
        };
        write_parquet(
            &input,
            &[("0::0::m", column(), 1), ("0::1::m", column(), 2)],
            &[
                ("source", r#"["vllm"]"#.to_string()),
                ("sampling_interval_ms", "1000".to_string()),
            ],
        );
        let output = dir.path().join("combined.dendro");
        let summary = super::super::legacy::convert(&input, &output).unwrap();
        assert!(
            summary.starts_with("2 source(s) (source=vllm instance=0, source=vllm instance=0)"),
            "{summary}"
        );
    }

    /// The parts of a combined file: (source, entry value).
    fn part(source: &str, entry: Option<(&'static str, &str)>) -> super::Part {
        super::Part {
            source: source.to_string(),
            entry: entry.map(|(k, v)| (k, v.to_string())),
            columns: Vec::new(),
        }
    }

    /// The source group's recorder keys go to its part, below its entry's
    /// keys; the `selection` and `systeminfo` go to the primary only; a
    /// top-level template goes to the part it names, else (naming no part,
    /// or naming none) to the primary.
    #[test]
    fn part_metadata_places_group_keys_selection_and_templates() {
        use super::part_metadata;

        let per_source = serde_json::json!({
            "vllm": {
                "role": "service",
                "first_sample_ns": 1,
                "last_sample_ns": 2,
                "sampler_status": {"vllm": "ok"},
                "0": {"version": "0.9.0", "role": "loadgen"},
            },
            "llm": {"version": "0.15.1"},
        });
        let top_with = |template: Option<serde_json::Value>| -> BTreeMap<String, String> {
            let mut top: BTreeMap<String, String> = [
                ("source", r#"["rezolus","vllm"]"#.to_string()),
                ("per_source_metadata", per_source.to_string()),
                ("selection", r#"{"start":1}"#.to_string()),
                ("systeminfo", r#"{"hostname":"h"}"#.to_string()),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
            if let Some(t) = template {
                top.insert("service_queries".to_string(), t.to_string());
            }
            top
        };
        let parts = [
            part("rezolus", Some(("node", "web01"))),
            part("vllm", Some(("instance", "0"))),
            part("llm", None),
        ];

        let top = top_with(None);
        let primary = part_metadata(&top, &parts, &parts[0], true);
        let vllm = part_metadata(&top, &parts, &parts[1], false);
        assert_eq!(
            vllm["role"], "loadgen",
            "the entry's key wins over the group's"
        );
        assert_eq!(vllm["first_sample_ns"], "1");
        assert_eq!(vllm["last_sample_ns"], "2");
        assert_eq!(vllm["sampler_status"], r#"{"vllm":"ok"}"#);
        assert_eq!(vllm["version"], "0.9.0");
        let llm = part_metadata(&top, &parts, &parts[2], false);
        assert_eq!(llm["version"], "0.15.1", "a group with no entries");
        assert_eq!(primary["selection"], r#"{"start":1}"#);
        assert_eq!(primary["systeminfo"], r#"{"hostname":"h"}"#);
        assert!(!vllm.contains_key("selection"));
        assert!(!vllm.contains_key("systeminfo"));
        assert!(!primary.contains_key("role"));

        let holder = |template: serde_json::Value| -> Vec<bool> {
            let top = top_with(Some(template));
            [(0, true), (1, false), (2, false)]
                .iter()
                .map(|&(i, p)| {
                    part_metadata(&top, &parts, &parts[i], p).contains_key("service_queries")
                })
                .collect()
        };
        assert_eq!(
            holder(serde_json::json!({"service_name": "vllm", "kpis": []})),
            [false, true, false]
        );
        assert_eq!(
            holder(serde_json::json!({"service_name": "other", "kpis": []})),
            [true, false, false]
        );
        assert_eq!(
            holder(serde_json::json!({"kpis": []})),
            [true, false, false]
        );
    }

    /// A file is combined only when its `source` is an array, it has metric
    /// columns, and every one carries a non-empty `source` label.
    #[test]
    fn a_file_is_combined_only_when_every_column_names_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let array = r#"["vllm","sglang"]"#.to_string();
        let gauge = |source: Option<&str>| {
            let mut m = meta(&[("metric_type", "gauge"), ("metric", "requests")]);
            if let Some(s) = source {
                m.insert("source".to_string(), s.to_string());
            }
            m
        };
        let combined =
            |name: &str, gauges: &[(&str, HashMap<String, String>, i64)], source: &str| {
                let path = dir.path().join(name);
                write_parquet(&path, gauges, &[("source", source.to_string())]);
                super::is_combined(&path)
            };
        assert!(combined(
            "both.parquet",
            &[
                ("0::requests", gauge(Some("vllm")), 1),
                ("1::requests", gauge(Some("sglang")), 1)
            ],
            &array
        ));
        assert!(!combined(
            "one.parquet",
            &[
                ("0::requests", gauge(Some("vllm")), 1),
                ("requests", gauge(None), 1)
            ],
            &array
        ));
        assert!(!combined(
            "empty.parquet",
            &[("0::requests", gauge(Some("")), 1)],
            &array
        ));
        assert!(!combined("none.parquet", &[], &array));
        assert!(!combined(
            "single.parquet",
            &[("0::requests", gauge(Some("vllm")), 1)],
            "vllm"
        ));
    }

    /// A file the 5.x recorder wrote for one endpoint given an array
    /// `source` has no `source` label on its columns, so it is not a
    /// combined file and converts to one source, its `source` kept.
    #[test]
    fn an_array_source_without_column_labels_is_one_source() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("recorded.parquet");
        let source = r#"["vllm","sglang"]"#.to_string();
        write_parquet(
            &input,
            &[(
                "requests",
                meta(&[("metric_type", "gauge"), ("metric", "requests")]),
                3,
            )],
            &[
                ("source", source.clone()),
                ("sampling_interval_ms", "1000".to_string()),
            ],
        );
        assert!(!super::is_combined(&input));
        let output = dir.path().join("recorded.dendro");
        let summary = super::super::legacy::convert(&input, &output).unwrap();
        assert!(summary.starts_with("1 source(s)"), "{summary}");

        let pool = metriken_storage::BufferPool::new(64 * 1024 * 1024);
        let recordings = ::rez::reader::RezReader::open_recordings(&output, pool).unwrap();
        assert_eq!(recordings.len(), 1);
        use metriken_query::MetricsSource;
        assert_eq!(recordings[0].1.file_metadata().get("source"), Some(&source));
    }
}
