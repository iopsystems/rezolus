mod annotate;
pub(crate) mod combine;
mod convert;
mod events;
mod filter;
pub(crate) mod metadata;
mod split_groups;

use arrow::datatypes::SchemaRef;
use clap::{value_parser, ArgMatches, Command};
use parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};
use parquet::file::metadata::KeyValue;
use parquet::file::metadata::ParquetMetaData;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

pub fn command() -> Command {
    Command::new("parquet")
        .about("Inspect and transform Rezolus parquet recordings")
        .long_about(
            "Offline operations on parquet recordings produced by `rezolus record` or\n\
             `rezolus hindsight`.\n\n\
             SUBCOMMANDS:\n    \
             metadata   Inspect a file's file-level/column metadata, schema, and geometry\n    \
             annotate   Embed service-extension KPIs, events, or source/node tags into a file\n    \
             combine    Merge multiple files (multi-node / multi-instance) or build an A/B tarball\n    \
             convert    Turn a raw msgpack recording (from `record -f raw`) into parquet\n    \
             filter     Drop columns not needed by a file's service-extension KPIs (shrink it)\n\n\
             Run `rezolus parquet <subcommand> --help` for per-subcommand examples.",
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("annotate")
                .about("Add service extension metadata to a parquet file")
                .long_about(
                    "Rewrite a parquet recording in place, adding metadata the viewer reads.\n\
                     Use it to attach service-extension KPI dashboards, tag the file's\n\
                     source/node identity, embed a systeminfo blob, or record timeline events.\n\n\
                     By default KPIs come from the built-in template matching the file's source;\n\
                     override with --queries <file.json>. --undo strips a prior annotation.\n\n\
                     A .rez archive is also accepted: --queries embeds a ServiceExtension (KPIs)\n\
                     into each recording's manifest metadata. Since a .rez is source=rezolus with\n\
                     no built-in template, --queries is required for a .rez. Only v2 (tar) .rez\n\
                     archives can be annotated in place today; a v3 (SQLite) archive errors\n\
                     clearly rather than being silently mishandled.\n\n\
                     EXAMPLES:\n    \
                     # Attach KPIs from the built-in template for this file's source\n    \
                     rezolus parquet annotate rezolus.parquet\n\n    \
                     # Attach KPIs from a custom service-extension JSON file\n    \
                     rezolus parquet annotate rezolus.parquet --queries ext.json\n\n    \
                     # Attach KPIs and also drop columns the KPIs don't need\n    \
                     rezolus parquet annotate rezolus.parquet --queries ext.json --filter\n\n    \
                     # Set the source name on a file that has none\n    \
                     rezolus parquet annotate service.parquet --source vllm\n\n    \
                     # Add a single timeline event\n    \
                     rezolus parquet annotate rezolus.parquet --event 'time=2026-05-12T15:23Z,kind=restart,description=\"deploy\"'\n\n    \
                     # Remove a previously added annotation\n    \
                     rezolus parquet annotate rezolus.parquet --undo\n\n    \
                     # Embed KPIs into each recording of a .rez archive (--queries required)\n    \
                     rezolus parquet annotate out.rez --queries kpis.json",
                )
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
                )
                .arg(
                    clap::Arg::new("templates")
                        .long("templates")
                        .value_name("DIR")
                        .help("Directory containing service extension template JSON files")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("node")
                        .long("node")
                        .value_name("NAME")
                        .help("Set the node attribute on this parquet file")
                        .value_parser(value_parser!(String))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("source")
                        .long("source")
                        .value_name("NAME")
                        .help("Set the source attribute (use with --overwrite to replace an existing one)")
                        .value_parser(value_parser!(String))
                        .action(clap::ArgAction::Set)
                        .conflicts_with("undo"),
                )
                .arg(
                    clap::Arg::new("overwrite")
                        .long("overwrite")
                        .help("Allow --source to replace an existing source value")
                        .action(clap::ArgAction::SetTrue)
                        .requires("source"),
                )
                .arg(
                    clap::Arg::new("systeminfo")
                        .long("systeminfo")
                        .value_name("PATH")
                        .help("Embed systeminfo JSON from PATH (or '-' for stdin) into the parquet footer")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set)
                        .conflicts_with("undo"),
                )
                .arg(
                    clap::Arg::new("add-events")
                        .long("add-events")
                        .value_name("PATH")
                        .help("Add one-off events from a JSON/JSONL file (or '-' for stdin). Repeatable.")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Append)
                        .conflicts_with("undo"),
                )
                .arg(
                    clap::Arg::new("event")
                        .long("event")
                        .value_name("KV")
                        .help("Add a single event inline, e.g. 'time=2026-05-12T15:23Z,kind=restart,description=\"...\"'. Repeatable.")
                        .value_parser(value_parser!(String))
                        .action(clap::ArgAction::Append)
                        .conflicts_with("undo"),
                )
                .arg(
                    clap::Arg::new("clear-events")
                        .long("clear-events")
                        .help("Remove existing events before applying --add-events / --event")
                        .action(clap::ArgAction::SetTrue)
                        .conflicts_with("undo"),
                ),
        )
        .subcommand(
            Command::new("combine")
                .about("Combine multiple parquet files (multi-node rezolus and/or multi-instance services)")
                .long_about(
                    "Merge two or more parquet recordings into a single file. Requires at least\n\
                     two inputs and an output path (-o).\n\n\
                     Default (row-merge): joins the inputs on timestamp into one recording — use\n\
                     it to stitch together multiple rezolus nodes and/or per-instance service\n\
                     files so the viewer shows them together.\n\n\
                     --ab (tarball): instead of row-merging, packages exactly two captures\n\
                     unmodified into a combined-A/B tarball for the viewer's compare mode. The\n\
                     output should end in `.parquet.ab.tar`, and you map each side with\n\
                     `baseline=<src> experiment=<src>` where <src> is a file's embedded SOURCE\n\
                     NAME (not its filename) — set with `annotate --source`, seen with\n\
                     `parquet metadata --field source`. In the example below a.parquet's source\n\
                     is `redis` and b.parquet's is `valkey`.\n\n\
                     .rez inputs: given single-recording `.rez` archives and a `.rez` output,\n\
                     combine assembles them into one multi-recording `.rez` (for multi-host or\n\
                     A/B), preserving each recording's labels. Only v2 (tar) .rez archives can be\n\
                     combined today; a v3 (SQLite) input, or a mix of v2 and v3 inputs, errors\n\
                     clearly instead of silently mis-assembling.\n\n\
                     EXAMPLES:\n    \
                     # Row-merge a rezolus agent file with a service file\n    \
                     rezolus parquet combine rezolus.parquet service.parquet -o combined.parquet\n\n    \
                     # Merge several rezolus nodes, pinning which one the viewer shows first\n    \
                     rezolus parquet combine node1.parquet node2.parquet -o cluster.parquet --pinned node1\n\n    \
                     # Package two captures as an A/B tarball for compare mode\n    \
                     rezolus parquet combine a.parquet b.parquet --ab baseline=redis experiment=valkey -o out.parquet.ab.tar\n\n    \
                     # Assemble two single-recording .rez into one multi-recording .rez\n    \
                     rezolus parquet combine baseline.rez experiment.rez -o ab.rez",
                )
                .arg(
                    clap::Arg::new("FILES")
                        .help("Input parquet files (rezolus agent and/or service files)")
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
                )
                .arg(
                    clap::Arg::new("bypass-time-check")
                        .long("bypass-time-check")
                        .help("Skip the timestamp alignment quality check")
                        .action(clap::ArgAction::SetTrue),
                )
                .arg(
                    clap::Arg::new("pinned")
                        .long("pinned")
                        .help("Default rezolus node to display in the viewer (node name or filename)")
                        .value_parser(clap::value_parser!(String)),
                )
                .arg(
                    clap::Arg::new("ab")
                        .long("ab")
                        .help(
                            "Package two captures into a combined-A/B tarball \
                             instead of row-merging into one parquet. The output \
                             path should end in `.parquet.ab.tar`. Requires \
                             exactly two input files. Pass `baseline=<src> \
                             experiment=<src>` mapping each side to one of \
                             the inputs' source names; the captures are stored \
                             unmodified next to an `ab.json` manifest.",
                        )
                        .value_parser(value_parser!(String))
                        .num_args(2)
                        .action(clap::ArgAction::Append),
                )
                .arg(
                    clap::Arg::new("category")
                        .long("category")
                        .value_name("NAME")
                        .help(
                            "Category template name to embed in the AB \
                             tarball's manifest (e.g. `inference-library`). \
                             The viewer auto-applies it on load when the \
                             user did not pass `--category` themselves. \
                             Only meaningful with `--ab`; not validated \
                             against the template registry at combine time.",
                        )
                        .value_parser(value_parser!(String))
                        .requires("ab"),
                ),
        )
        .subcommand(
            Command::new("metadata")
                .about("Display file and column metadata for a parquet file")
                .long_about(
                    "Print the metadata of a parquet recording: file-level key/values (source,\n\
                     sampling interval, systeminfo, descriptions, …), the column schema with\n\
                     each metric's type and labels, and table geometry (row/column counts and\n\
                     row-group layout).\n\n\
                     With no filter flag all sections are shown. Narrow with --file, --schema, or\n\
                     --geometry; pull a single file-level value with --field <KEY>; add --json for\n\
                     machine-readable output.\n\n\
                     A .rez archive is also accepted: it describes the manifest instead (the\n\
                     recordings, their labels, and each per-sampler table with its cadence);\n\
                     --json emits the raw manifest.\n\n\
                     EXAMPLES:\n    \
                     # Everything about a recording\n    \
                     rezolus parquet metadata -i rezolus.parquet\n\n    \
                     # Only file-level metadata, as JSON\n    \
                     rezolus parquet metadata -i rezolus.parquet --file --json\n\n    \
                     # Just the value of one metadata key\n    \
                     rezolus parquet metadata -i rezolus.parquet --field source\n\n    \
                     # Describe a .rez archive's manifest\n    \
                     rezolus parquet metadata -i out.rez",
                )
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
            Command::new("convert")
                .about("Convert a raw msgpack recording into parquet")
                .long_about(
                    "Convert a recording made with `rezolus record -f raw` (concatenated msgpack\n\
                     snapshots, one per sampling tick) into a parquet file that the viewer, the\n\
                     MCP tools and the rest of `rezolus parquet` can read.\n\n\
                     Raw is the cheapest capture mode: the recorder appends snapshots as they\n\
                     arrive and finalizing is a byte copy rather than a conversion that grows\n\
                     with run length, so long unattended captures record raw and convert\n\
                     afterwards. Recording straight to parquet (`record -f parquet`) is simpler\n\
                     for a short supervised run.\n\n\
                     The input may be plain or zstd-compressed; which one it is is detected from\n\
                     the file's contents, not its name. The output path defaults to the input\n\
                     with a trailing .zst and then a trailing .raw stripped, and .parquet\n\
                     appended: rezolus.raw.zst becomes rezolus.parquet, and a name with neither\n\
                     suffix just gains one (capture.msgpack becomes capture.msgpack.parquet).\n\n\
                     The sampling interval stamped into the file is inferred from the median gap\n\
                     between snapshot timestamps unless --interval says otherwise. A recording\n\
                     with fewer than two snapshots falls back to 1s, so pass --interval if such a\n\
                     recording was made at another cadence. Inference warns on stderr when the\n\
                     sampled gaps have no dominant cadence, or when they are closer together than\n\
                     the whole milliseconds the stamped value can hold; the conversion still\n\
                     succeeds in both cases.\n\n\
                     A raw recording carries no systeminfo or metric descriptions: the recorder\n\
                     fetches those over HTTP from the running rezolus agent while recording (its\n\
                     /systeminfo and /metrics/descriptions endpoints) and they never enter the\n\
                     snapshot stream. Without them the conversion still succeeds and every metric\n\
                     still queries normally; what you lose is the viewer's hardware panel and the\n\
                     help text beside each metric. Prefer stamping them here in one pass if you\n\
                     saved them -- and if that agent is still running, the same two endpoints can\n\
                     be curled now, at conversion time, to recover them. Afterwards, `rezolus parquet annotate\n\
                     <file> --systeminfo <path>` can still add the hardware summary, but there is\n\
                     no annotate route for descriptions: those can only be supplied here, which\n\
                     means reconverting the original raw input over the output (--force).\n\n\
                     A .rez archive cannot be produced from a raw recording: it needs per-sampler\n\
                     cadence and acquisition windows that a raw snapshot stream never carried.\n\n\
                     EXAMPLES:\n    \
                     # The producing side, for reference\n    \
                     rezolus record -f raw --url http://localhost:4241 -o rezolus.raw\n\n    \
                     # Convert a compressed recording (writes rezolus.parquet)\n    \
                     rezolus parquet convert rezolus.raw.zst\n\n    \
                     # Choose the output path\n    \
                     rezolus parquet convert rezolus.raw -o run7.parquet\n\n    \
                     # A recording made at a non-default cadence\n    \
                     rezolus parquet convert rezolus.raw --interval 250ms\n\n    \
                     # Save the two metadata blobs at record time, from the agent being recorded\n    \
                     curl -s http://localhost:4241/systeminfo > sysinfo.json\n    \
                     curl -s http://localhost:4241/metrics/descriptions > help.json\n\n    \
                     # ...then stamp them into the conversion\n    \
                     rezolus parquet convert rezolus.raw --systeminfo sysinfo.json --descriptions help.json\n\n    \
                     # Or pipe one of them straight in, with - for stdin\n    \
                     curl -s http://localhost:4241/systeminfo | rezolus parquet convert rezolus.raw --systeminfo -\n\n    \
                     # Tag the recording the way `record --metadata` would\n    \
                     rezolus parquet convert rezolus.raw -m source=llm-perf -m run=boat-7\n\n    \
                     # Replace an output left by an earlier attempt\n    \
                     rezolus parquet convert rezolus.raw --force\n\n    \
                     # Descriptions were missed the first time -- reconvert over the old output\n    \
                     rezolus parquet convert rezolus.raw --descriptions help.json --force",
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("Raw recording to convert (plain or zstd-compressed). Must be a real file; stdin is not accepted here")
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output")
                        .value_name("PATH")
                        .help("Output parquet path (default: input with a trailing .zst then .raw stripped and .parquet appended). Refuses to overwrite an existing file unless --force")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    // `--interval` must never gain a `-i` alias: `parquet
                    // metadata -i` means the input file, so `convert -i
                    // recording.raw` would parse a path as a duration and
                    // report "expected number at 0".
                    clap::Arg::new("interval")
                        .long("interval")
                        .value_name("DURATION")
                        .help("Sampling interval to stamp, as a number with a unit (ns, us, ms, s, m, h) -- 1s, 250ms. Stamped as whole milliseconds, rounded to the nearest; anything below 1ms is rejected. Default: inferred from the median gap between snapshot timestamps, or 1s if the recording holds fewer than two snapshots")
                        .value_parser(value_parser!(humantime::Duration))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("systeminfo")
                        .long("systeminfo")
                        .value_name("PATH")
                        .help("JSON hardware summary to embed, as served by the agent's /systeminfo endpoint; any JSON value is accepted and stored verbatim. Use - for stdin (only one of --systeminfo/--descriptions can read stdin)")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("descriptions")
                        .long("descriptions")
                        .value_name("PATH")
                        .help("Metric help text to embed, as served by the agent's /metrics/descriptions endpoint: a flat JSON object of name to string, {\"cpu_usage\":\"CPU time by state\"}. Use - for stdin (only one of --systeminfo/--descriptions can read stdin)")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("metadata")
                        .short('m')
                        .long("metadata")
                        .value_name("KEY=VALUE")
                        .help("Add a file-level metadata tag as key=value; repeat for multiple tags. Split on the first =, so values may contain = and may be empty. Keys are free-form, but `source` is the one the viewer and MCP tools read to identify where a recording came from (it defaults to rezolus). Tags are applied after the two keys convert derives, `source` and `sampling_interval_ms`, so a tag with either name wins -- set the interval with --interval rather than -m sampling_interval_ms=")
                        .action(clap::ArgAction::Append),
                )
                .arg(
                    clap::Arg::new("force")
                        .long("force")
                        .help("Overwrite the output file if it already exists (without this, an existing output is an error and nothing is written)")
                        .action(clap::ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("filter")
                .about("Filter parquet columns to only those needed by service extension KPIs")
                .long_about(
                    "Shrink a parquet recording by dropping every metric column not referenced by\n\
                     its service-extension KPIs. The KPI set is taken from the file's embedded\n\
                     annotation (or a matching built-in template); override with --queries.\n\n\
                     By default the input is rewritten in place; pass -o/--output to write a new\n\
                     file and leave the original untouched.\n\n\
                     EXAMPLES:\n    \
                     # Filter in place using the file's embedded KPIs\n    \
                     rezolus parquet filter rezolus.parquet\n\n    \
                     # Write a slimmed copy, keeping the original\n    \
                     rezolus parquet filter rezolus.parquet -o slim.parquet\n\n    \
                     # Filter to the columns a custom KPI set needs\n    \
                     rezolus parquet filter rezolus.parquet --queries ext.json -o slim.parquet",
                )
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
                )
                .arg(
                    clap::Arg::new("templates")
                        .long("templates")
                        .value_name("DIR")
                        .help("Directory containing service extension template JSON files")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("samplers")
                        .long("samplers")
                        .value_name("A,B,...")
                        .help("For .rez archives: comma-separated sampler names to KEEP; all other per-sampler tables are dropped (required for .rez, ignored for parquet). Only v2 (tar) .rez archives can be filtered today; a v3 (SQLite) archive errors clearly.")
                        .value_parser(value_parser!(String))
                        .action(clap::ArgAction::Set),
                ),
        )
        .subcommand(
            Command::new("split-groups")
                .hide(true)
                .about(
                    "DEV: rewrite a v3 .rez into per-acquisition-group tables \
                     (read-cost measurement scaffolding; see \
                     docs/journal/2026-08-17-window-sidecar-cost.md)",
                )
                .arg(
                    clap::Arg::new("input")
                        .short('i')
                        .long("input")
                        .value_name("REZ")
                        .help("Input .rez file (v3/SQLite)")
                        .required(true)
                        .value_parser(value_parser!(PathBuf)),
                )
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output")
                        .value_name("REZ")
                        .help("Output .rez file to write")
                        .required(true)
                        .value_parser(value_parser!(PathBuf)),
                )
                .arg(
                    clap::Arg::new("wide")
                        .long("wide")
                        .action(clap::ArgAction::SetTrue)
                        .help("Identity re-encode (provenance-matched wide baseline)"),
                ),
        )
}

pub fn run(args: ArgMatches) {
    use crate::viewer::load_template_registry;

    let result = match args.subcommand() {
        Some(("annotate", sub_args)) => {
            let registry = load_template_registry(
                sub_args
                    .get_one::<PathBuf>("templates")
                    .map(|p| p.as_path()),
            );
            annotate::run(sub_args, &registry);
            return;
        }
        Some(("combine", sub_args)) => combine::run(sub_args),
        Some(("convert", sub_args)) => {
            convert::run(sub_args);
            return;
        }
        Some(("filter", sub_args)) => {
            let registry = load_template_registry(
                sub_args
                    .get_one::<PathBuf>("templates")
                    .map(|p| p.as_path()),
            );
            filter::run(sub_args, &registry);
            return;
        }
        Some(("metadata", sub_args)) => metadata::run(sub_args),
        Some(("split-groups", sub_args)) => {
            let input = sub_args.get_one::<PathBuf>("input").unwrap();
            let output = sub_args.get_one::<PathBuf>("output").unwrap();
            let mode = if sub_args.get_flag("wide") {
                split_groups::SplitMode::Wide
            } else {
                split_groups::SplitMode::Groups
            };
            split_groups::split_rez(input, output, &mode)
        }
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
        .set_max_row_group_row_count(Some(crate::parquet_metadata::MAX_ROW_GROUP_SIZE))
        .set_compression(parquet::basic::Compression::ZSTD(Default::default()))
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
