use clap::ArgMatches;
use std::path::PathBuf;

use super::read_parquet_footer;

pub(super) fn run(args: &ArgMatches) -> Result<(), Box<dyn std::error::Error>> {
    let input = args.get_one::<PathBuf>("input").unwrap();
    let schema_only = args.get_flag("schema");

    let (metadata, schema, _) = read_parquet_footer(input)?;

    if !schema_only {
        // File-level metadata
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

        // Row group summary
        let row_groups = metadata.row_groups();
        println!("\nRow Groups: {}", row_groups.len());
        for (i, rg) in row_groups.iter().enumerate() {
            println!("  Group {}: {} rows", i, rg.num_rows());
        }

        println!();
    }

    // Column-level metadata (schema)
    println!("Schema ({} fields):", schema.fields().len());
    for field in schema.fields() {
        let meta = field.metadata();
        if meta.is_empty() {
            println!("  {} ({})", field.name(), field.data_type());
        } else {
            let meta_str: Vec<String> = meta.iter().map(|(k, v)| format!("{k}={v}")).collect();
            println!(
                "  {} ({}) [{}]",
                field.name(),
                field.data_type(),
                meta_str.join(", ")
            );
        }
    }

    Ok(())
}
