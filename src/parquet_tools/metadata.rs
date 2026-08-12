use clap::ArgMatches;
use std::path::PathBuf;

use super::read_parquet_footer;

pub(super) fn run(args: &ArgMatches) -> Result<(), Box<dyn std::error::Error>> {
    let input = args.get_one::<PathBuf>("input").unwrap();
    let schema_only = args.get_flag("schema");
    let geometry_only = args.get_flag("geometry");
    let file_only = args.get_flag("file");
    let field_key = args.get_one::<String>("field");
    let json = args.get_flag("json");

    // `.rez` archives have no single parquet footer — describe the manifest.
    if crate::recorder::rez::is_rez_path(input).unwrap_or(false) {
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

/// Describe a `.rez` archive from its manifest (the `metadata` command; `.rez`
/// has no single parquet footer, so we summarize the recordings/tables index).
fn describe_rez(path: &std::path::Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let (manifest, _tables) = crate::recorder::rez::read_archive_bytes(path)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&manifest)?);
    } else {
        print!("{}", describe_rez_string(&manifest));
    }
    Ok(())
}

/// The recording's clock line: the wall-clock anchor its row timestamps are
/// offset from, plus the newest drift observation. `None` for a v1 recording,
/// which carries neither field.
///
/// The observation reported is the newest by *timestamp*, not the last element:
/// the series is a per-batch maximum, so an age-sealed slow sampler can append
/// a timestamp older than one a fast-sampler batch already contributed.
fn clock_line(rec: &crate::recorder::rez::RezRecording) -> Option<String> {
    let anchor = rec.clock_anchor_wall_ns?;
    let when = i64::try_from(anchor)
        .map(|ns| {
            chrono::DateTime::from_timestamp_nanos(ns)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .unwrap_or_else(|_| format!("{anchor}ns"));
    let mut line = format!("clock anchor {when}");
    if let Some(&(_, offset)) = rec.clock_offsets.iter().max_by_key(|&&(ts, _)| ts) {
        let _ = std::fmt::Write::write_fmt(
            &mut line,
            format_args!(
                ", latest offset {:+.3}ms ({} observations)",
                offset as f64 / 1e6,
                rec.clock_offsets.len()
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
