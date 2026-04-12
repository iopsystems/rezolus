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

    let (metadata, schema, _) = read_parquet_footer(input)?;

    // --field=KEY: print the raw value of a single file-level metadata key
    if let Some(key) = field_key {
        let kv = metadata.file_metadata().key_value_metadata();
        let value = kv
            .and_then(|entries| entries.iter().find(|e| e.key == *key))
            .and_then(|e| e.value.as_deref());

        match value {
            Some(v) => {
                // Always try to pretty-print if it's valid JSON
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

    // File-level metadata
    if show_all || file_only {
        if let Some(kv) = metadata.file_metadata().key_value_metadata() {
            println!("File Metadata:");
            for entry in kv {
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

        // Compute column widths for row group table
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
        println!(
            "  {:-<idx_w$}-+-{:->rows_w$}-+-{:->bytes_w$}",
            "", "", "",
        );
        for (i, rg) in row_groups.iter().enumerate() {
            println!(
                "  {:<idx_w$} | {:>rows_w$} | {:>bytes_w$}",
                i,
                rg.num_rows(),
                format_bytes(rg.total_byte_size()),
            );
        }
        println!(
            "  {:-<idx_w$}-+-{:->rows_w$}-+-{:->bytes_w$}",
            "", "", "",
        );
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
        // Pre-compute rows: (name, type, metric_type, other_metadata)
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
            let dtype = format!("{}", field.data_type());
            let meta = field.metadata();

            let metric_type = meta
                .get("metric_type")
                .cloned()
                .unwrap_or_default();

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
            "  {:<name_w$} | {:<type_w$} | {:<mt_w$} | Metadata",
            "Name", "Type", "Metric Type",
        );
        println!(
            "  {:-<name_w$}-+-{:-<type_w$}-+-{:-<mt_w$}-+---------",
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
                let value = entry.value.as_deref().unwrap_or("");
                // Try to parse as JSON to nest it properly
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
        out.insert("file_metadata".to_string(), serde_json::Value::Object(file_meta));
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
