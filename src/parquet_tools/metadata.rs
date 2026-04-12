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
                // Try to pretty-print if it's valid JSON
                if json {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(v) {
                        println!("{}", serde_json::to_string_pretty(&parsed)?);
                    } else {
                        println!("{v}");
                    }
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
        let num_columns = schema.fields().len();

        println!("Geometry:");
        println!(
            "  Logical Table: {} columns x {} rows",
            num_columns, total_rows
        );
        println!("  Row Groups: {}", row_groups.len());
        for (i, rg) in row_groups.iter().enumerate() {
            println!(
                "    Group {}: {} rows, {} bytes",
                i,
                rg.num_rows(),
                rg.total_byte_size()
            );
        }
        if show_all {
            println!();
        }
    }

    // Column-level metadata (schema) - human-readable table
    if show_all || schema_only {
        // Compute column widths
        let mut name_w = 4; // "Name"
        let mut type_w = 4; // "Type"
        let mut meta_entries: Vec<(&str, &str, String)> = Vec::new();

        for field in schema.fields() {
            let name = field.name().as_str();
            let dtype = format!("{}", field.data_type());
            let meta = field.metadata();
            let meta_str = if meta.is_empty() {
                String::new()
            } else {
                meta.iter()
                    .map(|(k, v)| {
                        if v.len() > 40 {
                            format!("{k}={{...}}")
                        } else {
                            format!("{k}={v}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            name_w = name_w.max(name.len());
            type_w = type_w.max(dtype.len());
            meta_entries.push((name, Box::leak(dtype.into_boxed_str()), meta_str));
        }

        println!(
            "Schema ({} fields):",
            schema.fields().len()
        );
        println!(
            "  {:<name_w$}  {:<type_w$}  Metadata",
            "Name", "Type"
        );
        println!(
            "  {:<name_w$}  {:<type_w$}  --------",
            "----", "----"
        );
        for (name, dtype, meta_str) in &meta_entries {
            println!("  {:<name_w$}  {:<type_w$}  {}", name, dtype, meta_str);
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
