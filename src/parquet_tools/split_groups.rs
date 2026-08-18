//! DEV SCAFFOLDING for the acquisition-groups design
//! (docs/superpowers/specs/2026-08-17-acquisition-groups-design.md).
//! Rewrites a v3 `.rez` into per-acquisition-group tables so the split
//! layout's read cost can be measured before any production writer changes.
//! Grouping is inferred: windowed metrics cohort by agreeing
//! `:window_begin` columns; windowless metrics group by base-metric family.
//! Delete this module once the real grouped writer lands (Stage 3+).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, Int64Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;

use crate::recorder::rez::{segment_writer_props, WALL_OFFSET_COLUMN};
use crate::recorder::rez_sqlite::RezDb;

type Error = Box<dyn std::error::Error>;

pub(crate) enum SplitMode {
    /// Identity re-encode: same columns, one output table per input table.
    /// Produces the provenance-matched wide baseline (same codec/writer path
    /// as the split arm, so the comparison isolates layout, not encoding).
    Wide,
    /// Partition into per-acquisition-group tables.
    Groups,
}

/// True if `cand` never disagrees with the coalesced `begins` on a row
/// where both are present, and they agree on at least one such row.
fn begins_compatible(begins: &[Option<i64>], cand: &Int64Array) -> bool {
    let mut overlap = false;
    for (r, &b) in begins.iter().enumerate() {
        if let Some(bv) = b {
            if cand.is_valid(r) {
                if cand.value(r) != bv {
                    return false;
                }
                overlap = true;
            }
        }
    }
    overlap
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
    if rows == 0 {
        return Ok(Vec::new());
    }

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
    //
    // Cohorting key: a metric's `:window_begin`, not its full window.
    // `Acquisition::begin()` (src/agent/timing.rs) fixes `begin_ns` once per
    // sweep, while each member's `window()` call recomputes `end_ns` at the
    // moment that member is read — so members of one acquisition share
    // `begin` but carry distinct `end`s. Cohorting on the full window would
    // degenerate to one group per column.
    enum Key {
        // Coalesced `:window_begin` values across every member joined so
        // far (row-aligned, `None` where no member has reported yet).
        Windowed(Vec<Option<i64>>),
        Family(String), // base-metric family for windowless metrics
    }
    let mut groups: Vec<(Key, Vec<(usize, String)>)> = Vec::new();
    for (ci, base) in value_cols {
        let windowed = begin_idx
            .get(&base)
            .copied()
            .filter(|&bi| batch.column(bi).null_count() < rows);
        match windowed {
            Some(bi) => {
                let cand = batch
                    .column(bi)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .ok_or("window_begin is not Int64")?;
                // A candidate joins the first group whose coalesced begins
                // agree with it on every row where both are present, and
                // agree on at least one row (an empty overlap is not
                // evidence of shared acquisition).
                let joined = groups.iter().position(|(key, _)| {
                    if let Key::Windowed(begins) = key {
                        begins_compatible(begins, cand)
                    } else {
                        false
                    }
                });
                match joined {
                    Some(gi) => {
                        let (key, members) = &mut groups[gi];
                        if let Key::Windowed(begins) = key {
                            for (r, slot) in begins.iter_mut().enumerate() {
                                if slot.is_none() && cand.is_valid(r) {
                                    *slot = Some(cand.value(r));
                                }
                            }
                        }
                        members.push((ci, base));
                    }
                    None => {
                        let begins: Vec<Option<i64>> = (0..rows)
                            .map(|r| cand.is_valid(r).then(|| cand.value(r)))
                            .collect();
                        groups.push((Key::Windowed(begins), vec![(ci, base)]));
                    }
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
            Key::Windowed(begins) => {
                let first = begins.iter().find_map(|b| *b).unwrap_or(i64::MAX);
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
        if let Key::Windowed(begins) = key {
            fields.push(Field::new("window_begin", DataType::Int64, true));
            arrays.push(Arc::new(Int64Array::from(begins.clone())));
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

/// Copy every recording/segment of a v3 `.rez` through `split_segment` into a
/// fresh v3 file. `Wide` mode produces the provenance-matched baseline.
///
/// `RezDb::create` refuses a path that already exists, so a run that fails
/// partway through must not leave an invalid file behind at `output` — that
/// would turn every retry into a confusing "file exists" instead of the real
/// error. The copy itself lives in `split_rez_body`; this wrapper's only job
/// is to clean up the partial output on that path.
pub(crate) fn split_rez(input: &Path, output: &Path, mode: &SplitMode) -> Result<(), Error> {
    let in_db = RezDb::open(input)?;
    let mut out_db = RezDb::create(output)?;
    let result = split_rez_body(&in_db, &mut out_db, mode);
    if result.is_err() {
        // Drop the connection before unlinking so nothing holds the file
        // open on the way out. Best-effort: a failure to remove here should
        // not shadow the real error.
        drop(out_db);
        let _ = std::fs::remove_file(output);
    }
    result
}

/// The copy loop for `split_rez`, isolated so the wrapper can distinguish
/// "we created `output` and something after that failed" from "we never
/// touched `output` at all" (`RezDb::create` above already ruled out the
/// latter by the time this runs).
///
/// WAL materialization is deliberately NOT implemented here: this is
/// measurement-gate scaffolding for cleanly finalized `rezolus record`
/// output, not the production read path (`rez_reader.rs` does the real
/// recovery/replay). The live-WAL guard below turns "this recording still
/// has unsealed rows" into a loud refusal instead of a silent drop — either a
/// whole quiet sampler (never sealed, so absent from `samplers()`) or the
/// tail of rows past a sampler's last seal.
fn split_rez_body(in_db: &RezDb, out_db: &mut RezDb, mode: &SplitMode) -> Result<(), Error> {
    for rec in in_db.read_recordings()? {
        // `all_samplers`, not `samplers`: the latter only sees samplers with
        // at least one sealed segment, which would silently drop a sampler
        // still inside its first seal period.
        let samplers = in_db.all_samplers(rec.id)?;

        let mut live = Vec::new();
        for sampler in &samplers {
            if !in_db.live_wal(rec.id, sampler)?.is_empty() {
                live.push(sampler.as_str());
            }
        }
        if !live.is_empty() {
            return Err(format!(
                "recording {} has live WAL rows for sampler(s) {}: split-groups only handles \
                 finalized recordings (finalize or snapshot the recording first)",
                rec.id,
                live.join(", ")
            )
            .into());
        }

        let offsets = in_db.read_clock_offsets(rec.id)?;
        // One transaction per recording (not per segment): `insert_segment`
        // commits and fsyncs on its own, and batching every write for this
        // recording — the recording row, its clock offsets, every split
        // segment, and the completion flag — into one `transaction` call
        // both avoids a commit per segment and makes the recording atomic:
        // either all of it lands in the output file, or none of it does.
        out_db.transaction(|tx| {
            let out_id = tx.insert_recording(&rec.meta)?;
            for (ts, off) in &offsets {
                tx.insert_clock_offset(out_id, *ts, *off)?;
            }
            for sampler in &samplers {
                for seg in in_db.read_segments(rec.id, sampler)? {
                    for (group, bytes) in
                        split_segment(&seg.bytes, mode).map_err(|e| e.to_string())?
                    {
                        let table = if group.is_empty() {
                            sampler.clone()
                        } else {
                            format!("{sampler}/{group}")
                        };
                        // `seg.meta` (rows/first_ts/last_ts) is reused
                        // verbatim: valid only because `split_segment`
                        // repartitions columns, never rows. A row-filtering
                        // change there would have to recompute `rows` (and
                        // the timestamps) here too.
                        tx.insert_segment(out_id, &table, seg.seq, &seg.meta, &bytes)?;
                    }
                }
            }
            tx.mark_complete(out_id)?;
            Ok(())
        })?;
    }
    Ok(())
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
        assert_eq!(orig, rt);
    }

    #[test]
    fn value_field_metadata_survives_the_split() {
        let out = split_segment(&fixture_bytes(), &SplitMode::Groups).unwrap();
        let (_, bytes) = out.iter().find(|(n, _)| n == "acq1").unwrap();
        let b = decode(bytes);
        let f = b.schema().field_with_name("2").cloned().unwrap();
        assert_eq!(f.metadata().get("metric").map(String::as_str), Some("beta"));
    }

    /// Same acquisition, disjoint suppression: col "0" is suppressed on row 1,
    /// col "1" is suppressed on row 2, so their `:window_begin` columns have
    /// different null patterns even though every present value agrees. This
    /// is the shape the exact-equality bug fragmented into two singleton
    /// tables; overlap-agreement against a coalesced begin vector must merge
    /// them into one.
    #[test]
    fn cohorts_merge_across_differing_null_patterns() {
        let table = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: vec![1_000_000u64, 2_000_000u64, 3_000_000u64],
            wall_offsets: vec![10, 20, 30],
            columns: vec![
                RezColumn {
                    name: "0".into(),
                    metadata: meta("alpha_a"),
                    values: RezValues::Counter(vec![Some(1), None, Some(3)]),
                    windows: vec![
                        Some(Window::new(999_000, 999_400)),
                        None,
                        Some(Window::new(2_999_000, 2_999_400)),
                    ],
                },
                RezColumn {
                    name: "1".into(),
                    metadata: meta("alpha_b"),
                    values: RezValues::Counter(vec![Some(10), Some(20), None]),
                    windows: vec![
                        Some(Window::new(999_000, 999_500)),
                        Some(Window::new(1_999_000, 1_999_400)),
                        None,
                    ],
                },
            ],
        };
        let bytes = write_table_parquet(&table).expect("fixture encodes");

        let out = split_segment(&bytes, &SplitMode::Groups).unwrap();
        let got: Vec<&str> = out.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(got, vec!["acq0"], "must merge into a single cohort");

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

        // Coalesced begins: -1_000 on every row, and non-null everywhere even
        // though neither member alone reports on all three rows.
        let begin = b.column(2).as_any().downcast_ref::<Int64Array>().unwrap();
        for r in 0..3 {
            assert!(
                begin.is_valid(r),
                "row {r} should be coalesced from a member"
            );
            assert_eq!(begin.value(r), -1_000);
        }

        // Row 0 width is the max across members: 400 (col "0") vs 500 (col "1").
        let width = b.column(3).as_any().downcast_ref::<UInt64Array>().unwrap();
        assert_eq!(width.value(0), 500);
    }

    /// A histogram value column's identity lives on its `:buckets`-suffixed
    /// field, but its `:window_begin`/`:window_width` sidecars are keyed by
    /// the bare column id (same as any other metric type). It must cohort
    /// alongside a counter sharing the same acquisition.
    #[test]
    fn histogram_column_cohorts_by_its_sidecar() {
        let wa = vec![
            Some(Window::new(999_000, 999_400)),
            Some(Window::new(1_999_000, 1_999_400)),
        ];
        let mut hist_a = histogram::Histogram::new(3, 64).unwrap();
        hist_a.increment(5).unwrap();
        let mut hist_b = histogram::Histogram::new(3, 64).unwrap();
        hist_b.increment(50).unwrap();

        let table = RezTable {
            sampler: "cpu_usage".to_string(),
            timestamps: vec![1_000_000u64, 2_000_000u64],
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
                    metadata: meta("alpha_hist"),
                    values: RezValues::Histogram(vec![Some(hist_a), Some(hist_b)]),
                    windows: wa,
                },
            ],
        };
        let bytes = write_table_parquet(&table).expect("fixture encodes");

        let out = split_segment(&bytes, &SplitMode::Groups).unwrap();
        assert_eq!(out.len(), 1);
        let (name, bytes) = &out[0];
        assert_eq!(name, "acq0");
        let b = decode(bytes);
        assert_eq!(
            names(&b),
            vec![
                "timestamp",
                WALL_OFFSET_COLUMN,
                "window_begin",
                "window_width",
                "0",
                "1:buckets"
            ]
        );
        let f0 = b.schema().field_with_name("0").cloned().unwrap();
        assert_eq!(
            f0.metadata().get("metric").map(String::as_str),
            Some("alpha_a")
        );
        let f1 = b.schema().field_with_name("1:buckets").cloned().unwrap();
        assert_eq!(
            f1.metadata().get("metric").map(String::as_str),
            Some("alpha_hist")
        );
    }

    #[test]
    fn split_rez_rewrites_a_v3_container_into_group_tables() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("wide.rez");
        let output = dir.path().join("split.rez");

        // Build a minimal v3 file: one recording, one sampler, one segment,
        // one clock offset — everything sealed, nothing left in the WAL.
        let mut db = RezDb::create(&input).unwrap();
        let rec_id = db
            .insert_recording(&crate::recorder::rez_sqlite::RecordingMeta {
                labels: [("source".to_string(), "rezolus".to_string())].into(),
                metadata: Default::default(),
                clock_anchor_wall_ns: 1_000,
            })
            .unwrap();
        let seg = fixture_bytes();
        db.insert_segment(
            rec_id,
            "cpu_usage",
            0,
            &crate::recorder::rez_sqlite::SegmentMeta {
                rows: 2,
                first_ts: 1_000_000,
                last_ts: 2_000_000,
            },
            &seg,
        )
        .unwrap();
        db.transaction(|tx| tx.insert_clock_offset(rec_id, 1_500_000, -250))
            .unwrap();
        db.mark_complete(rec_id).unwrap();
        drop(db);

        split_rez(&input, &output, &SplitMode::Groups).unwrap();

        let out_db = RezDb::open(&output).unwrap();
        let recs = out_db.read_recordings().unwrap();
        assert_eq!(recs.len(), 1);
        assert!(recs[0].complete);
        let mut tables = out_db.samplers(recs[0].id).unwrap();
        tables.sort();
        assert_eq!(
            tables,
            vec![
                "cpu_usage/acq0",
                "cpu_usage/acq1",
                "cpu_usage/f_gamma_total"
            ]
        );
        let segs = out_db.read_segments(recs[0].id, "cpu_usage/acq0").unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].seq, 0);
        assert_eq!(segs[0].meta.rows, 2);

        // Clock offsets round-trip unchanged.
        let in_db = RezDb::open(&input).unwrap();
        assert_eq!(
            out_db.read_clock_offsets(recs[0].id).unwrap(),
            in_db.read_clock_offsets(rec_id).unwrap(),
        );
    }

    #[test]
    fn split_rez_wide_mode_keeps_table_names() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("wide.rez");
        let output = dir.path().join("wide_out.rez");

        let mut db = RezDb::create(&input).unwrap();
        let rec_id = db
            .insert_recording(&crate::recorder::rez_sqlite::RecordingMeta {
                labels: [("source".to_string(), "rezolus".to_string())].into(),
                metadata: Default::default(),
                clock_anchor_wall_ns: 1_000,
            })
            .unwrap();
        db.insert_segment(
            rec_id,
            "cpu_usage",
            0,
            &crate::recorder::rez_sqlite::SegmentMeta {
                rows: 2,
                first_ts: 1_000_000,
                last_ts: 2_000_000,
            },
            &fixture_bytes(),
        )
        .unwrap();
        db.mark_complete(rec_id).unwrap();
        drop(db);

        split_rez(&input, &output, &SplitMode::Wide).unwrap();

        let out_db = RezDb::open(&output).unwrap();
        let recs = out_db.read_recordings().unwrap();
        assert_eq!(recs.len(), 1);
        let tables = out_db.samplers(recs[0].id).unwrap();
        assert_eq!(tables, vec!["cpu_usage"]);
        let segs = out_db.read_segments(recs[0].id, "cpu_usage").unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].meta.rows, 2);
    }

    #[test]
    fn split_rez_refuses_live_wal() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("wide.rez");
        let output = dir.path().join("split.rez");

        let mut db = RezDb::create(&input).unwrap();
        let rec_id = db
            .insert_recording(&crate::recorder::rez_sqlite::RecordingMeta {
                labels: [("source".to_string(), "rezolus".to_string())].into(),
                metadata: Default::default(),
                clock_anchor_wall_ns: 1_000,
            })
            .unwrap();
        // A WAL row for a sampler that has never sealed a segment, left
        // unsealed on purpose — the "quiet table, first seal period" case
        // `all_samplers`/`live_wal` exist to surface.
        db.insert_wal_rows(
            rec_id,
            &[crate::recorder::rez_sqlite::WalRow {
                sampler: "scheduler".to_string(),
                ts: 1_000_000,
                wall_offset: 10,
                row: b"placeholder".to_vec(),
            }],
        )
        .unwrap();
        db.mark_complete(rec_id).unwrap();
        drop(db);

        let err = split_rez(&input, &output, &SplitMode::Groups).unwrap_err();
        assert!(err.to_string().contains("scheduler"));
        // Fix 3: a failed run must not leave a partial output file behind.
        assert!(!output.exists());
    }
}
