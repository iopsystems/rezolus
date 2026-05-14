//! Generate two minimal single-source parquet fixtures for the AB smoke
//! test. Each output carries a tiny timestamp / duration / gauge column,
//! distinct `source` footer values, and a 1000 ms sampling interval —
//! everything `parquet combine --ab` needs to disambiguate sides.
//!
//! Invoke as: `cargo run --example gen_ab_fixtures -- <a.parquet> <b.parquet>`
//! (or use the prebuilt binary at `target/debug/examples/gen_ab_fixtures`).

use std::path::PathBuf;
use std::sync::Arc;

use arrow::array::{Int64Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        eprintln!("usage: gen_ab_fixtures <a.parquet> <b.parquet>");
        std::process::exit(2);
    }
    write_fixture(&PathBuf::from(&args[0]), "source-a", 1_000_000_000)?;
    write_fixture(&PathBuf::from(&args[1]), "source-b", 2_000_000_000)?;
    Ok(())
}

fn write_fixture(
    path: &PathBuf,
    source: &str,
    base_ts_ns: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let sec_ns = 1_000_000_000u64;
    let ts_values: Vec<u64> = (0..3).map(|i| base_ts_ns + i * sec_ns).collect();

    let mut metric_meta = std::collections::HashMap::new();
    metric_meta.insert("metric_type".to_string(), "gauge".to_string());

    let schema = Arc::new(Schema::new(vec![
        Field::new("timestamp", DataType::UInt64, false),
        Field::new("duration", DataType::UInt64, false),
        Field::new("queue_depth", DataType::Int64, false).with_metadata(metric_meta),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(UInt64Array::from(ts_values)),
            Arc::new(UInt64Array::from(vec![sec_ns; 3])),
            Arc::new(Int64Array::from(vec![1i64, 2, 3])),
        ],
    )?;

    let kv = vec![
        KeyValue {
            key: "source".to_string(),
            value: Some(source.to_string()),
        },
        KeyValue {
            key: "sampling_interval_ms".to_string(),
            value: Some("1000".to_string()),
        },
    ];
    let props = WriterProperties::builder()
        .set_key_value_metadata(Some(kv))
        .build();

    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(())
}
