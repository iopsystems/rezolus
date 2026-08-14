use clap::ArgMatches;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::read_parquet_footer;
use crate::recorder::rez::RezFormat;
use crate::recorder::rez_sqlite::RezDb;

pub(super) fn run(args: &ArgMatches) -> Result<(), Box<dyn std::error::Error>> {
    let input = args.get_one::<PathBuf>("input").unwrap();
    let schema_only = args.get_flag("schema");
    let geometry_only = args.get_flag("geometry");
    let file_only = args.get_flag("file");
    let field_key = args.get_one::<String>("field");
    let json = args.get_flag("json");

    // `.rez` archives have no single parquet footer — describe the catalog.
    // Dispatch is by CONTENT, not extension, and covers both containers: a v3
    // `.rez` is a SQLite file, so `is_rez_path` (a tar sniff) reports false for
    // it and would have sent it to the parquet reader.
    if crate::recorder::rez::detect_rez_format(input).unwrap_or(RezFormat::NotRez)
        != RezFormat::NotRez
    {
        return describe_rez(input, json);
    }

    let (metadata, schema, _) = read_parquet_footer(input)?;

    // --field=KEY: print the raw value of a single file-level metadata key
    if let Some(key) = field_key {
        let kv = metadata.file_metadata().key_value_metadata();
        let value = kv
            .and_then(|entries| entries.iter().find(|e| e.key == *key))
            .and_then(|e| e.value.as_deref());

        match value {
            Some(v) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(v) {
                    println!("{}", serde_json::to_string_pretty(&parsed)?);
                } else {
                    println!("{v}");
                }
            }
            None => {
                return Err(format!("no file-level metadata key {:?}", key).into());
            }
        }
        return Ok(());
    }

    if json {
        return run_json(args, &metadata, &schema);
    }

    let show_all = !schema_only && !geometry_only && !file_only;

    if show_all || file_only {
        if let Some(kv) = metadata.file_metadata().key_value_metadata() {
            println!("File Metadata:");
            for entry in kv {
                if entry.key == "ARROW:schema" {
                    continue;
                }
                let value = entry.value.as_deref().unwrap_or("");
                if value.len() > 120 {
                    println!("  {}: {}...", entry.key, &value[..120]);
                } else {
                    println!("  {}: {}", entry.key, value);
                }
            }
        } else {
            println!("File Metadata: (none)");
        }
        println!();
    }

    // Geometry: logical table shape + row group layout
    if show_all || geometry_only {
        let row_groups = metadata.row_groups();
        let total_rows: i64 = row_groups.iter().map(|rg| rg.num_rows()).sum();
        let total_bytes: i64 = row_groups.iter().map(|rg| rg.total_byte_size()).sum();
        let num_columns = schema.fields().len();

        println!("Geometry:");
        println!(
            "  Logical Table: {} columns x {} rows",
            num_columns, total_rows
        );
        println!();

        let idx_w = row_groups.len().to_string().len().max(5);
        let rows_w = row_groups
            .iter()
            .map(|rg| rg.num_rows().to_string().len())
            .max()
            .unwrap_or(4)
            .max(4);
        let bytes_w = row_groups
            .iter()
            .map(|rg| format_bytes(rg.total_byte_size()).len())
            .max()
            .unwrap_or(5)
            .max(5);

        println!(
            "  {:<idx_w$} | {:>rows_w$} | {:>bytes_w$}",
            "Group", "Rows", "Bytes",
        );
        println!("  {:-<idx_w$}-+-{:->rows_w$}-+-{:->bytes_w$}", "", "", "",);
        for (i, rg) in row_groups.iter().enumerate() {
            println!(
                "  {:<idx_w$} | {:>rows_w$} | {:>bytes_w$}",
                i,
                rg.num_rows(),
                format_bytes(rg.total_byte_size()),
            );
        }
        println!("  {:-<idx_w$}-+-{:->rows_w$}-+-{:->bytes_w$}", "", "", "",);
        println!(
            "  {:<idx_w$} | {:>rows_w$} | {:>bytes_w$}",
            "Total",
            total_rows,
            format_bytes(total_bytes),
        );

        if show_all {
            println!();
        }
    }

    // Column-level metadata (schema) - human-readable table
    if show_all || schema_only {
        struct SchemaRow {
            name: String,
            dtype: String,
            metric_type: String,
            other_meta: String,
        }

        let mut rows: Vec<SchemaRow> = Vec::new();
        let mut name_w = 4; // "Name"
        let mut type_w = 4; // "Type"
        let mut mt_w = 11; // "Metric Type"

        for field in schema.fields() {
            let name = field.name().clone();
            let dtype = format_data_type(field.data_type());
            let meta = field.metadata();

            let metric_type = meta.get("metric_type").cloned().unwrap_or_default();

            let other_meta = meta
                .iter()
                .filter(|(k, _)| *k != "metric_type")
                .map(|(k, v)| {
                    if v.len() > 60 {
                        format!("{k}={{...}}")
                    } else {
                        format!("{k}={v}")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");

            name_w = name_w.max(name.len());
            type_w = type_w.max(dtype.len());
            mt_w = mt_w.max(metric_type.len());

            rows.push(SchemaRow {
                name,
                dtype,
                metric_type,
                other_meta,
            });
        }

        println!("Schema ({} fields):", schema.fields().len());
        println!(
            "  {:<name_w$} | {:<type_w$} | {:<mt_w$} | Other Metadata",
            "Name", "Type", "Metric Type",
        );
        println!(
            "  {:-<name_w$}-+-{:-<type_w$}-+-{:-<mt_w$}-+---------------",
            "", "", "",
        );
        for row in &rows {
            println!(
                "  {:<name_w$} | {:<type_w$} | {:<mt_w$} | {}",
                row.name, row.dtype, row.metric_type, row.other_meta,
            );
        }
    }

    Ok(())
}

fn run_json(
    args: &ArgMatches,
    metadata: &parquet::file::metadata::ParquetMetaData,
    schema: &arrow::datatypes::SchemaRef,
) -> Result<(), Box<dyn std::error::Error>> {
    let schema_only = args.get_flag("schema");
    let geometry_only = args.get_flag("geometry");
    let file_only = args.get_flag("file");
    let show_all = !schema_only && !geometry_only && !file_only;

    let mut out = serde_json::Map::new();

    if show_all || file_only {
        let mut file_meta = serde_json::Map::new();
        if let Some(kv) = metadata.file_metadata().key_value_metadata() {
            for entry in kv {
                if entry.key == "ARROW:schema" {
                    continue;
                }
                let value = entry.value.as_deref().unwrap_or("");
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(value) {
                    file_meta.insert(entry.key.clone(), parsed);
                } else {
                    file_meta.insert(
                        entry.key.clone(),
                        serde_json::Value::String(value.to_string()),
                    );
                }
            }
        }
        out.insert(
            "file_metadata".to_string(),
            serde_json::Value::Object(file_meta),
        );
    }

    if show_all || geometry_only {
        let row_groups = metadata.row_groups();
        let total_rows: i64 = row_groups.iter().map(|rg| rg.num_rows()).sum();

        let rg_details: Vec<serde_json::Value> = row_groups
            .iter()
            .enumerate()
            .map(|(i, rg)| {
                serde_json::json!({
                    "index": i,
                    "num_rows": rg.num_rows(),
                    "total_byte_size": rg.total_byte_size(),
                })
            })
            .collect();

        out.insert(
            "geometry".to_string(),
            serde_json::json!({
                "num_columns": schema.fields().len(),
                "num_rows": total_rows,
                "row_groups": rg_details,
            }),
        );
    }

    if show_all || schema_only {
        let fields: Vec<serde_json::Value> = schema
            .fields()
            .iter()
            .map(|field| {
                let mut f = serde_json::Map::new();
                f.insert(
                    "name".to_string(),
                    serde_json::Value::String(field.name().clone()),
                );
                f.insert(
                    "type".to_string(),
                    serde_json::Value::String(format!("{}", field.data_type())),
                );
                f.insert(
                    "nullable".to_string(),
                    serde_json::Value::Bool(field.is_nullable()),
                );
                if !field.metadata().is_empty() {
                    let meta: serde_json::Map<String, serde_json::Value> = field
                        .metadata()
                        .iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect();
                    f.insert("metadata".to_string(), serde_json::Value::Object(meta));
                }
                serde_json::Value::Object(f)
            })
            .collect();
        out.insert("schema".to_string(), serde_json::Value::Array(fields));
    }

    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// Human-friendly data type string. Simplifies `List(Field { name: "item",
/// data_type: UInt64, ... })` to just `List<UInt64>`.
fn format_data_type(dt: &arrow::datatypes::DataType) -> String {
    use arrow::datatypes::DataType;
    match dt {
        DataType::List(f) => format!("List<{}>", format_data_type(f.data_type())),
        DataType::LargeList(f) => format!("LargeList<{}>", format_data_type(f.data_type())),
        other => format!("{other}"),
    }
}

fn format_bytes(bytes: i64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.1} KiB", b / KIB)
    } else {
        format!("{} B", bytes)
    }
}

/// Describe a `.rez` archive (the `metadata` command; `.rez` has no single
/// parquet footer, so we summarize the recordings/tables index).
///
/// Both containers land here: a v2/v1 tar manifest, or a v3 SQLite catalog.
fn describe_rez(path: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if !json {
        print!("{}", describe_rez_string_at(path)?);
        return Ok(());
    }
    match crate::recorder::rez::detect_rez_format(path)? {
        RezFormat::V3Sqlite => {
            let json = v3_json(&read_v3_summary(path)?);
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        _ => {
            let (manifest, _tables) = crate::recorder::rez::read_archive_bytes(path)?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
    }
    Ok(())
}

/// The human-readable summary of whichever container `path` holds.
///
/// Dispatch is by CONTENT: a v3 `.rez` is a SQLite file whose catalog answers
/// every question here, a v1/v2 `.rez` is a tar whose `manifest.json` does.
pub(crate) fn describe_rez_string_at(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    match crate::recorder::rez::detect_rez_format(path)? {
        RezFormat::V3Sqlite => Ok(describe_v3_string(&read_v3_summary(path)?)),
        _ => Ok(describe_rez_string(
            &crate::recorder::rez::read_archive_bytes(path)?.0,
        )),
    }
}

/// One v3 table, summarized from the catalog: no segment BLOB is read, so this
/// costs the same on a 7 MB archive and a 605 MB one.
struct V3Table {
    sampler: String,
    /// Every row a reader will see: the sealed segments' rows plus the live WAL
    /// tail, which `RezReader` materializes as the newest segment.
    rows: u64,
    segments: u64,
    /// Rows committed per tick but not yet in a segment — recoverable, and the
    /// property v2 has no equivalent of. Non-zero here is the normal state of a
    /// recording that was killed, and for a quiet table it may be *all* its rows.
    live_wal_rows: u64,
    cadence_ns: Option<u64>,
}

/// One v3 recording, summarized from the catalog.
struct V3Recording {
    /// v3 stores no `dir`: a recording is a row, not a tar directory. The
    /// display name is derived the same way `dir` itself was derived —
    /// `recording_dir_slug(&labels)` — so the viewer's per-capture name
    /// (`rez_reader.rs`, which made the same choice) and this line agree.
    name: String,
    labels: BTreeMap<String, String>,
    metadata: BTreeMap<String, String>,
    complete: bool,
    clock_anchor_wall_ns: u64,
    clock_offsets: Vec<(u64, i64)>,
    tables: Vec<V3Table>,
}

/// Summarize a v3 `.rez` from its catalog rows alone.
fn read_v3_summary(path: &Path) -> Result<Vec<V3Recording>, String> {
    let db = RezDb::open(path)?;
    let mut out = Vec::new();
    for rec in db.read_recordings()? {
        let mut tables = Vec::new();
        // `all_samplers`, NOT `samplers`: the latter sees only `segments`, so a
        // table still inside its first seal period — 16 of 26 in the fleet
        // measurement this container exists for — would be missing from the
        // listing entirely, which is precisely the data v3 keeps.
        for sampler in db.all_samplers(rec.id)? {
            let (segments, sealed) = db.segment_span(rec.id, &sampler)?;
            let live = db.live_wal_span(rec.id, &sampler)?;
            let rows = sealed.rows + live.rows;
            let first = min_opt(sealed.first_ts, live.first_ts);
            let last = max_opt(sealed.last_ts, live.last_ts);
            tables.push(V3Table {
                sampler,
                rows,
                segments,
                live_wal_rows: live.rows,
                // What `cadence_hint` reports for the concatenated table: the
                // mean row interval, `None` under 2 rows. Computed from the
                // catalog's span rather than from row timestamps, which live
                // inside the segment BLOBs.
                cadence_ns: match (first, last) {
                    (Some(f), Some(l)) if rows >= 2 => Some(l.saturating_sub(f) / (rows - 1)),
                    _ => None,
                },
            });
        }
        out.push(V3Recording {
            name: crate::recorder::rez::recording_dir_slug(&rec.meta.labels),
            labels: rec.meta.labels,
            metadata: rec.meta.metadata,
            complete: rec.complete,
            clock_anchor_wall_ns: rec.meta.clock_anchor_wall_ns,
            clock_offsets: db.read_clock_offsets(rec.id)?,
            tables,
        })
    }
    Ok(out)
}

fn min_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (x, y) => x.or(y),
    }
}

fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (x, y) => x.or(y),
    }
}

/// Human-readable summary of a v3 `.rez`, deliberately shaped like the v2 one:
/// same recording/table lines, same clock line, with the container named and
/// the live WAL depth added.
fn describe_v3_string(recordings: &[V3Recording]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(
        out,
        ".rez archive v3 (sqlite) — {} recording(s)",
        recordings.len()
    );
    for rec in recordings {
        let labels: Vec<String> = rec.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _ = writeln!(out, "  recording {} [{}]", rec.name, labels.join(", "));
        // Deliberately NOT v2's wording. v2 recovered a `.partial` up to its
        // last checkpoint, and everything after it was gone; v3 commits every
        // tick to the WAL, so the loss is bounded by one sampling interval for
        // every table — including one that never sealed a segment.
        if !rec.complete {
            let _ = writeln!(
                out,
                "    ! not cleanly finalized — recovered from its write-ahead log; \
                 at most the final tick is missing"
            );
        }
        if let Some(line) = clock_line_of(Some(rec.clock_anchor_wall_ns), &rec.clock_offsets) {
            let _ = writeln!(out, "    {line}");
        }
        for t in &rec.tables {
            let cadence = t
                .cadence_ns
                .map(|ns| format!("{:.3}s", ns as f64 / 1e9))
                .unwrap_or_else(|| "—".to_string());
            // Segment counts are worth printing whenever they are not the
            // unremarkable single segment — including 0, which is a table whose
            // every row is still in the WAL.
            let detail = match (t.segments, t.live_wal_rows) {
                (_, 0) if t.segments == 1 => String::new(),
                (n, 0) => format!("  ({n} segments)"),
                (n, live) => format!("  ({n} segments, {live} unsealed in WAL)"),
            };
            let _ = writeln!(
                out,
                "    {:<24} {:>5} rows  ~{}{}",
                t.sampler, t.rows, cadence, detail
            );
        }
    }
    out
}

/// `--json` for a v3 archive: the same facts the text form renders. v2 dumps
/// its manifest verbatim; v3 has no manifest document, so this is its analogue.
fn v3_json(recordings: &[V3Recording]) -> serde_json::Value {
    serde_json::json!({
        "version": 3,
        "container": "sqlite",
        "recordings": recordings.iter().map(|rec| serde_json::json!({
            "name": rec.name,
            "labels": rec.labels,
            "metadata": rec.metadata,
            "complete": rec.complete,
            "clock_anchor_wall_ns": rec.clock_anchor_wall_ns,
            "clock_offsets": rec.clock_offsets,
            "tables": rec.tables.iter().map(|t| serde_json::json!({
                "sampler": t.sampler,
                "rows": t.rows,
                "segments": t.segments,
                "live_wal_rows": t.live_wal_rows,
                "cadence_ns": t.cadence_ns,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// The recording's clock line: the wall-clock anchor its row timestamps are
/// offset from, plus the newest drift observation. `None` for a v1 recording,
/// which carries neither field.
///
/// The observation reported is the newest by *timestamp*, not the last element:
/// the series is a per-batch maximum, so an age-sealed slow sampler can append
/// a timestamp older than one a fast-sampler batch already contributed.
fn clock_line(rec: &crate::recorder::rez::RezRecording) -> Option<String> {
    clock_line_of(rec.clock_anchor_wall_ns, &rec.clock_offsets)
}

/// Shared by both containers: v2 reads these two fields out of the manifest,
/// v3 out of `recordings.clock_anchor_wall_ns` and the `clock_offsets` rows —
/// in both cases without decoding a segment.
fn clock_line_of(anchor: Option<u64>, offsets: &[(u64, i64)]) -> Option<String> {
    let anchor = anchor?;
    let when = i64::try_from(anchor)
        .map(|ns| {
            chrono::DateTime::from_timestamp_nanos(ns)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .unwrap_or_else(|_| format!("{anchor}ns"));
    let mut line = format!("clock anchor {when}");
    if let Some(&(_, offset)) = offsets.iter().max_by_key(|&&(ts, _)| ts) {
        let _ = std::fmt::Write::write_fmt(
            &mut line,
            format_args!(
                ", latest offset {:+.3}ms ({} observations)",
                offset as f64 / 1e6,
                offsets.len()
            ),
        );
    }
    Some(line)
}

/// Human-readable summary of a `.rez` manifest: recordings, their labels, clock
/// anchor/drift, and each per-sampler table's row count, observed cadence and
/// segment count.
fn describe_rez_string(manifest: &crate::recorder::rez::RezManifest) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(
        out,
        ".rez archive v{} — {} recording(s)",
        manifest.version,
        manifest.recordings.len()
    );
    for rec in &manifest.recordings {
        let labels: Vec<String> = rec.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _ = writeln!(out, "  recording {} [{}]", rec.dir, labels.join(", "));
        // Only meaningful from v2 on: v1 predates unclean-kill recovery (every
        // v1 archive was written whole at finalize), so an absent flag there
        // means "old writer", not "recovered".
        if manifest.version >= 2 && !rec.complete {
            let _ = writeln!(
                out,
                "    ! not cleanly finalized — recovered up to its last checkpoint; \
                 data after that may be missing"
            );
        }
        if let Some(line) = clock_line(rec) {
            let _ = writeln!(out, "    {line}");
        }
        for t in &rec.tables {
            let cadence = t
                .cadence_ns
                .map(|ns| format!("{:.3}s", ns as f64 / 1e9))
                .unwrap_or_else(|| "—".to_string());
            // Only worth saying when the table was sealed more than once: a
            // single segment is the unremarkable case and every v1 table is one.
            let segments = match t.segment_files().len() {
                n if n > 1 => format!("  ({n} segments)"),
                _ => String::new(),
            };
            let _ = writeln!(
                out,
                "    {:<24} {:>5} rows  ~{}{}",
                t.sampler, t.rows, cadence, segments
            );
        }
    }
    out
}

#[cfg(test)]
mod rez_tests {
    use super::*;
    use crate::recorder::rez::{RezManifest, RezRecording, RezTableIndex};

    #[test]
    fn describe_rez_string_lists_recordings_and_tables() {
        let m = RezManifest {
            version: 1,
            recordings: vec![RezRecording {
                dir: "rezolus".to_string(),
                labels: [
                    ("source".to_string(), "rezolus".to_string()),
                    ("arm".to_string(), "baseline".to_string()),
                ]
                .into_iter()
                .collect(),
                metadata: Default::default(),
                complete: true,
                clock_anchor_wall_ns: None,
                clock_offsets: Vec::new(),
                tables: vec![RezTableIndex {
                    sampler: "cpu_usage".to_string(),
                    file: Some("cpu_usage.parquet".to_string()),
                    files: vec!["cpu_usage.parquet".to_string()],
                    columns: vec!["0".to_string()],
                    rows: 7,
                    cadence_ns: Some(1_000_000_000),
                }],
            }],
        };
        let s = describe_rez_string(&m);
        assert!(s.contains("recording rezolus"), "{s}");
        assert!(s.contains("arm=baseline"), "{s}");
        assert!(s.contains("cpu_usage"), "{s}");
        assert!(s.contains("7 rows"), "{s}");
        assert!(s.contains("~1.000s"), "{s}");
        assert!(
            !s.contains("not cleanly finalized"),
            "a v1 archive has no completeness flag to report: {s}"
        );
    }

    // A recording recovered from a `.partial` (killed recorder, power loss)
    // must say so: its tables are truthful but stop at the last checkpoint.
    #[test]
    fn describe_rez_string_flags_an_unfinalized_v2_recording() {
        let mut m = RezManifest {
            version: 2,
            recordings: vec![RezRecording {
                dir: "rezolus".to_string(),
                labels: Default::default(),
                metadata: Default::default(),
                complete: false,
                clock_anchor_wall_ns: Some(1_700_000_000_000_000_000),
                clock_offsets: Vec::new(),
                tables: Vec::new(),
            }],
        };
        assert!(
            describe_rez_string(&m).contains("not cleanly finalized"),
            "{}",
            describe_rez_string(&m)
        );

        m.recordings[0].complete = true;
        assert!(!describe_rez_string(&m).contains("not cleanly finalized"));

        // v1 predates the flag entirely, so it is never interpreted there.
        m.version = 1;
        m.recordings[0].complete = false;
        assert!(!describe_rez_string(&m).contains("not cleanly finalized"));
    }

    fn table(sampler: &str, segments: usize) -> RezTableIndex {
        let files: Vec<String> = (0..segments)
            .map(|i| format!("{sampler}/{i:04}.parquet"))
            .collect();
        RezTableIndex {
            sampler: sampler.to_string(),
            file: match files.as_slice() {
                [one] => Some(one.clone()),
                _ => None,
            },
            files,
            columns: Vec::new(),
            rows: 9,
            cadence_ns: Some(1_000_000_000),
        }
    }

    // A streamed recording's segment count is the reader's whole story: it says
    // whether the archive exercises the splice path at all, and (with the
    // completeness flag) how much a recovered one is likely missing.
    #[test]
    fn describe_rez_string_reports_segment_counts_for_a_recovered_archive() {
        let m = RezManifest {
            version: 2,
            recordings: vec![RezRecording {
                dir: "rezolus".to_string(),
                labels: Default::default(),
                metadata: Default::default(),
                complete: false,
                clock_anchor_wall_ns: None,
                clock_offsets: Vec::new(),
                // The real shape: a fast sampler sealed twice, a slow one once.
                tables: vec![table("cpu_usage", 2), table("blockio_latency", 1)],
            }],
        };
        let s = describe_rez_string(&m);
        assert!(s.contains("not cleanly finalized"), "{s}");
        assert!(s.contains("2 segments"), "{s}");
        assert!(
            !s.contains("1 segment"),
            "a single-segment table says nothing about segments: {s}"
        );
    }

    #[test]
    fn describe_rez_string_reports_clock_anchor_and_last_offset() {
        let mut m = RezManifest {
            version: 2,
            recordings: vec![RezRecording {
                dir: "rezolus".to_string(),
                labels: Default::default(),
                metadata: Default::default(),
                complete: true,
                clock_anchor_wall_ns: Some(1_700_000_000_000_000_000),
                // Deliberately out of order: the series is a per-batch maximum,
                // so an age-sealed slow sampler can append an older timestamp.
                // The reported offset is the newest observation, not the last
                // element.
                clock_offsets: vec![(3_000, 1_500_000), (5_000, -2_250_000), (4_000, 9)],
                tables: Vec::new(),
            }],
        };
        let s = describe_rez_string(&m);
        assert!(s.contains("clock anchor"), "{s}");
        assert!(s.contains("2023-11-14T22:13:20"), "{s}");
        assert!(s.contains("-2.250ms"), "the newest observation, in ms: {s}");
        assert!(s.contains("3 observations"), "{s}");

        // An anchor with no observations still reports the anchor.
        m.recordings[0].clock_offsets.clear();
        let s = describe_rez_string(&m);
        assert!(s.contains("clock anchor"), "{s}");
        assert!(!s.contains("offset"), "{s}");

        // v1 archives carry neither field, so nothing is printed.
        m.recordings[0].clock_anchor_wall_ns = None;
        assert!(!describe_rez_string(&m).contains("clock anchor"));
    }
}

/// `parquet metadata` on a v3 (SQLite) `.rez`.
///
/// Every fixture here writes segment BLOBs that are **not parquet**. That is
/// deliberate, and it is what makes "described from the catalog" testable: the
/// v2 summary decoded nothing, and the v3 one must not either — on a fleet
/// archive (197 MB / 149 segments) reading segment bytes to count rows or to
/// recover a clock offset would turn a 0.21 s command into a full scan.
#[cfg(test)]
mod rez_v3_tests {
    use super::*;
    use crate::recorder::rez_sqlite::{RecordingMeta, RezDb, SegmentMeta, WalRow};

    const ANCHOR: u64 = 1_700_000_000_000_000_000;
    const SECOND: u64 = 1_000_000_000;
    const NOT_PARQUET: &[u8] = b"if this were ever decoded, these tests would fail";

    fn meta(labels: &[(&str, &str)]) -> RecordingMeta {
        RecordingMeta {
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            metadata: [("sampling_interval_ms".to_string(), "1000".to_string())]
                .into_iter()
                .collect(),
            clock_anchor_wall_ns: ANCHOR,
        }
    }

    /// A segment covering `rows` ticks starting at tick `from`, one per second.
    fn seg(from: u64, rows: u64) -> SegmentMeta {
        SegmentMeta {
            rows,
            first_ts: ANCHOR + from * SECOND,
            last_ts: ANCHOR + (from + rows - 1) * SECOND,
        }
    }

    fn wal_row(sampler: &str, tick: u64) -> WalRow {
        WalRow {
            sampler: sampler.to_string(),
            ts: ANCHOR + tick * SECOND,
            wall_offset: 0,
            row: b"opaque".to_vec(),
        }
    }

    #[test]
    fn describe_v3_names_the_container_and_summarizes_each_table() {
        // Dispatch is by content: this file has a `.rez` name and a SQLite
        // body, so a describer that assumed the tar container fails outright
        // rather than mis-rendering.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut db = RezDb::create(&path).unwrap();
        let rid = db
            .insert_recording(&meta(&[("source", "rezolus"), ("arm", "baseline")]))
            .unwrap();
        db.insert_segment(rid, "cpu_usage", 0, &seg(0, 3), NOT_PARQUET)
            .unwrap();
        db.insert_segment(rid, "cpu_usage", 1, &seg(3, 2), NOT_PARQUET)
            .unwrap();
        db.transaction(|tx| tx.mark_complete(rid)).unwrap();
        drop(db);

        let s = describe_rez_string_at(&path).unwrap();
        assert!(s.contains(".rez archive v3"), "the container is named: {s}");
        assert!(s.contains("1 recording(s)"), "{s}");
        // v3 has no `dir`; the display name is the slug that produced it.
        assert!(s.contains("recording rezolus"), "{s}");
        assert!(s.contains("arm=baseline"), "{s}");
        assert!(s.contains("cpu_usage"), "{s}");
        assert!(s.contains("5 rows"), "rows are summed across segments: {s}");
        assert!(s.contains("2 segments"), "{s}");
        assert!(s.contains("~1.000s"), "cadence from the catalog span: {s}");
        assert!(
            !s.contains("not cleanly finalized"),
            "this recording was finalized: {s}"
        );
        assert!(
            !s.contains("unsealed"),
            "a finalized recording has an empty WAL: {s}"
        );
    }

    #[test]
    fn describe_v3_flags_a_killed_recording_and_reports_its_recoverable_wal_depth() {
        // A recording killed before finalize is the case v3 exists for, and it
        // is described in v3's OWN terms: there are no checkpoints here, and
        // the loss is bounded by one tick rather than by a seal period. The WAL
        // depth is what makes that claim checkable — how many rows are
        // recoverable but not yet in a segment, which v2 could not report at
        // all because it had nowhere to keep them.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut db = RezDb::create(&path).unwrap();
        let rid = db
            .insert_recording(&meta(&[("source", "rezolus")]))
            .unwrap();
        db.insert_segment(rid, "cpu_usage", 0, &seg(0, 3), NOT_PARQUET)
            .unwrap();
        // Ticks 3 and 4 are committed to the WAL but never sealed; tick 2 is
        // the straddle the deferred prune leaves behind, and is NOT recoverable
        // data — it is already inside the segment above.
        db.insert_wal_rows(
            rid,
            &[
                wal_row("cpu_usage", 2),
                wal_row("cpu_usage", 3),
                wal_row("cpu_usage", 4),
            ],
        )
        .unwrap();
        drop(db); // killed: `complete` stays 0

        let s = describe_rez_string_at(&path).unwrap();
        assert!(s.contains("not cleanly finalized"), "{s}");
        assert!(
            s.contains("final tick"),
            "v3's guarantee is one tick, not v2's last checkpoint: {s}"
        );
        assert!(
            !s.contains("checkpoint"),
            "v3 has no checkpoints to recover up to: {s}"
        );
        assert!(
            s.contains("2 unsealed in WAL"),
            "only the rows past the sealed watermark are recoverable: {s}"
        );
        assert!(
            s.contains("5 rows"),
            "3 sealed + 2 live is what a reader will see: {s}"
        );
    }

    #[test]
    fn describe_v3_lists_a_sampler_that_never_sealed_a_segment() {
        // The 16-of-26 case. A quiet table still inside its first seal period
        // has no `segments` row at all, so a listing built from `samplers()`
        // would not mention it — and the whole point of v3 is that its rows are
        // there to be read.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut db = RezDb::create(&path).unwrap();
        let rid = db
            .insert_recording(&meta(&[("source", "rezolus")]))
            .unwrap();
        db.insert_segment(rid, "cpu_usage", 0, &seg(0, 3), NOT_PARQUET)
            .unwrap();
        db.insert_wal_rows(
            rid,
            &[
                wal_row("drivehealth", 0),
                wal_row("drivehealth", 1),
                wal_row("drivehealth", 2),
            ],
        )
        .unwrap();
        drop(db);

        let s = describe_rez_string_at(&path).unwrap();
        assert!(
            s.contains("drivehealth"),
            "a never-sealed sampler must still be listed: {s}"
        );
        assert!(
            s.contains("(0 segments, 3 unsealed in WAL)"),
            "and every one of its rows is recoverable from the WAL: {s}"
        );
        assert!(
            s.contains("~1.000s"),
            "its cadence comes from the WAL span, there being no segment: {s}"
        );
    }

    #[test]
    fn describe_v3_renders_the_clock_line_from_the_catalog_with_no_segment_decode() {
        // v2 rendered drift straight from the manifest. In v3 the per-row
        // observations live in each segment's `:wall_offset` column, so
        // reproducing that without a decode is exactly why every seal writes a
        // `clock_offsets` ROW — and why a killed recording has drift to report
        // at all. The segment bytes here are not parquet, so an implementation
        // that reached into them to answer this fails.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut db = RezDb::create(&path).unwrap();
        let rid = db
            .insert_recording(&meta(&[("source", "rezolus")]))
            .unwrap();
        db.insert_segment(rid, "cpu_usage", 0, &seg(0, 3), NOT_PARQUET)
            .unwrap();
        // Deliberately out of order: the series is a per-batch maximum, so an
        // age-sealed slow sampler can append a timestamp older than one a
        // fast-sampler batch already contributed. The reported offset is the
        // newest observation, not the last row.
        db.transaction(|tx| {
            tx.insert_clock_offset(rid, ANCHOR + 3 * SECOND, 1_500_000)?;
            tx.insert_clock_offset(rid, ANCHOR + 5 * SECOND, -2_250_000)?;
            tx.insert_clock_offset(rid, ANCHOR + 4 * SECOND, 9)
        })
        .unwrap();
        drop(db);

        let s = describe_rez_string_at(&path).unwrap();
        assert!(s.contains("clock anchor 2023-11-14T22:13:20"), "{s}");
        assert!(s.contains("-2.250ms"), "the newest observation, in ms: {s}");
        assert!(s.contains("3 observations"), "{s}");
    }

    #[test]
    fn describe_v3_json_carries_the_same_facts_as_the_text_form() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let mut db = RezDb::create(&path).unwrap();
        let rid = db
            .insert_recording(&meta(&[("source", "rezolus"), ("arm", "baseline")]))
            .unwrap();
        db.insert_segment(rid, "cpu_usage", 0, &seg(0, 3), NOT_PARQUET)
            .unwrap();
        db.insert_wal_rows(rid, &[wal_row("cpu_usage", 3)]).unwrap();
        drop(db);

        let v = v3_json(&read_v3_summary(&path).unwrap());
        assert_eq!(v["version"], 3);
        assert_eq!(v["recordings"][0]["name"], "rezolus");
        assert_eq!(v["recordings"][0]["labels"]["arm"], "baseline");
        assert_eq!(
            v["recordings"][0]["metadata"]["sampling_interval_ms"],
            "1000"
        );
        assert_eq!(v["recordings"][0]["complete"], false);
        let table = &v["recordings"][0]["tables"][0];
        assert_eq!(table["sampler"], "cpu_usage");
        assert_eq!(table["rows"], 4);
        assert_eq!(table["segments"], 1);
        assert_eq!(table["live_wal_rows"], 1);
        assert_eq!(table["cadence_ns"], SECOND);
    }
}
