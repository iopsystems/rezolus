use std::collections::HashMap;

use arrow::array::{Array, AsArray};
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;

/// Walk a sequence of Arrow `RecordBatch`es and emit Prometheus
/// matrix-shape JSON. Empty input (no batches, or batches with zero rows)
/// emits the canonical empty-matrix response.
///
/// Schema contract (see crate docs): exactly one `t` field (timestamp,
/// seconds since epoch, coerced to f64) and one `v` field (numeric, NULLs
/// drop the row). All other fields become labels on the series. The first
/// batch's schema is authoritative; later batches must match (DuckDB's
/// `query_arrow` guarantees this for a single statement).
pub fn arrow_to_prom_matrix(batches: &[RecordBatch]) -> String {
    let Some(first) = batches.iter().find(|b| b.num_rows() > 0) else {
        return crate::EMPTY_PROM_MATRIX.to_string();
    };

    let schema = first.schema();
    let mut t_idx: Option<usize> = None;
    let mut v_idx: Option<usize> = None;
    let mut label_indices: Vec<(usize, String)> = Vec::new();
    for (i, field) in schema.fields().iter().enumerate() {
        match field.name().as_str() {
            "t" => t_idx = Some(i),
            "v" => v_idx = Some(i),
            name => label_indices.push((i, name.to_string())),
        }
    }
    let Some(t_idx) = t_idx else {
        return crate::EMPTY_PROM_MATRIX.to_string();
    };
    let Some(v_idx) = v_idx else {
        return crate::EMPTY_PROM_MATRIX.to_string();
    };

    let mut groups: Vec<crate::SeriesGroup> = Vec::new();
    let mut group_index: HashMap<String, usize> = HashMap::new();

    for batch in batches {
        if batch.num_rows() == 0 {
            continue;
        }
        let t_col = batch.column(t_idx);
        let v_col = batch.column(v_idx);
        let label_cols: Vec<(&dyn Array, &str)> = label_indices
            .iter()
            .map(|(i, name)| (batch.column(*i).as_ref(), name.as_str()))
            .collect();

        for row in 0..batch.num_rows() {
            let Some(v_str) = cell_to_string_opt(v_col.as_ref(), row) else {
                continue; // NULL value → drop row (Prometheus gap)
            };
            let t_secs = match cell_to_f64(t_col.as_ref(), row) {
                Some(t) => t,
                None => continue,
            };

            let mut metric = serde_json::Map::with_capacity(label_cols.len());
            for (col, name) in &label_cols {
                let s = cell_to_string_opt(*col, row).unwrap_or_else(|| "null".to_string());
                metric.insert((*name).to_string(), serde_json::Value::String(s));
            }

            let key = serde_json::to_string(&metric).unwrap_or_default();
            let idx = match group_index.get(&key) {
                Some(&idx) => idx,
                None => {
                    let idx = groups.len();
                    group_index.insert(key, idx);
                    groups.push((metric, Vec::new()));
                    idx
                }
            };
            groups[idx].1.push((t_secs, v_str));
        }
    }

    crate::emit_prom_matrix_json(&groups)
}

/// Stringify a single Arrow cell. Returns `None` for NULL values (caller
/// drops the row for `v` columns; for label columns we substitute the
/// literal `"null"` string instead, matching the WASM path's behavior).
///
/// Non-finite floats (`NaN`, `+Inf`, `-Inf`) also return `None`. Their
/// `Display` impls produce `"NaN"` / `"inf"` / `"-inf"` which are not
/// valid JSON numbers; the surrounding matrix shape wraps them in
/// quotes so they don't crash `serde_json::Value` parsing, but the
/// frontend's `Number(...)` parse of `"NaN"` yields NaN which ECharts
/// then draws as ugly axis gaps. Treating non-finite values as gaps
/// (drop the row, just like NULL) matches Prometheus semantics and
/// keeps plots clean.
fn cell_to_string_opt(arr: &dyn Array, row: usize) -> Option<String> {
    if arr.is_null(row) {
        return None;
    }
    Some(match arr.data_type() {
        DataType::Float64 => {
            let v = arr.as_primitive::<arrow::datatypes::Float64Type>().value(row);
            if !v.is_finite() {
                return None;
            }
            v.to_string()
        }
        DataType::Float32 => {
            let v = arr.as_primitive::<arrow::datatypes::Float32Type>().value(row);
            if !v.is_finite() {
                return None;
            }
            v.to_string()
        }
        DataType::Int64 => arr.as_primitive::<arrow::datatypes::Int64Type>().value(row).to_string(),
        DataType::Int32 => arr.as_primitive::<arrow::datatypes::Int32Type>().value(row).to_string(),
        DataType::Int16 => arr.as_primitive::<arrow::datatypes::Int16Type>().value(row).to_string(),
        DataType::Int8 => arr.as_primitive::<arrow::datatypes::Int8Type>().value(row).to_string(),
        DataType::UInt64 => arr.as_primitive::<arrow::datatypes::UInt64Type>().value(row).to_string(),
        DataType::UInt32 => arr.as_primitive::<arrow::datatypes::UInt32Type>().value(row).to_string(),
        DataType::UInt16 => arr.as_primitive::<arrow::datatypes::UInt16Type>().value(row).to_string(),
        DataType::UInt8 => arr.as_primitive::<arrow::datatypes::UInt8Type>().value(row).to_string(),
        DataType::Boolean => arr.as_boolean().value(row).to_string(),
        DataType::Utf8 => arr.as_string::<i32>().value(row).to_string(),
        DataType::LargeUtf8 => arr.as_string::<i64>().value(row).to_string(),
        _ => format!("{arr:?}#{row}"), // unsupported dtype — keep something printable
    })
}

fn cell_to_f64(arr: &dyn Array, row: usize) -> Option<f64> {
    if arr.is_null(row) {
        return None;
    }
    Some(match arr.data_type() {
        DataType::Float64 => arr.as_primitive::<arrow::datatypes::Float64Type>().value(row),
        DataType::Float32 => arr.as_primitive::<arrow::datatypes::Float32Type>().value(row) as f64,
        DataType::Int64 => arr.as_primitive::<arrow::datatypes::Int64Type>().value(row) as f64,
        DataType::Int32 => arr.as_primitive::<arrow::datatypes::Int32Type>().value(row) as f64,
        DataType::UInt64 => arr.as_primitive::<arrow::datatypes::UInt64Type>().value(row) as f64,
        DataType::UInt32 => arr.as_primitive::<arrow::datatypes::UInt32Type>().value(row) as f64,
        _ => return None,
    })
}
