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
use std::collections::HashMap;
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
    node: Option<String>,
    instance: Option<String>,
}

/// Combine multiple parquet files into one. Used by the `parquet combine` CLI
/// command and by the multi-endpoint recorder for combined output.
pub(crate) fn combine_files(
    paths: &[PathBuf],
    output: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut inputs = load_inputs(paths)?;
    validate_sampling_interval(&inputs)?;
    validate_labels(&mut inputs)?;
    validate_time_overlap(&inputs)?;
    combine_and_write(&inputs, output, false, None)
}

pub(super) fn run(args: &ArgMatches) -> Result<(), Box<dyn std::error::Error>> {
    let files: Vec<PathBuf> = args
        .get_many::<PathBuf>("FILES")
        .unwrap()
        .cloned()
        .collect();
    let output = args.get_one::<PathBuf>("output").unwrap();
    let bypass_time_check = args.get_flag("bypass-time-check");
    let pinned = args.get_one::<String>("pinned");

    // Phase 1: Load all input files
    let mut inputs = load_inputs(&files)?;

    // Phase 2: Validate (cheapest checks first)
    validate_sampling_interval(&inputs)?;
    validate_labels(&mut inputs)?;
    validate_time_overlap(&inputs)?;

    // Validate --pinned matches an actual rezolus node
    if let Some(pinned_node) = pinned {
        let rez_nodes: Vec<&str> = inputs
            .iter()
            .filter(|i| i.source == "rezolus")
            .filter_map(|i| i.node.as_deref())
            .collect();
        if !rez_nodes.contains(&pinned_node.as_str()) {
            return Err(format!(
                "--pinned {:?} does not match any rezolus node (available: {:?})",
                pinned_node, rez_nodes
            )
            .into());
        }
    }

    // Phase 3: Combine and write
    combine_and_write(&inputs, output, bypass_time_check, pinned)?;

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

    let node = kv_metadata
        .iter()
        .find(|kv| kv.key == KEY_NODE)
        .and_then(|kv| kv.value.clone())
        .or_else(|| {
            // For rezolus files, fall back to filename stem
            if source == "rezolus" {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        });

    let instance = kv_metadata
        .iter()
        .find(|kv| kv.key == KEY_INSTANCE)
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
        node,
        instance,
    })
}

// ── Validation ──────────────────────────────────────────────────────────

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

/// Validate and resolve node/instance labels across all inputs.
///
/// - Rezolus files: `node` must be unique across all rezolus inputs.
/// - Service files: within each source group, either all have `instance`
///   metadata or none do. If none do, auto-assign "0", "1", "2"...
///   If all do, they must be unique within the group.
/// - Mixed (some have instance, some don't) within the same source: error.
fn validate_labels(inputs: &mut [InputFile]) -> Result<(), Box<dyn std::error::Error>> {
    // ── Rezolus node validation ──
    let mut seen_nodes: HashMap<&str, &PathBuf> = HashMap::new();
    for input in inputs.iter() {
        if input.source != "rezolus" {
            continue;
        }
        let node = input.node.as_deref().ok_or_else(|| {
            format!(
                "{:?}: rezolus file has no node label and filename fallback failed",
                input.path
            )
        })?;
        if let Some(prev) = seen_nodes.get(node) {
            return Err(
                format!("duplicate node {:?}: {:?} and {:?}", node, prev, input.path).into(),
            );
        }
        seen_nodes.insert(node, &input.path);
    }

    // ── Service instance validation ──
    // Group non-rezolus files by source (use owned keys to avoid borrow conflicts)
    let mut source_groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, input) in inputs.iter().enumerate() {
        if input.source != "rezolus" {
            source_groups
                .entry(input.source.clone())
                .or_default()
                .push(i);
        }
    }

    for (source, indices) in &source_groups {
        if indices.len() <= 1 {
            // Single file for this source — auto-assign instance "0" if missing
            let idx = indices[0];
            if inputs[idx].instance.is_none() {
                inputs[idx].instance = Some("0".to_string());
            }
            continue;
        }

        let has_instance: Vec<bool> = indices
            .iter()
            .map(|&i| inputs[i].instance.is_some())
            .collect();
        let all_have = has_instance.iter().all(|&b| b);
        let none_have = has_instance.iter().all(|&b| !b);

        if !all_have && !none_have {
            return Err(format!(
                "source {:?}: mixed instance metadata — either all files must have \
                 'instance' metadata or none should",
                source
            )
            .into());
        }

        if none_have {
            // Auto-assign sequential instance IDs
            for (seq, &idx) in indices.iter().enumerate() {
                inputs[idx].instance = Some(seq.to_string());
            }
        } else {
            // All have instance — check for duplicates within the group
            let mut seen: Vec<(&str, &PathBuf)> = Vec::new();
            for &idx in indices {
                let inst = inputs[idx].instance.as_deref().unwrap();
                if let Some((_, prev)) = seen.iter().find(|(s, _)| *s == inst) {
                    return Err(format!(
                        "source {:?}: duplicate instance {:?} in {:?} and {:?}",
                        source, inst, prev, inputs[idx].path
                    )
                    .into());
                }
                seen.push((inst, &inputs[idx].path));
            }
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
    bypass_time_check: bool,
    pinned: Option<&String>,
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
    if bypass_time_check {
        eprintln!("warning: skipping timestamp alignment quality check (--bypass-time-check)");
    } else {
        validate_alignment_quality(inputs, &common_timestamps, interval_ns)?;
    }

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
    let mut merged_kv = merge_metadata(inputs)?;
    if let Some(pinned_node) = pinned {
        merged_kv.push(KeyValue {
            key: KEY_PINNED_NODE.to_string(),
            value: Some(pinned_node.clone()),
        });
    }
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

    // All metric columns from each input, prefixed and labeled
    for input in inputs {
        let (prefix, label_key, label_value) = if input.source == "rezolus" {
            let node = input.node.as_deref().unwrap_or("unknown");
            (node.to_string(), "node".to_string(), node.to_string())
        } else {
            let instance = input.instance.as_deref().unwrap_or("0");
            (
                instance.to_string(),
                "instance".to_string(),
                instance.to_string(),
            )
        };

        for field in input.schema.fields() {
            let name = field.name().as_str();
            if name != "timestamp" && name != "duration" {
                let prefixed_name = format!("{}::{}", prefix, name);
                let mut meta = field.metadata().clone();
                meta.insert("source".to_string(), input.source.clone());
                meta.insert(label_key.clone(), label_value.clone());
                let new_field = Field::new(
                    prefixed_name,
                    field.data_type().clone(),
                    field.is_nullable(),
                )
                .with_metadata(meta);
                fields.push(Arc::new(new_field));
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

    // source: deduplicated JSON array of all source names
    let sources: Vec<&str> = {
        let mut s: Vec<&str> = inputs.iter().map(|i| i.source.as_str()).collect();
        s.sort();
        s.dedup();
        s
    };
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

    // systeminfo: keep first rezolus file as the top-level value (viewer compat).
    // Per-node systeminfo is stashed in per_source_metadata for future use.
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

    // per_source_metadata: nested by source name, then by node/instance ID.
    // Structure: { "rezolus": { "web01": {...}, "web02": {...} },
    //              "vllm":    { "0": {...}, "1": {...} } }
    // For rezolus sources, the sub-key is the node name.
    // For service sources, the sub-key is the instance ID.
    let mut per_source: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for input in inputs {
        // Merge existing per_source_metadata if present (from already-combined files)
        if let Some(psm_str) = input
            .kv_metadata
            .iter()
            .find(|kv| kv.key == KEY_PER_SOURCE_METADATA)
            .and_then(|kv| kv.value.as_deref())
        {
            if let Ok(psm) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(psm_str)
            {
                // Deep-merge: source groups are objects, merge their sub-entries
                for (source_name, group_val) in psm {
                    let target = per_source
                        .entry(source_name)
                        .or_insert_with(|| serde_json::json!({}));
                    if let (
                        serde_json::Value::Object(target_map),
                        serde_json::Value::Object(src_map),
                    ) = (target, group_val)
                    {
                        for (k, v) in src_map {
                            target_map.entry(k).or_insert(v);
                        }
                    }
                }
            }
        }

        // Determine the sub-key within this source's group
        let sub_key = if input.source == "rezolus" {
            input.node.as_deref().unwrap_or("unknown").to_string()
        } else {
            input.instance.as_deref().unwrap_or("0").to_string()
        };

        let source_group = per_source
            .entry(input.source.clone())
            .or_insert_with(|| serde_json::json!({}));
        let serde_json::Value::Object(group_map) = source_group else {
            continue;
        };
        let entry = group_map
            .entry(sub_key)
            .or_insert_with(|| serde_json::json!({}));
        let serde_json::Value::Object(map) = entry else {
            continue;
        };

        // Move top-level version into per-source entry
        if let Some(version) = input
            .kv_metadata
            .iter()
            .find(|kv| kv.key == KEY_VERSION)
            .and_then(|kv| kv.value.clone())
        {
            map.entry(NESTED_VERSION.to_string())
                .or_insert(serde_json::Value::String(version));
        }

        // Store node or instance label in per-source metadata
        if input.source == "rezolus" {
            if let Some(ref node) = input.node {
                map.entry(NESTED_NODE.to_string())
                    .or_insert(serde_json::Value::String(node.clone()));
            }
        } else {
            if let Some(ref instance) = input.instance {
                map.entry(NESTED_INSTANCE.to_string())
                    .or_insert(serde_json::Value::String(instance.clone()));
            }
            // Also store node for service files if present (informational — which host it ran on)
            if let Some(ref node) = input.node {
                map.entry(NESTED_NODE.to_string())
                    .or_insert(serde_json::Value::String(node.clone()));
            }
        }
    }

    // Stash per-node systeminfo into per_source_metadata entries
    for input in inputs.iter().filter(|i| i.source == "rezolus") {
        if let Some(sysinfo_val) = input
            .kv_metadata
            .iter()
            .find(|kv| kv.key == KEY_SYSTEMINFO)
            .and_then(|kv| kv.value.as_deref())
        {
            let node = input.node.as_deref().unwrap_or("unknown");
            if let Some(serde_json::Value::Object(group)) = per_source.get_mut("rezolus") {
                if let Some(serde_json::Value::Object(map)) = group.get_mut(node) {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(sysinfo_val) {
                        map.entry(KEY_SYSTEMINFO.to_string()).or_insert(parsed);
                    }
                }
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
        .set_compression(parquet::basic::Compression::ZSTD(Default::default()))
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
    fn test_validate_sampling_interval_mismatch() {
        let (_t1, p1) = make_test_file(&[100, 200], "m1", &[Some(1), Some(2)], "rezolus", "1000");
        let (_t2, p2) = make_test_file(&[100, 200], "m2", &[Some(3), Some(4)], "llm-perf", "500");
        let inputs = vec![load(&p1), load(&p2)];
        assert!(validate_sampling_interval(&inputs).is_err());
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

        let mut inputs = vec![load(&p1), load(&p2)];
        validate_labels(&mut inputs).unwrap();
        combine_and_write(&inputs, &out_path, false, None).unwrap();

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

        // Find prefixed column names
        let field_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        let m1_col_name = *field_names.iter().find(|n| n.ends_with("::m1")).unwrap();
        let m2_col_name = *field_names.iter().find(|n| n.ends_with("::m2")).unwrap();

        let m1_col = batch
            .column(schema.index_of(m1_col_name).unwrap())
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(m1_col.value(0), 2);
        assert_eq!(m1_col.value(1), 3);

        let m2_col = batch
            .column(schema.index_of(m2_col_name).unwrap())
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

        let mut inputs = load_inputs(&[p1, p2]).unwrap();
        validate_sampling_interval(&inputs).unwrap();
        validate_labels(&mut inputs).unwrap();
        validate_time_overlap(&inputs).unwrap();
        combine_and_write(&inputs, &out_path, false, None).unwrap();

        // Read back and verify schema
        let file = std::fs::File::open(&out_path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = builder.schema().clone();

        // Columns are now prefixed with node/instance
        let field_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(field_names.len(), 4); // timestamp, duration, <node>::cpu, 0::tokens
        assert!(field_names[2].ends_with("::cpu"));
        assert!(field_names[3].starts_with("0::tokens"));

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
        assert!(sources.contains(&"rezolus".to_string()));
        assert!(sources.contains(&"llm-perf".to_string()));

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

        let mut inputs = vec![load(&p1), load(&p2)];
        validate_labels(&mut inputs).unwrap();
        combine_and_write(&inputs, &out_path, false, None).unwrap();

        let file = std::fs::File::open(&out_path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = builder.schema().clone();

        // Find prefixed column names
        let field_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        let m1_name = *field_names.iter().find(|n| n.ends_with("::m1")).unwrap();
        let m2_name = *field_names.iter().find(|n| n.ends_with("::m2")).unwrap();

        // Check that metric_type metadata is preserved on metric columns
        let m1_field = schema.field_with_name(m1_name).unwrap();
        assert_eq!(m1_field.metadata().get("metric_type").unwrap(), "gauge");
        assert_eq!(m1_field.metadata().get("source").unwrap(), "rezolus");
        assert!(m1_field.metadata().contains_key("node"));

        let m2_field = schema.field_with_name(m2_name).unwrap();
        assert_eq!(m2_field.metadata().get("metric_type").unwrap(), "gauge");
        assert_eq!(m2_field.metadata().get("source").unwrap(), "llm-perf");
        assert!(m2_field.metadata().contains_key("instance"));

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
        let result = combine_and_write(&inputs, &out_path, false, None);
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

        let mut inputs = vec![load(&p1), load(&p2)];
        validate_labels(&mut inputs).unwrap();
        combine_and_write(&inputs, &out_path, false, None).unwrap();

        let file = std::fs::File::open(&out_path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = builder.schema().clone();
        let mut reader = builder.build().unwrap();
        let batch = reader.next().unwrap().unwrap();

        // All 3 timestamps should match
        assert_eq!(batch.num_rows(), 3);

        let field_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        let m2_col_name = *field_names.iter().find(|n| n.ends_with("::m2")).unwrap();
        let m2_col = batch
            .column(schema.index_of(m2_col_name).unwrap())
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
        let result = combine_and_write(&inputs, &out_path, false, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("alignment too poor"),
            "unexpected error: {err}"
        );
    }

    fn make_test_file_with_metadata(
        timestamps: &[u64],
        metric_name: &str,
        metric_values: &[Option<i64>],
        source: &str,
        sampling_interval_ms: &str,
        extra_kv: Vec<(&str, &str)>,
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

        let mut kv = vec![
            KeyValue {
                key: KEY_SOURCE.to_string(),
                value: Some(source.to_string()),
            },
            KeyValue {
                key: KEY_SAMPLING_INTERVAL_MS.to_string(),
                value: Some(sampling_interval_ms.to_string()),
            },
        ];
        for (k, v) in extra_kv {
            kv.push(KeyValue {
                key: k.to_string(),
                value: Some(v.to_string()),
            });
        }
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

    // ── Label resolution tests ──

    #[test]
    fn test_resolve_node_from_metadata() {
        let (_t1, p1) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "cpu",
            &[Some(10), Some(20)],
            "rezolus",
            "1000",
            vec![("node", "web01")],
        );
        let input = load(&p1);
        assert_eq!(input.node.as_deref(), Some("web01"));
    }

    #[test]
    fn test_resolve_node_fallback_to_filename() {
        let (_t1, p1) = make_test_file(
            &[SEC, 2 * SEC],
            "cpu",
            &[Some(10), Some(20)],
            "rezolus",
            "1000",
        );
        let input = load(&p1);
        assert!(input.node.is_some());
    }

    #[test]
    fn test_resolve_instance_from_metadata() {
        let (_t1, p1) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "tokens",
            &[Some(10), Some(20)],
            "vllm",
            "1000",
            vec![("instance", "primary")],
        );
        let input = load(&p1);
        assert_eq!(input.instance.as_deref(), Some("primary"));
    }

    #[test]
    fn test_resolve_instance_none_when_absent() {
        let (_t1, p1) = make_test_file(
            &[SEC, 2 * SEC],
            "tokens",
            &[Some(10), Some(20)],
            "vllm",
            "1000",
        );
        let input = load(&p1);
        assert!(input.instance.is_none());
    }

    // ── Label validation tests ──

    #[test]
    fn test_validate_labels_rejects_duplicate_nodes() {
        let (_t1, p1) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "cpu",
            &[Some(10), Some(20)],
            "rezolus",
            "1000",
            vec![("node", "web01")],
        );
        let (_t2, p2) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "cpu",
            &[Some(30), Some(40)],
            "rezolus",
            "1000",
            vec![("node", "web01")],
        );
        let mut inputs = vec![load(&p1), load(&p2)];
        assert!(validate_labels(&mut inputs).is_err());
    }

    #[test]
    fn test_validate_labels_allows_different_nodes() {
        let (_t1, p1) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "cpu",
            &[Some(10), Some(20)],
            "rezolus",
            "1000",
            vec![("node", "web01")],
        );
        let (_t2, p2) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "cpu",
            &[Some(30), Some(40)],
            "rezolus",
            "1000",
            vec![("node", "web02")],
        );
        let mut inputs = vec![load(&p1), load(&p2)];
        assert!(validate_labels(&mut inputs).is_ok());
    }

    #[test]
    fn test_validate_labels_rejects_mixed_instance_metadata() {
        let (_t1, p1) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "tokens_a",
            &[Some(10), Some(20)],
            "vllm",
            "1000",
            vec![("instance", "primary")],
        );
        let (_t2, p2) = make_test_file(
            &[SEC, 2 * SEC],
            "tokens_b",
            &[Some(30), Some(40)],
            "vllm",
            "1000",
        );
        let mut inputs = vec![load(&p1), load(&p2)];
        let err = validate_labels(&mut inputs).unwrap_err().to_string();
        assert!(err.contains("mixed"), "unexpected error: {err}");
    }

    #[test]
    fn test_validate_labels_auto_assigns_instances() {
        let (_t1, p1) = make_test_file(
            &[SEC, 2 * SEC],
            "tokens_a",
            &[Some(10), Some(20)],
            "vllm",
            "1000",
        );
        let (_t2, p2) = make_test_file(
            &[SEC, 2 * SEC],
            "tokens_b",
            &[Some(30), Some(40)],
            "vllm",
            "1000",
        );
        let mut inputs = vec![load(&p1), load(&p2)];
        validate_labels(&mut inputs).unwrap();
        assert_eq!(inputs[0].instance.as_deref(), Some("0"));
        assert_eq!(inputs[1].instance.as_deref(), Some("1"));
    }

    // ── Multi-node / multi-instance combine tests ──

    #[test]
    fn test_combine_two_rezolus_nodes() {
        let (_t1, p1) = make_test_file_with_metadata(
            &[SEC, 2 * SEC, 3 * SEC],
            "cpu",
            &[Some(10), Some(20), Some(30)],
            "rezolus",
            "1000",
            vec![("node", "web01")],
        );
        let (_t2, p2) = make_test_file_with_metadata(
            &[SEC, 2 * SEC, 3 * SEC],
            "cpu",
            &[Some(40), Some(50), Some(60)],
            "rezolus",
            "1000",
            vec![("node", "web02")],
        );

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().to_path_buf();

        let mut inputs = vec![load(&p1), load(&p2)];
        validate_labels(&mut inputs).unwrap();
        combine_and_write(&inputs, &out_path, false, None).unwrap();

        let file = std::fs::File::open(&out_path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = builder.schema().clone();
        let mut reader = builder.build().unwrap();
        let batch = reader.next().unwrap().unwrap();

        assert_eq!(batch.num_rows(), 3);

        let field_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(
            field_names,
            vec!["timestamp", "duration", "web01::cpu", "web02::cpu"]
        );

        let web01_field = schema.field_with_name("web01::cpu").unwrap();
        assert_eq!(web01_field.metadata().get("node").unwrap(), "web01");
        assert_eq!(web01_field.metadata().get("source").unwrap(), "rezolus");

        let web02_field = schema.field_with_name("web02::cpu").unwrap();
        assert_eq!(web02_field.metadata().get("node").unwrap(), "web02");

        let web01_col = batch
            .column(schema.index_of("web01::cpu").unwrap())
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(web01_col.value(0), 10);

        let web02_col = batch
            .column(schema.index_of("web02::cpu").unwrap())
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(web02_col.value(0), 40);
    }

    #[test]
    fn test_combine_two_service_instances() {
        let (_t1, p1) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "tokens",
            &[Some(100), Some(200)],
            "vllm",
            "1000",
            vec![("instance", "primary")],
        );
        let (_t2, p2) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "tokens",
            &[Some(300), Some(400)],
            "vllm",
            "1000",
            vec![("instance", "secondary")],
        );

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().to_path_buf();

        let mut inputs = vec![load(&p1), load(&p2)];
        validate_labels(&mut inputs).unwrap();
        combine_and_write(&inputs, &out_path, false, None).unwrap();

        let file = std::fs::File::open(&out_path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = builder.schema().clone();

        let field_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(
            field_names,
            vec![
                "timestamp",
                "duration",
                "primary::tokens",
                "secondary::tokens"
            ]
        );

        let primary_field = schema.field_with_name("primary::tokens").unwrap();
        assert_eq!(primary_field.metadata().get("instance").unwrap(), "primary");
        assert_eq!(primary_field.metadata().get("source").unwrap(), "vllm");
    }

    #[test]
    fn test_combine_mixed_rezolus_and_service() {
        let (_t1, p1) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "cpu",
            &[Some(10), Some(20)],
            "rezolus",
            "1000",
            vec![("node", "web01")],
        );
        let (_t2, p2) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "cpu",
            &[Some(30), Some(40)],
            "rezolus",
            "1000",
            vec![("node", "web02")],
        );
        let (_t3, p3) = make_test_file(
            &[SEC, 2 * SEC],
            "tokens",
            &[Some(100), Some(200)],
            "vllm",
            "1000",
        );

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().to_path_buf();

        let mut inputs = vec![load(&p1), load(&p2), load(&p3)];
        validate_labels(&mut inputs).unwrap();
        combine_and_write(&inputs, &out_path, false, None).unwrap();

        let file = std::fs::File::open(&out_path).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = builder.schema().clone();

        let field_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        assert_eq!(
            field_names,
            vec![
                "timestamp",
                "duration",
                "web01::cpu",
                "web02::cpu",
                "0::tokens"
            ]
        );
    }

    #[test]
    fn test_merge_metadata_includes_nodes() {
        let (_t1, p1) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "cpu",
            &[Some(10), Some(20)],
            "rezolus",
            "1000",
            vec![("node", "web01")],
        );
        let (_t2, p2) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "cpu",
            &[Some(30), Some(40)],
            "rezolus",
            "1000",
            vec![("node", "web02")],
        );

        let mut inputs = vec![load(&p1), load(&p2)];
        validate_labels(&mut inputs).unwrap();

        let out_tmp = NamedTempFile::new().unwrap();
        let out_path = out_tmp.path().to_path_buf();
        combine_and_write(&inputs, &out_path, false, None).unwrap();

        let meta_reader =
            SerializedFileReader::new(std::fs::File::open(&out_path).unwrap()).unwrap();
        let kv = meta_reader
            .metadata()
            .file_metadata()
            .key_value_metadata()
            .unwrap();

        // source array should be deduplicated
        let source_val = kv
            .iter()
            .find(|kv| kv.key == KEY_SOURCE)
            .and_then(|kv| kv.value.as_deref())
            .unwrap();
        let sources: Vec<String> = serde_json::from_str(source_val).unwrap();
        assert_eq!(sources, vec!["rezolus"]);

        // per_source_metadata nested by source, then by node/instance
        let psm_str = kv
            .iter()
            .find(|kv| kv.key == KEY_PER_SOURCE_METADATA)
            .and_then(|kv| kv.value.as_deref())
            .unwrap();
        let psm: serde_json::Value = serde_json::from_str(psm_str).unwrap();

        assert!(psm["rezolus"].get("web01").is_some());
        assert!(psm["rezolus"].get("web02").is_some());
        assert_eq!(psm["rezolus"]["web01"]["node"].as_str().unwrap(), "web01");
        assert_eq!(psm["rezolus"]["web02"]["node"].as_str().unwrap(), "web02");
    }

    #[test]
    fn test_merge_metadata_includes_service_node() {
        let (_t1, p1) = make_test_file_with_metadata(
            &[SEC, 2 * SEC],
            "metric_a",
            &[Some(1), Some(2)],
            "vllm",
            "1000",
            vec![("instance", "0"), ("node", "gpu01")],
        );

        let inputs = vec![load(&p1)];
        let kv = merge_metadata(&inputs).unwrap();

        let psm_str = kv
            .iter()
            .find(|kv| kv.key == KEY_PER_SOURCE_METADATA)
            .and_then(|kv| kv.value.as_deref())
            .expect("per_source_metadata should exist");

        let psm: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(psm_str).unwrap();

        let entry = psm
            .get("vllm")
            .and_then(|g| g.get("0"))
            .expect("vllm.0 entry should exist");
        assert_eq!(entry.get("instance").and_then(|v| v.as_str()), Some("0"));
        assert_eq!(entry.get("node").and_then(|v| v.as_str()), Some("gpu01"));
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
