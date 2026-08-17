//! DEV SCAFFOLDING for the acquisition-groups design
//! (docs/superpowers/specs/2026-08-17-acquisition-groups-design.md).
//! Rewrites a v3 `.rez` into per-acquisition-group tables so the split
//! layout's read cost can be measured before any production writer changes.
//! Grouping is inferred: windowed metrics cohort by identical
//! `:window_begin` columns; windowless metrics group by base-metric family.
//! Delete this module once the real grouped writer lands (Stage 3+).
//!
//! `#[allow(dead_code)]` below: nothing calls into this module yet — Task 3
//! adds `split_rez`, which drives `split_segment` against a real `.rez` file.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, Int64Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;

use crate::recorder::rez::{segment_writer_props, WALL_OFFSET_COLUMN};
// NOTE: Task 3 restores `use crate::recorder::rez_sqlite::RezDb;` and
// `use std::path::Path;` when it adds `split_rez`, which drives this module
// against a real .rez file.

type Error = Box<dyn std::error::Error>;

pub(crate) enum SplitMode {
    /// Identity re-encode: same columns, one output table per input table.
    /// Produces the provenance-matched wide baseline (same codec/writer path
    /// as the split arm, so the comparison isolates layout, not encoding).
    Wide,
    /// Partition into per-acquisition-group tables.
    Groups,
}

/// `"5:window_begin"` → base `"5"`; `"5:buckets"` → base `"5"`; `"5"` → `"5"`.
fn sidecar_base(name: &str) -> &str {
    for suffix in [":window_begin", ":window_width", ":buckets"] {
        if let Some(base) = name.strip_suffix(suffix) {
            return base;
        }
    }
    name
}

/// Split one wide segment (parquet bytes) into `(group_name, parquet bytes)`
/// tables. `Wide` mode returns a single `("", bytes)` identity re-encode.
pub(crate) fn split_segment(
    bytes: &[u8],
    mode: &SplitMode,
) -> Result<Vec<(String, Vec<u8>)>, Error> {
    let reader =
        ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes.to_vec()))?.build()?;
    let mut batches = Vec::new();
    for b in reader {
        batches.push(b?);
    }
    if batches.is_empty() {
        return Ok(Vec::new());
    }
    let schema = batches[0].schema();
    let batch = arrow::compute::concat_batches(&schema, &batches)?;
    let rows = batch.num_rows();

    // Classify columns.
    let mut ts_idx: Option<usize> = None;
    let mut wall_idx: Option<usize> = None;
    let mut begin_idx: HashMap<String, usize> = HashMap::new(); // base -> begin col
    let mut width_idx: HashMap<String, usize> = HashMap::new(); // base -> width col
    let mut value_cols: Vec<(usize, String)> = Vec::new(); // (col idx, base)
    for (i, f) in schema.fields().iter().enumerate() {
        let n = f.name().as_str();
        if n == "timestamp" {
            ts_idx = Some(i);
        } else if n == WALL_OFFSET_COLUMN {
            wall_idx = Some(i);
        } else if n.ends_with(":window_begin") {
            begin_idx.insert(sidecar_base(n).to_string(), i);
        } else if n.ends_with(":window_width") {
            width_idx.insert(sidecar_base(n).to_string(), i);
        } else {
            value_cols.push((i, sidecar_base(n).to_string()));
        }
    }
    let ts_idx = ts_idx.ok_or("segment has no timestamp column")?;

    if matches!(mode, SplitMode::Wide) {
        let buf = encode(schema.as_ref().clone(), batch.columns().to_vec())?;
        return Ok(vec![(String::new(), buf)]);
    }

    // Partition value columns into groups.
    enum Key {
        Windowed(usize), // representative begin column index
        Family(String),  // base-metric family for windowless metrics
    }
    let mut groups: Vec<(Key, Vec<(usize, String)>)> = Vec::new();
    for (ci, base) in value_cols {
        let windowed = begin_idx
            .get(&base)
            .copied()
            .filter(|&bi| batch.column(bi).null_count() < rows);
        match windowed {
            Some(bi) => {
                let existing = groups.iter_mut().find(|(k, _)| {
                    matches!(k, Key::Windowed(ri)
                        if batch.column(*ri).to_data() == batch.column(bi).to_data())
                });
                match existing {
                    Some((_, members)) => members.push((ci, base)),
                    None => groups.push((Key::Windowed(bi), vec![(ci, base)])),
                }
            }
            None => {
                let fam = schema
                    .field(ci)
                    .metadata()
                    .get("metric")
                    .cloned()
                    .unwrap_or_else(|| "misc".to_string());
                let existing = groups
                    .iter_mut()
                    .find(|(k, _)| matches!(k, Key::Family(f) if *f == fam));
                match existing {
                    Some((_, members)) => members.push((ci, base)),
                    None => groups.push((Key::Family(fam), vec![(ci, base)])),
                }
            }
        }
    }

    // Deterministic names: windowed groups ranked by first non-null begin
    // (acquisition order within the tick), then families ranked by name.
    let mut windowed: Vec<(i64, usize)> = Vec::new(); // (first begin, groups idx)
    let mut families: Vec<(String, usize)> = Vec::new();
    for (gi, (key, _)) in groups.iter().enumerate() {
        match key {
            Key::Windowed(bi) => {
                let a = batch
                    .column(*bi)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .ok_or("window_begin is not Int64")?;
                let first = (0..a.len())
                    .find(|&r| a.is_valid(r))
                    .map(|r| a.value(r))
                    .unwrap_or(i64::MAX);
                windowed.push((first, gi));
            }
            Key::Family(f) => families.push((f.clone(), gi)),
        }
    }
    windowed.sort();
    families.sort();

    let mut out = Vec::new();
    let named: Vec<(String, usize)> = windowed
        .into_iter()
        .enumerate()
        .map(|(rank, (_, gi))| (format!("acq{rank}"), gi))
        .chain(families.into_iter().map(|(f, gi)| (format!("f_{f}"), gi)))
        .collect();
    for (name, gi) in named {
        let (key, members) = &groups[gi];
        let mut fields: Vec<Field> = vec![schema.field(ts_idx).clone()];
        let mut arrays: Vec<ArrayRef> = vec![Arc::clone(batch.column(ts_idx))];
        if let Some(wi) = wall_idx {
            fields.push(schema.field(wi).clone());
            arrays.push(Arc::clone(batch.column(wi)));
        }
        if let Key::Windowed(bi) = key {
            fields.push(Field::new("window_begin", DataType::Int64, true));
            arrays.push(Arc::clone(batch.column(*bi)));
            // Table window width: per-row max over the members' widths.
            let mut width: Vec<Option<u64>> = vec![None; rows];
            for (_, base) in members {
                if let Some(&wi) = width_idx.get(base) {
                    let a = batch
                        .column(wi)
                        .as_any()
                        .downcast_ref::<UInt64Array>()
                        .ok_or("window_width is not UInt64")?;
                    for (r, slot) in width.iter_mut().enumerate() {
                        if a.is_valid(r) {
                            let v = a.value(r);
                            *slot = Some(slot.map_or(v, |w| w.max(v)));
                        }
                    }
                }
            }
            fields.push(Field::new("window_width", DataType::UInt64, true));
            arrays.push(Arc::new(UInt64Array::from(width)));
        }
        for (ci, _) in members {
            fields.push(schema.field(*ci).clone());
            arrays.push(Arc::clone(batch.column(*ci)));
        }
        out.push((name, encode(Schema::new(fields), arrays)?));
    }
    Ok(out)
}

fn encode(schema: Schema, arrays: Vec<ArrayRef>) -> Result<Vec<u8>, Error> {
    let schema = Arc::new(schema);
    let batch = RecordBatch::try_new(Arc::clone(&schema), arrays)?;
    let mut buf = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut buf, schema, Some(segment_writer_props()))?;
    writer.write(&batch)?;
    writer.close()?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::rez::{write_table_parquet, RezColumn, RezTable, RezValues};
    use metriken::Window;
    use std::collections::HashMap as Meta;

    fn meta(metric: &str) -> Meta<String, String> {
        [
            ("metric".to_string(), metric.to_string()),
            ("metric_type".to_string(), "counter".to_string()),
        ]
        .into()
    }

    /// Two ticks. Cohort A (two counters, shared windows), cohort B (one
    /// counter, later windows), family gamma (two windowless counters with the
    /// same base metric name).
    fn fixture_bytes() -> Vec<u8> {
        let ts = vec![1_000_000u64, 2_000_000u64];
        let wa = vec![
            Some(Window::new(999_000, 999_400)),
            Some(Window::new(1_999_000, 1_999_400)),
        ];
        let wb = vec![
            Some(Window::new(999_700, 999_900)),
            Some(Window::new(1_999_700, 1_999_900)),
        ];
        let table = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: ts,
            wall_offsets: vec![10, 20],
            columns: vec![
                RezColumn {
                    name: "0".into(),
                    metadata: meta("alpha_a"),
                    values: RezValues::Counter(vec![Some(1), Some(2)]),
                    windows: wa.clone(),
                },
                RezColumn {
                    name: "1".into(),
                    metadata: meta("alpha_b"),
                    values: RezValues::Counter(vec![Some(3), Some(4)]),
                    windows: wa,
                },
                RezColumn {
                    name: "2".into(),
                    metadata: meta("beta"),
                    values: RezValues::Counter(vec![Some(5), Some(6)]),
                    windows: wb,
                },
                RezColumn {
                    name: "3x0".into(),
                    metadata: meta("gamma_total"),
                    values: RezValues::Counter(vec![Some(7), Some(8)]),
                    windows: vec![None, None],
                },
                RezColumn {
                    name: "3x1".into(),
                    metadata: meta("gamma_total"),
                    values: RezValues::Counter(vec![Some(9), None]),
                    windows: vec![None, None],
                },
            ],
        };
        write_table_parquet(&table).expect("fixture encodes")
    }

    fn decode(bytes: &[u8]) -> RecordBatch {
        let reader = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes.to_vec()))
            .unwrap()
            .build()
            .unwrap();
        let batches: Vec<_> = reader.map(Result::unwrap).collect();
        arrow::compute::concat_batches(&batches[0].schema(), &batches).unwrap()
    }

    fn names(batch: &RecordBatch) -> Vec<String> {
        batch
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }

    #[test]
    fn sidecar_base_strips_known_suffixes() {
        assert_eq!(sidecar_base("5:window_begin"), "5");
        assert_eq!(sidecar_base("5:window_width"), "5");
        assert_eq!(sidecar_base("5:buckets"), "5");
        assert_eq!(sidecar_base("5x3"), "5x3");
    }

    #[test]
    fn groups_mode_partitions_cohorts_and_families() {
        let out = split_segment(&fixture_bytes(), &SplitMode::Groups).unwrap();
        let mut got: Vec<&str> = out.iter().map(|(n, _)| n.as_str()).collect();
        got.sort();
        assert_eq!(got, vec!["acq0", "acq1", "f_gamma_total"]);
    }

    #[test]
    fn windowed_group_has_table_level_window_and_members() {
        let out = split_segment(&fixture_bytes(), &SplitMode::Groups).unwrap();
        let (_, bytes) = out.iter().find(|(n, _)| n == "acq0").unwrap();
        let b = decode(bytes);
        assert_eq!(
            names(&b),
            vec![
                "timestamp",
                WALL_OFFSET_COLUMN,
                "window_begin",
                "window_width",
                "0",
                "1"
            ]
        );
        // Stored begins are offsets from the row timestamp: 999_000 - 1_000_000.
        let begin = b.column(2).as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(begin.value(0), -1_000);
        // Width is the max across members; both members share wa, width 400.
        let width = b.column(3).as_any().downcast_ref::<UInt64Array>().unwrap();
        assert_eq!(width.value(0), 400);
        // Values preserved.
        let v0 = b.column(4).as_any().downcast_ref::<UInt64Array>().unwrap();
        assert_eq!((v0.value(0), v0.value(1)), (1, 2));
    }

    #[test]
    fn family_group_has_no_window_columns_and_keeps_nulls() {
        let out = split_segment(&fixture_bytes(), &SplitMode::Groups).unwrap();
        let (_, bytes) = out.iter().find(|(n, _)| n == "f_gamma_total").unwrap();
        let b = decode(bytes);
        assert_eq!(
            names(&b),
            vec!["timestamp", WALL_OFFSET_COLUMN, "3x0", "3x1"]
        );
        let v = b.column(3).as_any().downcast_ref::<UInt64Array>().unwrap();
        assert!(v.is_valid(0) && !v.is_valid(1));
    }

    #[test]
    fn wide_mode_is_an_identity_reencode() {
        let out = split_segment(&fixture_bytes(), &SplitMode::Wide).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "");
        let orig = decode(&fixture_bytes());
        let rt = decode(&out[0].1);
        assert_eq!(names(&orig), names(&rt));
        assert_eq!(orig.num_rows(), rt.num_rows());
    }

    #[test]
    fn value_field_metadata_survives_the_split() {
        let out = split_segment(&fixture_bytes(), &SplitMode::Groups).unwrap();
        let (_, bytes) = out.iter().find(|(n, _)| n == "acq1").unwrap();
        let b = decode(bytes);
        let f = b.schema().field_with_name("2").cloned().unwrap();
        assert_eq!(f.metadata().get("metric").map(String::as_str), Some("beta"));
    }
}
