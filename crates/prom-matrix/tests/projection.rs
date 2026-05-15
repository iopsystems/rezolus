//! Native projection tests for `arrow_to_prom_matrix`.
//!
//! Covers the edge cases not exercised by the production code path
//! (which only sees DuckDB-emitted batches): NULL handling, non-finite
//! float values, multi-batch grouping, and the empty-input contract.

use std::sync::Arc;

use arrow::array::{Float64Array, RecordBatch, StringArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};

use prom_matrix::{arrow_to_prom_matrix, EMPTY_PROM_MATRIX};

fn schema_t_v() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("t", DataType::Float64, false),
        Field::new("v", DataType::Float64, true),
    ]))
}

fn schema_t_v_label() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("t", DataType::Float64, false),
        Field::new("v", DataType::Float64, true),
        Field::new("id", DataType::Utf8, false),
    ]))
}

#[test]
fn empty_batches_return_canonical_empty_matrix() {
    let json = arrow_to_prom_matrix(&[]);
    assert_eq!(json, EMPTY_PROM_MATRIX);
}

#[test]
fn zero_row_batch_returns_canonical_empty_matrix() {
    let batch = RecordBatch::try_new(
        schema_t_v(),
        vec![
            Arc::new(Float64Array::from(Vec::<f64>::new())),
            Arc::new(Float64Array::from(Vec::<f64>::new())),
        ],
    )
    .unwrap();
    let json = arrow_to_prom_matrix(&[batch]);
    assert_eq!(json, EMPTY_PROM_MATRIX);
}

#[test]
fn single_series_basic_projection() {
    let batch = RecordBatch::try_new(
        schema_t_v(),
        vec![
            Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0])),
            Arc::new(Float64Array::from(vec![Some(10.0), Some(20.0), Some(30.0)])),
        ],
    )
    .unwrap();
    let json = arrow_to_prom_matrix(&[batch]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let result = &parsed["data"]["result"];
    assert_eq!(result.as_array().unwrap().len(), 1);
    let values = result[0]["values"].as_array().unwrap();
    assert_eq!(values.len(), 3);
    assert_eq!(values[0][0].as_f64().unwrap(), 1.0);
    assert_eq!(values[0][1].as_str().unwrap(), "10");
}

#[test]
fn null_v_rows_dropped() {
    let batch = RecordBatch::try_new(
        schema_t_v(),
        vec![
            Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0])),
            Arc::new(Float64Array::from(vec![Some(10.0), None, Some(30.0)])),
        ],
    )
    .unwrap();
    let json = arrow_to_prom_matrix(&[batch]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let values = parsed["data"]["result"][0]["values"].as_array().unwrap();
    // Row at t=2.0 had NULL v — dropped (Prometheus gap).
    assert_eq!(values.len(), 2);
    assert_eq!(values[0][0].as_f64().unwrap(), 1.0);
    assert_eq!(values[1][0].as_f64().unwrap(), 3.0);
}

#[test]
fn non_finite_v_rows_dropped() {
    // NaN, +Inf, -Inf all produce gaps. `f64::to_string()` would
    // otherwise stringify them as `"NaN"` / `"inf"` / `"-inf"`,
    // breaking the frontend's `Number(...)` parsing.
    let batch = RecordBatch::try_new(
        schema_t_v(),
        vec![
            Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0, 4.0, 5.0])),
            Arc::new(Float64Array::from(vec![
                Some(10.0),
                Some(f64::NAN),
                Some(f64::INFINITY),
                Some(f64::NEG_INFINITY),
                Some(50.0),
            ])),
        ],
    )
    .unwrap();
    let json = arrow_to_prom_matrix(&[batch]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let values = parsed["data"]["result"][0]["values"].as_array().unwrap();
    // Only the two finite rows survive.
    assert_eq!(values.len(), 2);
    assert_eq!(values[0][0].as_f64().unwrap(), 1.0);
    assert_eq!(values[0][1].as_str().unwrap(), "10");
    assert_eq!(values[1][0].as_f64().unwrap(), 5.0);
    assert_eq!(values[1][1].as_str().unwrap(), "50");
}

#[test]
fn all_nan_series_collapses_to_empty_matrix() {
    // A series whose entire v column is NaN/Inf drops every row, leaving
    // a series with zero values. Documented behavior: such a series is
    // omitted from the result entirely (frontend has nothing to render),
    // matching Prometheus "no data" semantics. Distinct from the mixed
    // case in `non_finite_v_rows_dropped`, which preserves the series
    // alongside surviving rows.
    let batch = RecordBatch::try_new(
        schema_t_v(),
        vec![
            Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0])),
            Arc::new(Float64Array::from(vec![
                Some(f64::NAN),
                Some(f64::INFINITY),
                Some(f64::NEG_INFINITY),
            ])),
        ],
    )
    .unwrap();
    let json = arrow_to_prom_matrix(&[batch]);
    assert_eq!(json, EMPTY_PROM_MATRIX);
}

#[test]
fn label_columns_group_into_series() {
    let batch = RecordBatch::try_new(
        schema_t_v_label(),
        vec![
            Arc::new(Float64Array::from(vec![1.0, 1.0, 2.0, 2.0])),
            Arc::new(Float64Array::from(vec![
                Some(10.0),
                Some(11.0),
                Some(20.0),
                Some(21.0),
            ])),
            Arc::new(StringArray::from(vec!["a", "b", "a", "b"])),
        ],
    )
    .unwrap();
    let json = arrow_to_prom_matrix(&[batch]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let result = parsed["data"]["result"].as_array().unwrap();
    assert_eq!(result.len(), 2);
    // Insertion order is preserved; `a` appears first.
    assert_eq!(result[0]["metric"]["id"].as_str().unwrap(), "a");
    assert_eq!(result[1]["metric"]["id"].as_str().unwrap(), "b");
    assert_eq!(result[0]["values"].as_array().unwrap().len(), 2);
    assert_eq!(result[1]["values"].as_array().unwrap().len(), 2);
}

#[test]
fn multi_batch_concatenation() {
    let b1 = RecordBatch::try_new(
        schema_t_v(),
        vec![
            Arc::new(Float64Array::from(vec![1.0, 2.0])),
            Arc::new(Float64Array::from(vec![Some(10.0), Some(20.0)])),
        ],
    )
    .unwrap();
    let b2 = RecordBatch::try_new(
        schema_t_v(),
        vec![
            Arc::new(Float64Array::from(vec![3.0, 4.0])),
            Arc::new(Float64Array::from(vec![Some(30.0), Some(40.0)])),
        ],
    )
    .unwrap();
    let json = arrow_to_prom_matrix(&[b1, b2]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let values = parsed["data"]["result"][0]["values"].as_array().unwrap();
    assert_eq!(values.len(), 4);
    assert_eq!(values[0][0].as_f64().unwrap(), 1.0);
    assert_eq!(values[3][0].as_f64().unwrap(), 4.0);
}

#[test]
fn ubigint_v_round_trips_as_string() {
    // Large UInt64 values are stringified rather than going through
    // f64 (where we'd lose precision past 2^53). Frontend parses the
    // string; integer cells survive.
    let schema = Arc::new(Schema::new(vec![
        Field::new("t", DataType::Float64, false),
        Field::new("v", DataType::UInt64, true),
    ]));
    let big = (1u64 << 60) + 7;
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Float64Array::from(vec![1.0])),
            Arc::new(UInt64Array::from(vec![Some(big)])),
        ],
    )
    .unwrap();
    let json = arrow_to_prom_matrix(&[batch]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let v_str = parsed["data"]["result"][0]["values"][0][1]
        .as_str()
        .unwrap();
    assert_eq!(v_str, big.to_string());
}
