mod annotate;
mod combine;
mod filter;
mod metadata;

use arrow::datatypes::SchemaRef;
use clap::{value_parser, ArgMatches, Command};
use parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};
use parquet::file::metadata::ParquetMetaData;
use parquet::format::KeyValue;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

/// Built-in service extension templates keyed by canonical source name.
pub(crate) static TEMPLATES: &[(&str, &str)] = &[
    ("llm-perf", include_str!("templates/llm_perf.json")),
    ("cachecannon", include_str!("templates/cachecannon.json")),
];

/// Source name aliases for renamed projects (old name → canonical name).
pub(crate) static SOURCE_ALIASES: &[(&str, &str)] = &[("llm-bench", "llm-perf")];

/// Look up a built-in service template by source name, resolving aliases.
pub(crate) fn lookup_template(source: &str) -> Option<&'static str> {
    let canonical = SOURCE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == source)
        .map(|(_, canon)| *canon)
        .unwrap_or(source);
    TEMPLATES
        .iter()
        .find(|(name, _)| *name == canonical)
        .map(|(_, json)| *json)
}

pub fn command() -> Command {
    Command::new("parquet")
        .about("Parquet file operations")
        .subcommand_required(true)
        .subcommand(
            Command::new("annotate")
                .about("Add service extension metadata to a parquet file")
                .arg(
                    clap::Arg::new("FILE")
                        .help("Parquet file to annotate")
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("queries")
                        .long("queries")
                        .value_name("PATH")
                        .help("Custom service extension JSON file (overrides built-in template)")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("undo")
                        .long("undo")
                        .help("Remove service extension annotation from the parquet file")
                        .action(clap::ArgAction::SetTrue)
                        .conflicts_with("queries"),
                )
                .arg(
                    clap::Arg::new("filter")
                        .long("filter")
                        .help("Also filter columns to only those needed by the service extension KPIs")
                        .action(clap::ArgAction::SetTrue)
                        .conflicts_with("undo"),
                ),
        )
        .subcommand(
            Command::new("combine")
                .about("Combine a rezolus parquet file with service-level parquet files")
                .arg(
                    clap::Arg::new("FILES")
                        .help("Input parquet files (one rezolus + one or more service files)")
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .num_args(2..)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output")
                        .help("Output parquet file path")
                        .value_parser(value_parser!(PathBuf))
                        .required(true),
                ),
        )
        .subcommand(
            Command::new("metadata")
                .about("Display file and column metadata for a parquet file")
                .arg(
                    clap::Arg::new("input")
                        .short('i')
                        .long("input")
                        .help("Input parquet file")
                        .value_parser(value_parser!(PathBuf))
                        .required(true),
                )
                .arg(
                    clap::Arg::new("schema")
                        .long("schema")
                        .help("Show only column-level metadata (schema)")
                        .action(clap::ArgAction::SetTrue),
                )
                .arg(
                    clap::Arg::new("file")
                        .long("file")
                        .help("Show only file-level metadata")
                        .action(clap::ArgAction::SetTrue),
                )
                .arg(
                    clap::Arg::new("geometry")
                        .long("geometry")
                        .help("Show only table geometry (shape and row group layout)")
                        .action(clap::ArgAction::SetTrue),
                )
                .arg(
                    clap::Arg::new("field")
                        .long("field")
                        .help("Print the value of a specific file-level metadata key")
                        .value_name("KEY")
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .help("Output in JSON format (for programmatic use)")
                        .action(clap::ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("filter")
                .about("Filter parquet columns to only those needed by service extension KPIs")
                .arg(
                    clap::Arg::new("FILE")
                        .help("Parquet file to filter")
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("queries")
                        .long("queries")
                        .value_name("PATH")
                        .help("Custom service extension JSON file (overrides metadata/template)")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output")
                        .value_name("PATH")
                        .help("Output file path (default: overwrite input file in-place)")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
                ),
        )
}

pub fn run(args: ArgMatches) {
    let result = match args.subcommand() {
        Some(("annotate", sub_args)) => {
            annotate::run(sub_args);
            return;
        }
        Some(("combine", sub_args)) => combine::run(sub_args),
        Some(("filter", sub_args)) => {
            filter::run(sub_args);
            return;
        }
        Some(("metadata", sub_args)) => metadata::run(sub_args),
        _ => unreachable!(),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

/// Read file-level key-value metadata from a parquet file footer.
pub(crate) fn read_file_metadata(
    path: impl AsRef<Path>,
) -> Result<Vec<KeyValue>, Box<dyn std::error::Error>> {
    use parquet::file::reader::FileReader;
    use parquet::file::serialized_reader::SerializedFileReader;

    let reader = SerializedFileReader::new(std::fs::File::open(path)?)?;
    Ok(reader
        .metadata()
        .file_metadata()
        .key_value_metadata()
        .cloned()
        .unwrap_or_default())
}

/// Rewrite a parquet file with updated metadata, optionally projecting columns.
/// Returns the serialized parquet bytes.
///
/// If `projection` is `Some`, only the columns at those indices are kept and
/// the output schema is projected accordingly.  If `None`, all columns are
/// passed through unchanged.
pub(crate) fn rewrite_parquet(
    path: impl AsRef<Path>,
    kv_meta: Vec<KeyValue>,
    projection: Option<&[usize]>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    use parquet::arrow::ArrowWriter;
    use parquet::file::properties::WriterProperties;

    let builder = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(path)?)?;
    let schema = builder.schema().clone();
    let reader = builder.build()?;

    let output_schema = match projection {
        Some(indices) => Arc::new(schema.project(indices)?),
        None => schema,
    };

    let props = WriterProperties::builder()
        .set_key_value_metadata(Some(kv_meta))
        .set_max_row_group_size(crate::parquet_metadata::MAX_ROW_GROUP_SIZE)
        .build();

    let mut buf = Vec::new();
    {
        let mut writer =
            ArrowWriter::try_new(std::io::Cursor::new(&mut buf), output_schema, Some(props))?;
        for batch in reader {
            let batch = batch?;
            let batch = match projection {
                Some(indices) => batch.project(indices)?,
                None => batch,
            };
            writer.write(&batch)?;
        }
        writer.close()?;
    }

    Ok(buf)
}

fn read_parquet_footer(
    input: impl AsRef<Path>,
) -> Result<(Arc<ParquetMetaData>, SchemaRef, ParquetRecordBatchReader), Box<dyn std::error::Error>>
{
    let file = std::fs::File::open(input)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let metadata = builder.metadata().clone();
    let schema = builder.schema().clone();
    let reader = builder.build()?;
    Ok((metadata, schema, reader))
}
