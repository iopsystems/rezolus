use clap::ArgMatches;
use std::path::PathBuf;

use super::read_parquet_footer;

pub(super) fn run(args: &ArgMatches) -> Result<(), Box<dyn std::error::Error>> {
    let input = args.get_one::<PathBuf>("input").unwrap();
    let schema_only = args.get_flag("schema");
    let geometry_only = args.get_flag("geometry");
    let file_only = args.get_flag("file");
    let show_all = !schema_only && !geometry_only && !file_only;

    let (metadata, schema, _) = read_parquet_footer(input)?;

    // File-level metadata
    if show_all || file_only {
        if let Some(kv) = metadata.file_metadata().key_value_metadata() {
            println!("File Metadata:");
            for entry in kv {
                let value = entry.value.as_deref().unwrap_or("");
                // Truncate long values for readability
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
        println!("  Logical Table: {} columns x {} rows", num_columns, total_rows);
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

    // Column-level metadata (schema)
    if show_all || schema_only {
        println!("Schema ({} fields):", schema.fields().len());
        for field in schema.fields() {
            let meta = field.metadata();
            if meta.is_empty() {
                println!("  {} ({})", field.name(), field.data_type());
            } else {
                let meta_str: Vec<String> =
                    meta.iter().map(|(k, v)| format!("{k}={v}")).collect();
                println!(
                    "  {} ({}) [{}]",
                    field.name(),
                    field.data_type(),
                    meta_str.join(", ")
                );
            }
        }
    }

    Ok(())
}
