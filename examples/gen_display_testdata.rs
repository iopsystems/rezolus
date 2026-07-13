//! Synthetic display-mode test data with KNOWN properties, so backend tests can
//! assert the decimation guarantees *exactly* rather than eyeballing real data:
//!
//!   - `synth_gauge`   — flat baseline + isolated 1-sample spikes at known
//!                       seconds (tests min/max spike preservation).
//!   - `synth_counter` — cumulative counter whose rate has periodic bursts
//!                       (tests rate() + band over a bursty signal).
//!   - `synth_latency` — H2 histogram, ~1 ms bulk + a heavy tail, with extra
//!                       tail mass injected at known seconds (tests percentile
//!                       scatter/bands, the bucket heatmap, and the spectrum).
//!
//! Duration is 3600 s at 1 s interval, so any point budget < 3600 forces
//! decimation and exercises the display/heatmap decimate-then-refetch path.
//!
//! One output → single-capture file. Two outputs → baseline + experiment, where
//! the experiment carries a deliberate REGRESSION (higher gauge floor, ~2×
//! latency) so compare-mode / diff paths have a detectable delta.
//!
//!   cargo run --example gen_display_testdata -- <baseline.parquet> [<experiment.parquet>]

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, ListArray, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema, UInt64Type};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::file::metadata::KeyValue;
use parquet::file::properties::WriterProperties;

const DURATION_S: u64 = 3600;
const SEC_NS: u64 = 1_000_000_000;
const BASE_TS_NS: u64 = 1_700_000_000 * SEC_NS; // fixed epoch so tests are deterministic
const GP: u8 = 4; // histogram grouping_power
const MVP: u8 = 30; // histogram max_value_power (~1.07 s ceiling; fits 2× regression)

// Gauge: flat floor with isolated 1-sample spikes at these seconds. Chosen at
// NON-round seconds so a coarsely point-sampled query steps right over them —
// only the min/max envelope preserves them, which is exactly what we assert.
const GAUGE_FLOOR: i64 = 100;
const GAUGE_SPIKE: i64 = 900;
const GAUGE_SPIKE_SECS: &[u64] = &[907, 1823, 2731];

// Latency: extra tail mass (~100 ms) injected at these seconds (also off-grid).
const LAT_SPIKE_SECS: &[u64] = &[1237, 2411];
const MS: u64 = 1_000_000; // ns per millisecond

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.len() > 2 {
        eprintln!("usage: gen_display_testdata <baseline.parquet> [<experiment.parquet>]");
        std::process::exit(2);
    }
    write_capture(&args[0], false)?;
    if let Some(exp) = args.get(1) {
        write_capture(exp, true)?;
    }
    Ok(())
}

fn write_capture(path: &str, regression: bool) -> Result<(), Box<dyn std::error::Error>> {
    let n = DURATION_S as usize;

    let timestamps: Vec<u64> = (0..DURATION_S).map(|i| BASE_TS_NS + i * SEC_NS).collect();
    let durations: Vec<u64> = vec![SEC_NS; n];

    // Gauge: floor (higher under regression) + isolated spikes.
    let floor = if regression {
        GAUGE_FLOOR + 50
    } else {
        GAUGE_FLOOR
    };
    let gauge: Vec<i64> = (0..DURATION_S)
        .map(|s| {
            if GAUGE_SPIKE_SECS.contains(&s) {
                GAUGE_SPIKE
            } else {
                floor
            }
        })
        .collect();

    // Counter: cumulative, base rate + a 60 s burst every 900 s.
    let base_rate: u64 = if regression { 80 } else { 50 };
    let mut counter: Vec<u64> = Vec::with_capacity(n);
    let mut acc: u64 = 0;
    for s in 0..DURATION_S {
        let bursting = (s % 900) < 60;
        acc += base_rate + if bursting { 450 } else { 0 };
        counter.push(acc);
    }

    // Histogram: cumulative H2 histogram; each row is the dense bucket slice.
    let mut hist = histogram::Histogram::new(GP, MVP)?;
    let mut hist_rows: Vec<Vec<u64>> = Vec::with_capacity(n);
    for s in 0..DURATION_S {
        add_latency_second(&mut hist, s, regression)?;
        hist_rows.push(hist.as_slice().to_vec());
    }
    let hist_col = list_u64(&hist_rows);

    // ── schema ──────────────────────────────────────────────────────────────
    let gauge_meta = meta(&[("metric_type", "gauge")]);
    let counter_meta = meta(&[("metric_type", "counter")]);
    let hist_meta = meta(&[
        ("metric_type", "histogram"),
        ("grouping_power", &GP.to_string()),
        ("max_value_power", &MVP.to_string()),
    ]);
    let inner = Arc::new(Field::new("item", DataType::UInt64, true));
    let schema = Arc::new(Schema::new(vec![
        Field::new("timestamp", DataType::UInt64, false),
        Field::new("duration", DataType::UInt64, false),
        Field::new("synth_gauge", DataType::Int64, false).with_metadata(gauge_meta),
        Field::new("synth_counter", DataType::UInt64, false).with_metadata(counter_meta),
        Field::new("synth_latency", DataType::List(inner), true).with_metadata(hist_meta),
    ]));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(UInt64Array::from(timestamps)) as ArrayRef,
            Arc::new(UInt64Array::from(durations)),
            Arc::new(Int64Array::from(gauge)),
            Arc::new(UInt64Array::from(counter)),
            hist_col,
        ],
    )?;

    let source = if regression {
        "synth-experiment"
    } else {
        "synth-baseline"
    };
    let kv = vec![
        KeyValue {
            key: "source".into(),
            value: Some(source.into()),
        },
        KeyValue {
            key: "sampling_interval_ms".into(),
            value: Some("1000".into()),
        },
    ];
    let props = WriterProperties::builder()
        .set_key_value_metadata(Some(kv))
        .build();
    let file = std::fs::File::create(path)?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;
    writer.write(&batch)?;
    writer.close()?;
    eprintln!("wrote {path} ({DURATION_S} rows, regression={regression})");
    Ok(())
}

// One second of latency samples: a fixed shape (bulk ~1 ms, small tail) plus
// extra ~100 ms mass at the known spike seconds. Under regression everything is
// shifted ~2×. Deterministic — no RNG — so percentiles are reproducible.
fn add_latency_second(
    h: &mut histogram::Histogram,
    s: u64,
    regression: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let scale = if regression { 2 } else { 1 };
    // (value_ms, count)
    let shape: &[(u64, u64)] = &[(1, 50), (2, 30), (5, 15), (20, 4), (50, 1)];
    for &(v_ms, c) in shape {
        for _ in 0..c {
            h.increment(v_ms * MS * scale)?;
        }
    }
    if LAT_SPIKE_SECS.contains(&s) {
        for _ in 0..10 {
            h.increment(100 * MS * scale)?;
        }
    }
    Ok(())
}

fn list_u64(rows: &[Vec<u64>]) -> ArrayRef {
    Arc::new(ListArray::from_iter_primitive::<UInt64Type, _, _>(
        rows.iter()
            .map(|r| Some(r.iter().map(|&c| Some(c)).collect::<Vec<_>>())),
    ))
}

fn meta(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
