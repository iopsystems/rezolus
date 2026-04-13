use arrow::array::{Array, ArrayRef, UInt64Array};
use arrow::compute;
use arrow::datatypes::{Field, Schema, SchemaRef};
use arrow::record_batch::RecordBatch;
use clap::ArgMatches;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;
use parquet::file::reader::FileReader;
use parquet::file::serialized_reader::SerializedFileReader;
use parquet::format::KeyValue;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::parquet_metadata::*;

/// Parsed data from one input parquet file.
struct InputFile {
    path: PathBuf,
    schema: SchemaRef,
    kv_metadata: Vec<KeyValue>,
    batches: Vec<RecordBatch>,
    source: String,
    sampling_interval_ms: Option<String>,
}

pub(super) fn run(args: &ArgMatches) -> Result<(), Box<dyn std::error::Error>> {
    let files: Vec<PathBuf> = args
        .get_many::<PathBuf>("FILES")
        .unwrap()
        .cloned()
        .collect();
    let output = args.get_one::<PathBuf>("output").unwrap();

    // Phase 1: Load all input files
    let inputs = load_inputs(&files)?;

    // Phase 2: Validate (cheapest checks first)
    validate_sources(&inputs)?;
    validate_sampling_interval(&inputs)?;
    validate_no_column_conflicts(&inputs)?;
    validate_time_overlap(&inputs)?;

    // Phase 3: Combine and write
    combine_and_write(&inputs, output)?;

    let source_names: Vec<&str> = inputs.iter().map(|i| i.source.as_str()).collect();
    println!(
        "Combined {} files ({}) into {:?}",
        inputs.len(),
        source_names.join(", "),
        output
    );

    Ok(())
}

// ── Loading ─────────────────────────────────────────────────────────────

fn load_inputs(paths: &[PathBuf]) -> Result<Vec<InputFile>, Box<dyn std::error::Error>> {
    paths.iter().map(load_single_input).collect()
}

fn load_single_input(path: &PathBuf) -> Result<InputFile, Box<dyn std::error::Error>> {
    // Read file-level metadata via SerializedFileReader
    let meta_reader = SerializedFileReader::new(std::fs::File::open(path)?)?;
    let kv_metadata: Vec<KeyValue> = meta_reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .cloned()
        .unwrap_or_default();

    let source = kv_metadata
        .iter()
        .find(|kv| kv.key == KEY_SOURCE)
        .and_then(|kv| kv.value.clone())
        .ok_or_else(|| format!("{:?}: missing '{}' metadata", path, KEY_SOURCE))?;

    let sampling_interval_ms = kv_metadata
        .iter()
        .find(|kv| kv.key == KEY_SAMPLING_INTERVAL_MS)
        .and_then(|kv| kv.value.clone());

    // Read all record batches
    let file = std::fs::File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let schema = builder.schema().clone();
    let reader = builder.build()?;
    let batches: Vec<RecordBatch> = reader.collect::<Result<Vec<_>, _>>()?;

    Ok(InputFile {
        path: path.clone(),
        schema,
        kv_metadata,
        batches,
        source,
        sampling_interval_ms,
    })
}

// ── Validation ──────────────────────────────────────────────────────────

fn validate_sources(inputs: &[InputFile]) -> Result<(), Box<dyn std::error::Error>> {
    let rezolus_count = inputs.iter().filter(|i| i.source == "rezolus").count();
    if rezolus_count > 1 {
        return Err(format!(
            "found {} files with source=\"rezolus\"; at most one is allowed",
            rezolus_count
        )
        .into());
    }
    Ok(())
}

fn validate_sampling_interval(inputs: &[InputFile]) -> Result<(), Box<dyn std::error::Error>> {
    let intervals: Vec<(&str, &PathBuf)> = inputs
        .iter()
        .filter_map(|i| i.sampling_interval_ms.as_deref().map(|v| (v, &i.path)))
        .collect();

    if let Some((first_val, _)) = intervals.first() {
        for (val, path) in &intervals[1..] {
            if val != first_val {
                return Err(format!(
                    "sampling_interval_ms mismatch: \"{}\" vs \"{}\" in {:?}",
                    first_val, val, path
                )
                .into());
            }
        }
    }
    Ok(())
}

fn validate_no_column_conflicts(inputs: &[InputFile]) -> Result<(), Box<dyn std::error::Error>> {
    let shared_columns: HashSet<&str> = ["timestamp", "duration"].into_iter().collect();
    let mut seen: HashMap<&str, &PathBuf> = HashMap::new();

    for input in inputs {
        for field in input.schema.fields() {
            let name = field.name().as_str();
            if shared_columns.contains(name) {
                continue;
            }
            if let Some(prev_path) = seen.get(name) {
                return Err(format!(
                    "column {:?} appears in both {:?} and {:?}",
                    name, prev_path, input.path
                )
                .into());
            }
            seen.insert(name, &input.path);
        }
    }
    Ok(())
}

fn validate_time_overlap(inputs: &[InputFile]) -> Result<(), Box<dyn std::error::Error>> {
    let mut ranges: Vec<(u64, u64, &PathBuf)> = Vec::new();

    for input in inputs {
        let (min_ts, max_ts) = timestamp_range(input)?;
        ranges.push((min_ts, max_ts, &input.path));
    }

    let global_min = ranges.iter().map(|(lo, _, _)| *lo).max().unwrap();
    let global_max = ranges.iter().map(|(_, hi, _)| *hi).min().unwrap();

    if global_min > global_max {
        let range_strs: Vec<String> = ranges
            .iter()
            .map(|(lo, hi, path)| format!("  {:?}: {} - {}", path, lo, hi))
            .collect();
        return Err(format!(
            "timestamp ranges do not overlap:\n{}",
            range_strs.join("\n")
        )
        .into());
    }

    Ok(())
}

fn timestamp_range(input: &InputFile) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let ts_idx = input
        .schema
        .index_of("timestamp")
        .map_err(|_| format!("{:?}: missing 'timestamp' column", input.path))?;

    let mut min_ts = u64::MAX;
    let mut max_ts = u64::MIN;

    for batch in &input.batches {
        let ts_col = batch
            .column(ts_idx)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or_else(|| format!("{:?}: timestamp column is not UInt64", input.path))?;

        for i in 0..ts_col.len() {
            let v = ts_col.value(i);
            min_ts = min_ts.min(v);
            max_ts = max_ts.max(v);
        }
    }

    if min_ts == u64::MAX {
        return Err(format!("{:?}: file has no rows", input.path).into());
    }

    Ok((min_ts, max_ts))
}

// ── Combine ─────────────────────────────────────────────────────────────

fn combine_and_write(
    inputs: &[InputFile],
    output: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let interval_ns = resolve_interval_ns(inputs)?;

    // Step 1: Build quantized-timestamp → row-index maps
    let ts_maps: Vec<HashMap<u64, usize>> = inputs
        .iter()
        .map(|input| build_timestamp_map(input, interval_ns))
        .collect::<Result<Vec<_>, _>>()?;

    // Step 2: Compute intersection of all quantized timestamp sets (sorted)
    let common_timestamps = compute_common_timestamps(&ts_maps);
    if common_timestamps.is_empty() {
        return Err("no common timestamps across all input files".into());
    }

    // Step 2b: Validate alignment quality — at least 95% of matched
    // timestamps must have raw values within 10% of the interval.
    validate_alignment_quality(inputs, &common_timestamps, interval_ns)?;

    // Step 3: Build merged schema
    let (primary_idx, merged_schema) = build_merged_schema(inputs);

    // Step 4: Build output record batch with aligned rows
    let output_batch = build_output_batch(
        inputs,
        &ts_maps,
        &common_timestamps,
        primary_idx,
        &merged_schema,
    )?;

    // Step 5: Merge metadata and write
    let merged_kv = merge_metadata(inputs)?;
    write_parquet(output, &merged_schema, &output_batch, merged_kv)?;

    Ok(())
}

/// Parse the (already-validated-identical) sampling interval as nanoseconds.
fn resolve_interval_ns(inputs: &[InputFile]) -> Result<u64, Box<dyn std::error::Error>> {
    let ms_str = inputs
        .iter()
        .find_map(|i| i.sampling_interval_ms.as_deref())
        .ok_or("no sampling_interval_ms metadata in any input file")?;
    let ms: u64 = ms_str
        .parse()
        .map_err(|_| format!("sampling_interval_ms {:?} is not a valid integer", ms_str))?;
    Ok(ms * 1_000_000) // ms → ns
}

/// Round a nanosecond timestamp to the nearest interval boundary.
fn quantize(ts: u64, interval_ns: u64) -> u64 {
    let half = interval_ns / 2;
    ((ts + half) / interval_ns) * interval_ns
}

/// Validate that aligned timestamps are close enough across files.
///
/// For each quantized bucket in the common set, collect the raw timestamps
/// from every file that mapped to that bucket. At least 95% of these buckets
/// must have all raw timestamps within 10% of the interval of each other.
fn validate_alignment_quality(
    inputs: &[InputFile],
    common_quantized: &[u64],
    interval_ns: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let tolerance = interval_ns / 10; // 10% of interval
    let threshold = 0.95;

    // Build quantized → raw timestamp maps for each file
    let raw_maps: Vec<HashMap<u64, u64>> = inputs
        .iter()
        .map(|input| {
            let ts_idx = input.schema.index_of("timestamp").unwrap();
            let mut map = HashMap::new();
            for batch in &input.batches {
                let ts_col = batch
                    .column(ts_idx)
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .unwrap();
                for i in 0..ts_col.len() {
                    let raw = ts_col.value(i);
                    let q = quantize(raw, interval_ns);
                    // Keep the value closest to the bucket center
                    map.entry(q)
                        .and_modify(|existing: &mut u64| {
                            if raw.abs_diff(q) < existing.abs_diff(q) {
                                *existing = raw;
                            }
                        })
                        .or_insert(raw);
                }
            }
            map
        })
        .collect();

    let mut aligned = 0usize;
    for &qts in common_quantized {
        let raws: Vec<u64> = raw_maps
            .iter()
            .filter_map(|m| m.get(&qts).copied())
            .collect();
        if raws.len() < 2 {
            aligned += 1;
            continue;
        }
        let min_raw = *raws.iter().min().unwrap();
        let max_raw = *raws.iter().max().unwrap();
        if max_raw - min_raw <= tolerance {
            aligned += 1;
        }
    }

    let ratio = aligned as f64 / common_quantized.len() as f64;
    if ratio < threshold {
        return Err(format!(
            "timestamp alignment too poor: only {:.1}% of matched timestamps are within \
             10% of the sampling interval (need ≥95%)",
            ratio * 100.0
        )
        .into());
    }

    if ratio < 1.0 {
        let misaligned = common_quantized.len() - aligned;
        eprintln!(
            "warning: {misaligned}/{} timestamps have phase offset >10% of interval",
            common_quantized.len()
        );
    }

    Ok(())
}

fn build_timestamp_map(
    input: &InputFile,
    interval_ns: u64,
) -> Result<HashMap<u64, usize>, Box<dyn std::error::Error>> {
    let ts_idx = input
        .schema
        .index_of("timestamp")
        .map_err(|_| format!("{:?}: missing 'timestamp' column", input.path))?;

    let mut map: HashMap<u64, (usize, u64)> = HashMap::new(); // quantized → (row_idx, raw_ts)
    let mut global_row = 0usize;

    for batch in &input.batches {
        let ts_col = batch
            .column(ts_idx)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or("timestamp column is not UInt64")?;

        for i in 0..ts_col.len() {
            let raw = ts_col.value(i);
            let q = quantize(raw, interval_ns);
            let row = global_row + i;
            map.entry(q)
                .and_modify(|(existing_row, existing_raw)| {
                    // Keep the row whose raw timestamp is closest to the bucket center
                    if raw.abs_diff(q) < existing_raw.abs_diff(q) {
                        *existing_row = row;
                        *existing_raw = raw;
                    }
                })
                .or_insert((row, raw));
        }
        global_row += batch.num_rows();
    }

    Ok(map.into_iter().map(|(q, (row, _))| (q, row)).collect())
}

fn compute_common_timestamps(ts_maps: &[HashMap<u64, usize>]) -> Vec<u64> {
    if ts_maps.is_empty() {
        return Vec::new();
    }

    // Start from the smallest map for efficiency
    let (smallest_idx, _) = ts_maps
        .iter()
        .enumerate()
        .min_by_key(|(_, m)| m.len())
        .unwrap();

    let mut common: Vec<u64> = ts_maps[smallest_idx]
        .keys()
        .filter(|ts| ts_maps.iter().all(|m| m.contains_key(ts)))
        .copied()
        .collect();

    common.sort_unstable();
    common
}

fn build_merged_schema(inputs: &[InputFile]) -> (usize, SchemaRef) {
    // Prefer rezolus file as primary (for timestamp/duration), else first file
    let primary_idx = inputs
        .iter()
        .position(|i| i.source == "rezolus")
        .unwrap_or(0);

    let mut fields: Vec<Arc<Field>> = Vec::new();

    // timestamp and duration from primary
    let primary_schema = &inputs[primary_idx].schema;
    for field in primary_schema.fields() {
        let name = field.name().as_str();
        if name == "timestamp" || name == "duration" {
            fields.push(field.clone());
        }
    }

    // All metric columns from each input in order
    for input in inputs {
        for field in input.schema.fields() {
            let name = field.name().as_str();
            if name != "timestamp" && name != "duration" {
                fields.push(field.clone());
            }
        }
    }

    (primary_idx, Arc::new(Schema::new(fields)))
}

fn build_output_batch(
    inputs: &[InputFile],
    ts_maps: &[HashMap<u64, usize>],
    common_timestamps: &[u64],
    primary_idx: usize,
    merged_schema: &SchemaRef,
) -> Result<RecordBatch, Box<dyn std::error::Error>> {
    // For each input, compute selection indices (row numbers for common timestamps)
    let selection_indices: Vec<UInt64Array> = ts_maps
        .iter()
        .map(|ts_map| {
            UInt64Array::from(
                common_timestamps
                    .iter()
                    .map(|ts| ts_map[ts] as u64)
                    .collect::<Vec<u64>>(),
            )
        })
        .collect();

    // Concatenate batches per input into single contiguous arrays per column
    let concatenated: Vec<Vec<ArrayRef>> = inputs
        .iter()
        .map(concatenate_columns)
        .collect::<Result<Vec<_>, _>>()?;

    let mut output_columns: Vec<ArrayRef> = Vec::new();

    // Timestamp column: use quantized (bucket-center) values for a clean,
    // uniform time grid regardless of per-file phase offsets.
    output_columns.push(Arc::new(UInt64Array::from(common_timestamps.to_vec())));

    // Duration from primary file
    let dur_idx = inputs[primary_idx].schema.index_of("duration").unwrap();
    output_columns.push(compute::take(
        concatenated[primary_idx][dur_idx].as_ref(),
        &selection_indices[primary_idx],
        None,
    )?);

    // Metric columns from each input
    for (file_idx, input) in inputs.iter().enumerate() {
        for (col_idx, field) in input.schema.fields().iter().enumerate() {
            if field.name() == "timestamp" || field.name() == "duration" {
                continue;
            }
            output_columns.push(compute::take(
                concatenated[file_idx][col_idx].as_ref(),
                &selection_indices[file_idx],
                None,
            )?);
        }
    }

    let batch = RecordBatch::try_new(merged_schema.clone(), output_columns)?;
    Ok(batch)
}

fn concatenate_columns(input: &InputFile) -> Result<Vec<ArrayRef>, Box<dyn std::error::Error>> {
    if input.batches.is_empty() {
        return Err(format!("{:?}: file has no data", input.path).into());
    }

    if input.batches.len() == 1 {
        return Ok((0..input.batches[0].num_columns())
            .map(|i| input.batches[0].column(i).clone())
            .collect());
    }

    let num_cols = input.schema.fields().len();
    let mut result = Vec::with_capacity(num_cols);

    for col_idx in 0..num_cols {
        let arrays: Vec<&dyn Array> = input
            .batches
            .iter()
            .map(|b| b.column(col_idx).as_ref())
            .collect();
        result.push(compute::concat(&arrays)?);
    }

    Ok(result)
}

// ── Metadata merge ──────────────────────────────────────────────────────

fn merge_metadata(inputs: &[InputFile]) -> Result<Vec<KeyValue>, Box<dyn std::error::Error>> {
    let mut result: Vec<KeyValue> = Vec::new();

    // source: JSON array of all source names
    let sources: Vec<&str> = inputs.iter().map(|i| i.source.as_str()).collect();
    result.push(KeyValue {
        key: KEY_SOURCE.to_string(),
        value: Some(serde_json::to_string(&sources)?),
    });

    // sampling_interval_ms: take from first file that has it (already validated identical)
    if let Some(interval) = inputs.iter().find_map(|i| i.sampling_interval_ms.clone()) {
        result.push(KeyValue {
            key: KEY_SAMPLING_INTERVAL_MS.to_string(),
            value: Some(interval),
        });
    }

    // systeminfo: prefer rezolus file
    if let Some(val) = find_kv_value(inputs, KEY_SYSTEMINFO, Some("rezolus")) {
        result.push(KeyValue {
            key: KEY_SYSTEMINFO.to_string(),
            value: Some(val),
        });
    }

    // descriptions: union-merge all JSON maps
    let mut merged_descriptions: serde_json::Map<String, serde_json::Value> =
        serde_json::Map::new();
    for input in inputs {
        if let Some(desc_str) = input
            .kv_metadata
            .iter()
            .find(|kv| kv.key == KEY_DESCRIPTIONS)
            .and_then(|kv| kv.value.as_deref())
        {
            if let Ok(desc_map) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(desc_str)
            {
                for (k, v) in desc_map {
                    merged_descriptions.entry(k).or_insert(v);
                }
            }
        }
    }
    if !merged_descriptions.is_empty() {
        result.push(KeyValue {
            key: KEY_DESCRIPTIONS.to_string(),
            value: Some(serde_json::to_string(&merged_descriptions)?),
        });
    }

    // per_source_metadata: merge maps, nest top-level version under each source
    let mut per_source: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for input in inputs {
        // Merge existing per_source_metadata if present
        if let Some(psm_str) = input
            .kv_metadata
            .iter()
            .find(|kv| kv.key == KEY_PER_SOURCE_METADATA)
            .and_then(|kv| kv.value.as_deref())
        {
            if let Ok(psm) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(psm_str)
            {
                for (k, v) in psm {
                    per_source.insert(k, v);
                }
            }
        }

        // Move top-level version and service_queries into per_source_metadata.<source>
        let source_entry = per_source
            .entry(input.source.clone())
            .or_insert_with(|| serde_json::json!({}));
        if let serde_json::Value::Object(map) = source_entry {
            if let Some(version) = input
                .kv_metadata
                .iter()
                .find(|kv| kv.key == KEY_VERSION)
                .and_then(|kv| kv.value.clone())
            {
                map.entry(NESTED_VERSION.to_string())
                    .or_insert(serde_json::Value::String(version));
            }
        }
    }
    if !per_source.is_empty() {
        result.push(KeyValue {
            key: KEY_PER_SOURCE_METADATA.to_string(),
            value: Some(serde_json::to_string(&per_source)?),
        });
    }

    // selection: preserve from primary (rezolus) file if present
    let primary_idx = inputs
        .iter()
        .position(|i| i.source == "rezolus")
        .unwrap_or(0);
    if let Some(sel) = inputs[primary_idx]
        .kv_metadata
        .iter()
        .find(|kv| kv.key == KEY_SELECTION)
        .and_then(|kv| kv.value.clone())
    {
        result.push(KeyValue {
            key: KEY_SELECTION.to_string(),
            value: Some(sel),
        });
    }

    Ok(result)
}

fn find_kv_value(
    inputs: &[InputFile],
    key: &str,
    preferred_source: Option<&str>,
) -> Option<String> {
    if let Some(src) = preferred_source {
        if let Some(input) = inputs.iter().find(|i| i.source == src) {
            if let Some(val) = input
                .kv_metadata
                .iter()
                .find(|kv| kv.key == key)
                .and_then(|kv| kv.value.clone())
            {
                return Some(val);
            }
        }
    }
    inputs.iter().find_map(|i| {
        i.kv_metadata
            .iter()
            .find(|kv| kv.key == key)
            .and_then(|kv| kv.value.clone())
    })
}

// ── Output ──────────────────────────────────────────────────────────────

fn write_parquet(
    output: &PathBuf,
    schema: &SchemaRef,
    batch: &RecordBatch,
    kv_metadata: Vec<KeyValue>,
) -> Result<(), Box<dyn std::error::Error>> {
    let props = WriterProperties::builder()
        .set_key_value_metadata(Some(kv_metadata))
        .set_max_row_group_size(crate::parquet_metadata::MAX_ROW_GROUP_SIZE)
        .build();

    let file = std::fs::File::create(output)?;
    let mut writer = ArrowWriter::try_new(file, schema.clone(), Some(props))?;
    writer.write(batch)?;
    writer.close()?;

    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Int64Array;
    use arrow::datatypes::DataType;
    use tempfile::NamedTempFile;

    /// 1 second in nanoseconds — use as a multiplier so test timestamps
    /// are at a realistic scale relative to the sampling interval.
    const SEC: u64 = 1_000_000_000;

    /// Create a test parquet file with a timestamp column, duration column,
    /// and one gauge metric column.
    fn make_test_file(
        timestamps: &[u64],
        metric_name: &str,
        metric_values: &[Option<i64>],
        source: &str,
        sampling_interval_ms: &str,
    ) -> (NamedTempFile, PathBuf) {
        let ts_field =
            Field::new("timestamp", DataType::UInt64, false).with_metadata(HashMap::from([
                ("metric_type".to_string(), "timestamp".to_string()),
                ("unit".to_string(), "nanoseconds".to_string()),
            ]));
        let dur_field =
            Field::new("duration", DataType::UInt64, true).with_metadata(HashMap::from([
                ("metric_type".to_string(), "duration".to_string()),
                ("unit".to_string(), "nanoseconds".to_string()),
            ]));
        let metric_field = Field::new(metric_name, DataType::Int64, true).with_metadata(
            HashMap::from([("metric_type".to_string(), "gauge".to_string())]),
        );

        let schema = Arc::new(Schema::new(vec![ts_field, dur_field, metric_field]));

        let ts_array = UInt64Array::from(timestamps.to_vec());
        let dur_array = UInt64Array::from(vec![None::<u64>; timestamps.len()]);
        let metric_array = Int64Array::from(metric_values.to_vec());

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(ts_array),
                Arc::new(dur_array),
                Arc::new(metric_array),
            ],
        )
        .unwrap();

        let kv = vec![
            KeyValue {
                key: KEY_SOURCE.to_string(),
                value: Some(source.to_string()),
            },
            KeyValue {
                key: KEY_SAMPLING_INTERVAL_MS.to_string(),
                value: Some(sampling_interval_ms.to_string()),
            },
        ];
        let props = WriterProperties::builder()
            .set_key_value_metadata(Some(kv))
            .build();

        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        (tmp, path)
    }

    fn load(path: &PathBuf) -> InputFile {
        load_single_input(path).unwrap()
    }

    #[test]
    fn test_validate_sources_rejects_duplicate_rezolus() {
        let (_t1, p1) = make_test_file(&[100, 200], "m1", &[Some(1), Some(2)], "rezolus", "1000");
        let (_t2, p2) = make_test_file(&[100, 200], "m2", &[Some(3), Some(4)], "rezolus", "1000");
        let inputs = vec![load(&p1), load(&p2)];
        assert!(validate_sources(&inputs).is_err());
    }

    #[test]
    fn test_validate_sources_allows_one_rezolus() {
        let (_t1, p1) = make_test_file(&[100, 200], "m1", &[Some(1), Some(2)], "rezolus", "1000");
        let (_t2, p2) = make_test_file(&[100, 200], "m2", &[Some(3), Some(4)], "llm-perf", "1000");
        let inputs = vec![load(&p1), load(&p2)];
        assert!(validate_sources(&inputs).is_ok());
    }

    #[test]
    fn test_validate_sampling_interval_mismatch() {
        let (_t1, p1) = make_test_file(&[100, 200], "m1", &[Some(1), Some(2)], "rezolus", "1000");
        let (_t2, p2) = make_test_file(&[100, 200], "m2", &[Some(3), Some(4)], "llm-perf", "500");
        let inputs = vec![load(&p1), load(&p2)];
        assert!(validate_sampling_interval(&inputs).is_err());
    }

    #[test]
    fn test_validate_column_conflicts() {
        let (_t1, p1) = make_test_file(
            &[100, 200],
            "same_name",
            &[Some(1), Some(2)],
            "rezolus",
            "1000",
        );
        let (_t2, p2) = make_test_file(
            &[100, 200],
            "same_name",
            &[Some(3), Some(4)],
            "llm-perf",
            "1000",
        );
        let inputs = vec![load(&p1), load(&p2)];
        assert!(validate_no_column_conflicts(&inputs).is_err());
    }

    #[test]
    fn test_validate_column_shared_ok() {
        // timestamp and duration are shared and should not conflict
        let (_t1, p1) = make_test_file(&[100, 200], "m1", &[Some(1), Some(2)], "rezolus", "1000");
        let (_t2, p2) = make_test_file(&[100, 200], "m2", &[Some(3), Some(4)], "llm-perf", "1000");
        let inputs = vec![load(&p1), load(&p2)];
        assert!(validate_no_column_conflicts(&inputs).is_ok());
    }

    #[test]
    fn test_validate_time_overlap_none() {
        let (_t1, p1) = make_test_file(&[100, 200], "m1", &[Some(1), Some(2)], "rezolus", "1000");
        let (_t2, p2) = make_test_file(&[300, 400], "m2", &[Some(3), Some(4)], "llm-perf", "1000");
        let inputs = vec![load(&p1), load(&p2)];
        assert!(validate_time_overlap(&inputs).is_err());
    }

    #[test]
    fn test_validate_time_overlap_partial() {
        let (_t1, p1) = make_test_file(
            &[100, 200, 300],
            "m1",
            &[Some(1), Some(2), Some(3)],
            "rezolus",
            "1000",
        );
        let (_t2, p2) = make_test_file(
            &[200, 300, 400],
            "m2",
            &[Some(4), Some(5), Some(6)],
            "llm-perf",
            "1000",
        );
        let inputs = vec![load(&p1), load(&p2)];
        assert!(validate_time_overlap(&inputs).is_ok());
    }

    #[test]
    fn test_combine_trims_to_overlap() {
        let (_t1, p1) = make_test_file(
            &[SEC, 2 * SEC, 3 * SEC],
            "m1",
            &[Some(1), Some(2), Some(3)],
            "rezolus",
            "1000",
        );
        let (_t2, p2) = make_test_file(
            &[2 * SEC, 3 * SEC, 4 * SEC],
            "m2",
            &[Some(4), Some(5), Some(6)],
            "llm-perf",
            "1000",
        );

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().to_path_buf();

        let inputs = vec![load(&p1), load(&p2)];
        combine_and_write(&inputs, &out_path).unwrap();

        // Read back
        let file = std::fs::File::open(&out_path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = builder.schema().clone();
        let mut reader = builder.build().unwrap();
        let batch = reader.next().unwrap().unwrap();

        // Only timestamps at 2s and 3s should be present
        assert_eq!(batch.num_rows(), 2);

        let ts_col = batch
            .column(schema.index_of("timestamp").unwrap())
            .as_any()
            .downcast_ref::<UInt64Array>()
            .unwrap();
        assert_eq!(ts_col.value(0), 2 * SEC);
        assert_eq!(ts_col.value(1), 3 * SEC);

        // m1 values for timestamps 2s, 3s are 2, 3
        let m1_col = batch
            .column(schema.index_of("m1").unwrap())
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(m1_col.value(0), 2);
        assert_eq!(m1_col.value(1), 3);

        // m2 values for timestamps 2s, 3s are 4, 5
        let m2_col = batch
            .column(schema.index_of("m2").unwrap())
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(m2_col.value(0), 4);
        assert_eq!(m2_col.value(1), 5);
    }

    #[test]
    fn test_combine_end_to_end() {
        let (_t1, p1) = make_test_file(
            &[SEC, 2 * SEC, 3 * SEC],
            "cpu",
            &[Some(10), Some(20), Some(30)],
            "rezolus",
            "1000",
        );
        let (_t2, p2) = make_test_file(
            &[2 * SEC, 3 * SEC, 4 * SEC],
            "tokens",
            &[Some(40), Some(50), Some(60)],
            "llm-perf",
            "1000",
        );

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().to_path_buf();

        let inputs = load_inputs(&[p1, p2]).unwrap();
        validate_sources(&inputs).unwrap();
        validate_sampling_interval(&inputs).unwrap();
        validate_no_column_conflicts(&inputs).unwrap();
        validate_time_overlap(&inputs).unwrap();
        combine_and_write(&inputs, &out_path).unwrap();

        // Read back and verify schema
        let file = std::fs::File::open(&out_path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = builder.schema().clone();

        let field_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(field_names, vec!["timestamp", "duration", "cpu", "tokens"]);

        // Verify metadata
        let meta_reader =
            SerializedFileReader::new(std::fs::File::open(&out_path).unwrap()).unwrap();
        let kv = meta_reader
            .metadata()
            .file_metadata()
            .key_value_metadata()
            .unwrap();

        let source_val = kv
            .iter()
            .find(|kv| kv.key == KEY_SOURCE)
            .and_then(|kv| kv.value.as_deref())
            .unwrap();
        let sources: Vec<String> = serde_json::from_str(source_val).unwrap();
        assert_eq!(sources, vec!["rezolus", "llm-perf"]);

        let interval_val = kv
            .iter()
            .find(|kv| kv.key == KEY_SAMPLING_INTERVAL_MS)
            .and_then(|kv| kv.value.as_deref())
            .unwrap();
        assert_eq!(interval_val, "1000");
    }

    #[test]
    fn test_combine_preserves_field_metadata() {
        let (_t1, p1) = make_test_file(
            &[SEC, 2 * SEC],
            "m1",
            &[Some(1), Some(2)],
            "rezolus",
            "1000",
        );
        let (_t2, p2) = make_test_file(
            &[SEC, 2 * SEC],
            "m2",
            &[Some(3), Some(4)],
            "llm-perf",
            "1000",
        );

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().to_path_buf();

        let inputs = vec![load(&p1), load(&p2)];
        combine_and_write(&inputs, &out_path).unwrap();

        let file = std::fs::File::open(&out_path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = builder.schema().clone();

        // Check that metric_type metadata is preserved on metric columns
        let m1_field = schema.field_with_name("m1").unwrap();
        assert_eq!(m1_field.metadata().get("metric_type").unwrap(), "gauge");

        let m2_field = schema.field_with_name("m2").unwrap();
        assert_eq!(m2_field.metadata().get("metric_type").unwrap(), "gauge");

        // Check timestamp field metadata
        let ts_field = schema.field_with_name("timestamp").unwrap();
        assert_eq!(ts_field.metadata().get("metric_type").unwrap(), "timestamp");
    }

    #[test]
    fn test_combine_empty_intersection() {
        // Overlapping time range but timestamps land in different buckets
        // (offset by a full interval so they never share a quantized bucket)
        let (_t1, p1) = make_test_file(
            &[SEC, 3 * SEC, 5 * SEC],
            "m1",
            &[Some(1), Some(2), Some(3)],
            "rezolus",
            "2000", // 2s interval
        );
        let (_t2, p2) = make_test_file(
            &[2 * SEC, 4 * SEC, 6 * SEC],
            "m2",
            &[Some(4), Some(5), Some(6)],
            "llm-perf",
            "2000",
        );

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().to_path_buf();

        let inputs = vec![load(&p1), load(&p2)];
        let result = combine_and_write(&inputs, &out_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_combine_fuzzy_timestamp_matching() {
        // Two files with a small phase offset (5ms) on a 1s interval.
        // Quantization should snap them to the same bucket.
        let offset = 5_000_000; // 5ms in ns
        let (_t1, p1) = make_test_file(
            &[SEC, 2 * SEC, 3 * SEC],
            "m1",
            &[Some(10), Some(20), Some(30)],
            "rezolus",
            "1000",
        );
        let (_t2, p2) = make_test_file(
            &[SEC + offset, 2 * SEC + offset, 3 * SEC + offset],
            "m2",
            &[Some(40), Some(50), Some(60)],
            "llm-perf",
            "1000",
        );

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().to_path_buf();

        let inputs = vec![load(&p1), load(&p2)];
        combine_and_write(&inputs, &out_path).unwrap();

        let file = std::fs::File::open(&out_path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = builder.schema().clone();
        let mut reader = builder.build().unwrap();
        let batch = reader.next().unwrap().unwrap();

        // All 3 timestamps should match
        assert_eq!(batch.num_rows(), 3);

        let m2_col = batch
            .column(schema.index_of("m2").unwrap())
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(m2_col.value(0), 40);
        assert_eq!(m2_col.value(1), 50);
        assert_eq!(m2_col.value(2), 60);
    }

    #[test]
    fn test_combine_rejects_poor_alignment() {
        // Two files where >5% of timestamps have phase offset >10% of interval.
        // With a 1s interval, 150ms offset exceeds the 100ms (10%) tolerance.
        let bad_offset = 150_000_000; // 150ms in ns
        let (_t1, p1) = make_test_file(
            &[SEC, 2 * SEC, 3 * SEC],
            "m1",
            &[Some(1), Some(2), Some(3)],
            "rezolus",
            "1000",
        );
        let (_t2, p2) = make_test_file(
            &[SEC + bad_offset, 2 * SEC + bad_offset, 3 * SEC + bad_offset],
            "m2",
            &[Some(4), Some(5), Some(6)],
            "llm-perf",
            "1000",
        );

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().to_path_buf();

        let inputs = vec![load(&p1), load(&p2)];
        let result = combine_and_write(&inputs, &out_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("alignment too poor"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_quantize_rounds_to_nearest() {
        let interval = 1_000_000_000; // 1s
                                      // Exact boundary
        assert_eq!(quantize(2 * interval, interval), 2 * interval);
        // Slightly after boundary
        assert_eq!(quantize(2 * interval + 1000, interval), 2 * interval);
        // Just before next boundary (rounds up)
        assert_eq!(quantize(3 * interval - 1000, interval), 3 * interval);
        // Exactly at midpoint (rounds up)
        assert_eq!(
            quantize(2 * interval + interval / 2, interval),
            3 * interval
        );
    }
}
