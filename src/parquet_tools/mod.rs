mod annotate;
mod compare;
mod metadata;

use arrow::datatypes::SchemaRef;
use clap::{value_parser, ArgMatches, Command};
use parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};
use parquet::file::metadata::ParquetMetaData;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

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
                    clap::Arg::new("service-extension")
                        .long("file")
                        .value_name("PATH")
                        .help("Custom service extension JSON file (overrides built-in template)")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
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
                ),
        )
        .subcommand(
            Command::new("compare-schema")
                .about("Compare schemas between two parquet files")
                .arg(
                    clap::Arg::new("left")
                        .short('l')
                        .long("left")
                        .help("Left parquet file")
                        .value_parser(value_parser!(PathBuf))
                        .required(true),
                )
                .arg(
                    clap::Arg::new("right")
                        .short('r')
                        .long("right")
                        .help("Right parquet file")
                        .value_parser(value_parser!(PathBuf))
                        .required(true),
                ),
        )
        .subcommand(
            Command::new("compare")
                .about("Compare data values between two parquet files")
                .arg(
                    clap::Arg::new("left")
                        .short('l')
                        .long("left")
                        .help("Left parquet file")
                        .value_parser(value_parser!(PathBuf))
                        .required(true),
                )
                .arg(
                    clap::Arg::new("right")
                        .short('r')
                        .long("right")
                        .help("Right parquet file")
                        .value_parser(value_parser!(PathBuf))
                        .required(true),
                ),
        )
}

pub fn run(args: ArgMatches) {
    let result = match args.subcommand() {
        Some(("annotate", sub_args)) => {
            annotate::run(sub_args);
            return;
        }
        Some(("metadata", sub_args)) => metadata::run(sub_args),
        Some(("compare-schema", sub_args)) => compare::run_compare_schema(sub_args),
        Some(("compare", sub_args)) => compare::run_compare(sub_args),
        _ => unreachable!(),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
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
