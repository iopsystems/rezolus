mod annotate;
pub(crate) mod combine;
mod convert;
mod events;
mod filter;
pub(crate) mod metadata;

use arrow::datatypes::SchemaRef;
use clap::{value_parser, ArgMatches, Command};
use parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};
use parquet::file::metadata::KeyValue;
use parquet::file::metadata::ParquetMetaData;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

pub fn command() -> Command {
    // `recording` is the name; `parquet` is kept as an alias because it is
    // what every existing script types. The rename is not cosmetic — these
    // subcommands stopped being parquet-specific once `.rez` arrived, and
    // `rezolus recording upgrade old.rez` describes neither its input nor its
    // output. A `.rez` holds parquet segments, but the file you hand these
    // commands is a recording.
    Command::new("recording")
        .alias("parquet")
        .about("Inspect and transform Rezolus recordings (.parquet files and .rez archives)")
        .long_about(
            "Offline operations on recordings produced by `rezolus record` or\n\
             `rezolus hindsight` — both plain `.parquet` files and `.rez` archives.\n\n\
             SUBCOMMANDS:\n    \
             metadata   Inspect a file's file-level/column metadata, schema, and geometry\n    \
             annotate   Embed service-extension KPIs, events, or source/node tags into a file\n    \
             combine    Merge multiple files (multi-node / multi-instance), assemble an A/B .rez, or build a legacy A/B tarball\n    \
             convert    Turn a raw msgpack recording (from `record -o out.raw`) into parquet\n    \
             filter     Drop columns not needed by a file's service-extension KPIs (shrink it)\n    \
             upgrade    Convert a v1/v2 (tar) `.rez` archive to the v3 (SQLite) container\n\n\
             Run `rezolus recording <subcommand> --help` for per-subcommand examples.\n\n\
             `rezolus parquet ...` still works and does the same thing.",
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("annotate")
                .about("Add service extension KPIs, events or source/node tags to a recording (.parquet or .rez)")
                .long_about(
                    "Rewrite a parquet recording in place, adding metadata the viewer reads.\n\
                     Use it to attach service-extension KPI dashboards, tag the file's\n\
                     source/node identity, embed a systeminfo blob, or record timeline events.\n\n\
                     By default KPIs come from the built-in template matching the file's source;\n\
                     override with --queries <file.json>. --undo strips a prior annotation.\n\n\
                     A .rez archive is also accepted: --queries embeds a ServiceExtension (KPIs)\n\
                     and the event flags (--event/--add-events/--clear-events) embed timeline\n\
                     events, both into each recording's manifest metadata. Since a .rez is\n\
                     source=rezolus with no built-in template, at least one of --queries or the\n\
                     event flags is required. Either container works, and what is left behind is\n\
                     always v3 (SQLite): annotating is a rewrite, so a v1/v2 tar archive is\n\
                     upgraded on the way through and says so.\n\n\
                     EXAMPLES:\n    \
                     # Attach KPIs from the built-in template for this file's source\n    \
                     rezolus recording annotate rezolus.parquet\n\n    \
                     # Attach KPIs from a custom service-extension JSON file\n    \
                     rezolus recording annotate rezolus.parquet --queries ext.json\n\n    \
                     # Attach KPIs and also drop columns the KPIs don't need\n    \
                     rezolus recording annotate rezolus.parquet --queries ext.json --filter\n\n    \
                     # Set the source name on a file that has none\n    \
                     rezolus recording annotate service.parquet --source vllm\n\n    \
                     # Add a single timeline event\n    \
                     rezolus recording annotate rezolus.parquet --event 'time=2026-05-12T15:23Z,kind=restart,description=\"deploy\"'\n\n    \
                     # Remove a previously added annotation\n    \
                     rezolus recording annotate rezolus.parquet --undo\n\n    \
                     # Embed KPIs into each recording of a .rez archive\n    \
                     rezolus recording annotate out.rez --queries kpis.json\n\n    \
                     # Add a timeline event to each recording of a .rez archive\n    \
                     rezolus recording annotate out.rez --event 'time=2026-05-12T15:23Z,kind=deploy,description=\"rollout\"'",
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
                .about("Combine recordings: multi-node/multi-instance parquet, a multi-recording .rez, or an A/B tarball (legacy)")
                .long_about(
                    "Merge two or more parquet recordings into a single file. Requires at least\n\
                     two inputs and an output path (-o).\n\n\
                     Default (row-merge): joins the inputs on timestamp into one recording — use\n\
                     it to stitch together multiple rezolus nodes and/or per-instance service\n\
                     files so the viewer shows them together.\n\n\
                     A/B compare — PREFER a `.rez` output: give a `.rez` output path and combine\n\
                     assembles the inputs into one multi-recording archive, a recording per\n\
                     input, which the viewer renders as a baseline/experiment comparison for two\n\
                     recordings. This is the recommended A/B form: it identifies the sides by\n\
                     their LABEL SETS rather than a `baseline=<source>` mapping you must look up.\n\
                     `.rez` inputs keep each side's per-sampler cadence and real acquisition\n\
                     windows (v1/v2 tar inputs are upgraded to v3 on the way in). `.parquet`\n\
                     inputs are ingested too — each becomes one recording, split into a table\n\
                     per sampler; a parquet has no acquisition windows, so those recordings are\n\
                     WINDOWLESS and the viewer/query engine shows no rate uncertainty band for\n\
                     them (the honest outcome — the windows were never recorded). Inputs must be\n\
                     all `.rez` or all `.parquet`, not a mix.\n\n\
                     --ab (tarball, legacy): packages exactly two parquet captures unmodified\n\
                     into a combined-A/B tarball. The output should end in `.parquet.ab.tar`, and\n\
                     you map each side with `baseline=<src> experiment=<src>` where <src> is a\n\
                     file's embedded SOURCE NAME (not its filename) — set with `annotate\n\
                     --source`, seen with `recording metadata --field source`. Superseded by a\n\
                     `.rez` output (above), which needs no source mapping; kept for readers that\n\
                     only speak the tarball.\n\n\
                     EXAMPLES:\n    \
                     # Row-merge a rezolus agent file with a service file\n    \
                     rezolus recording combine rezolus.parquet service.parquet -o combined.parquet\n\n    \
                     # Merge several rezolus nodes, pinning which one the viewer shows first\n    \
                     rezolus recording combine node1.parquet node2.parquet -o cluster.parquet --pinned node1\n\n    \
                     # A/B compare two rezolus .rez captures (PREFERRED): a 2-recording .rez\n    \
                     rezolus recording combine baseline.rez experiment.rez -o ab.rez\n\n    \
                     # A/B compare two parquet captures into one .rez (windowless recordings)\n    \
                     rezolus recording combine redis.parquet valkey.parquet -o ab.rez\n\n    \
                     # A/B tarball (legacy) — a combined tarball rather than a .rez\n    \
                     rezolus recording combine a.parquet b.parquet --ab baseline=redis experiment=valkey -o out.parquet.ab.tar",
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
                            "Legacy A/B form: package two captures into a \
                             combined-A/B tarball instead of row-merging into one \
                             parquet. Prefer assembling a 2-recording `.rez` \
                             (`combine baseline.rez experiment.rez -o ab.rez`) for \
                             rezolus captures; use `--ab` only for parquet inputs \
                             with no `.rez` form. The output path should end in \
                             `.parquet.ab.tar`. Requires exactly two input files. \
                             Pass `baseline=<src> experiment=<src>` mapping each \
                             side to one of the inputs' source names; the captures \
                             are stored unmodified next to an `ab.json` manifest.",
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
                .about("Display file and column metadata for a recording (.parquet, or a .rez manifest)")
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
                     rezolus recording metadata -i rezolus.parquet\n\n    \
                     # Only file-level metadata, as JSON\n    \
                     rezolus recording metadata -i rezolus.parquet --file --json\n\n    \
                     # Just the value of one metadata key\n    \
                     rezolus recording metadata -i rezolus.parquet --field source\n\n    \
                     # Describe a .rez archive's manifest\n    \
                     rezolus recording metadata -i out.rez",
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
                    "Convert a recording made with `rezolus record -o out.raw` (concatenated msgpack\n\
                     snapshots, one per sampling tick) into a parquet file that the viewer, the\n\
                     MCP tools and the rest of `rezolus recording` can read.\n\n\
                     Raw is the cheapest capture mode: the recorder appends snapshots as they\n\
                     arrive and finalizing is a byte copy rather than a conversion that grows\n\
                     with run length, so long unattended captures record raw and convert\n\
                     afterwards. Recording straight to parquet (`record -o out.parquet`) is simpler\n\
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
                     be curled now, at conversion time, to recover them. Afterwards, `rezolus recording annotate\n\
                     <file> --systeminfo <path>` can still add the hardware summary, but there is\n\
                     no annotate route for descriptions: those can only be supplied here, which\n\
                     means reconverting the original raw input over the output (--force).\n\n\
                     A .rez archive cannot be produced from a raw recording: it needs per-sampler\n\
                     cadence and acquisition windows that a raw snapshot stream never carried.\n\n\
                     EXAMPLES:\n    \
                     # The producing side, for reference\n    \
                     rezolus record --url http://localhost:4241 -o rezolus.raw\n\n    \
                     # Convert a compressed recording (writes rezolus.parquet)\n    \
                     rezolus recording convert rezolus.raw.zst\n\n    \
                     # Choose the output path\n    \
                     rezolus recording convert rezolus.raw -o run7.parquet\n\n    \
                     # A recording made at a non-default cadence\n    \
                     rezolus recording convert rezolus.raw --interval 250ms\n\n    \
                     # Save the two metadata blobs at record time, from the agent being recorded\n    \
                     curl -s http://localhost:4241/systeminfo > sysinfo.json\n    \
                     curl -s http://localhost:4241/metrics/descriptions > help.json\n\n    \
                     # ...then stamp them into the conversion\n    \
                     rezolus recording convert rezolus.raw --systeminfo sysinfo.json --descriptions help.json\n\n    \
                     # Or pipe one of them straight in, with - for stdin\n    \
                     curl -s http://localhost:4241/systeminfo | rezolus recording convert rezolus.raw --systeminfo -\n\n    \
                     # Tag the recording the way `record --metadata` would\n    \
                     rezolus recording convert rezolus.raw -m source=llm-perf -m run=boat-7\n\n    \
                     # Replace an output left by an earlier attempt\n    \
                     rezolus recording convert rezolus.raw --force\n\n    \
                     # Descriptions were missed the first time -- reconvert over the old output\n    \
                     rezolus recording convert rezolus.raw --descriptions help.json --force",
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
                .about("Shrink a recording to what its KPIs need: parquet columns, or .rez samplers")
                .long_about(
                    "Shrink a recording by dropping what its service-extension KPIs do not need.\n\n\
                     For a parquet file that means metric COLUMNS: the KPI set comes from the\n\
                     file's embedded annotation (or a matching built-in template), and --queries\n\
                     overrides it.\n\n\
                     For a .rez archive there is no KPI column set to filter against (it is\n\
                     all-rezolus data), so you name what to keep directly: --samplers keeps whole\n\
                     SAMPLERS (a sampler's group tables go together) and --metrics keeps metric\n\
                     COLUMNS (re-encoding each table's segments, always keeping the timestamp and\n\
                     acquisition-window sidecars; a table left with none of the kept metrics is\n\
                     dropped). Use either alone or together; at least one is required, and both\n\
                     are ignored for parquet. Either container works and the output is always v3\n\
                     (SQLite).\n\n\
                     By default the input is rewritten in place; pass -o/--output to write a new\n\
                     file and leave the original untouched.\n\n\
                     EXAMPLES:\n    \
                     # Filter in place using the file's embedded KPIs\n    \
                     rezolus recording filter rezolus.parquet\n\n    \
                     # Write a slimmed copy, keeping the original\n    \
                     rezolus recording filter rezolus.parquet -o slim.parquet\n\n    \
                     # Filter to the columns a custom KPI set needs\n    \
                     rezolus recording filter rezolus.parquet --queries ext.json -o slim.parquet\n\n    \
                     # Keep only two samplers of a .rez, writing a slimmed copy\n    \
                     rezolus recording filter out.rez --samplers cpu_usage,scheduler -o slim.rez\n\n    \
                     # Keep only specific metric columns of a .rez\n    \
                     rezolus recording filter out.rez --metrics cpu_usage,cpu_frequency -o slim.rez",
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("Recording to filter: a .parquet file, or a .rez archive (which needs --samplers)")
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
                        .help("For .rez archives: comma-separated sampler names to KEEP; every other sampler's tables are dropped (ignored for parquet). Filtering is by sampler, so a sampler's group tables go together. Either container works; the output is always v3 (SQLite)")
                        .value_parser(value_parser!(String))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("metrics")
                        .long("metrics")
                        .value_name("A,B,...")
                        .help("For .rez archives: comma-separated metric names to KEEP as COLUMNS; every other metric's columns are dropped and re-encoded (ignored for parquet). Timestamp and acquisition-window sidecars are always kept; a table left with none of the kept metrics is dropped. Combine with --samplers, or use either alone (at least one is required for a .rez)")
                        .value_parser(value_parser!(String))
                        .action(clap::ArgAction::Set),
                ),
        )
        .subcommand(
            Command::new("upgrade")
                .about("Upgrade a v1/v2 (tar) .rez archive to the v3 (SQLite) container")
                .long_about(
                    "Rewrite a v1 or v2 `.rez` (a tar archive) as a v3 `.rez` (a single\n\
                     SQLite file), the container the recorder and hindsight write today.\n\n\
                     Segment parquet BLOBs are carried across byte-for-byte: the container\n\
                     changes, the data does not. Labels, per-recording metadata and the\n\
                     `complete` flag come with them, so an archive recovered from a\n\
                     checkpoint still reads as recovered rather than being laundered into a\n\
                     clean one.\n\n\
                     `combine`, `filter` and `annotate` already upgrade a tar input on the\n\
                     way through. This is for upgrading an archive on its own, without\n\
                     otherwise changing it. A v3 input is refused rather than copied:\n\
                     \"upgrade\" on something already current is far more likely a mistaken\n\
                     path than a request for a duplicate.\n\n\
                     EXAMPLES:\n    \
                     # Upgrade in place\n    \
                     rezolus recording upgrade old.rez\n\n    \
                     # ...or leave the original alone\n    \
                     rezolus recording upgrade old.rez -o new.rez",
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("The .rez archive to upgrade")
                        .required(true)
                        .value_parser(value_parser!(PathBuf)),
                )
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output")
                        .value_name("REZ")
                        .help("Write here instead of replacing the input in place")
                        .value_parser(value_parser!(PathBuf)),
                ),
        )
        .subcommand(
            Command::new("snapshot")
                .about("Copy a .rez that is still being written, without stopping the writer")
                .long_about(
                    "Take a complete, standalone copy of a `.rez` archive while a recorder\n\
                     or `hindsight` is still appending to it.\n\n\
                     WHY NOT `cp`: a v3 `.rez` is a SQLite database in WAL mode, so recent\n\
                     commits live in a `<file>-wal` SIDECAR until they are checkpointed into\n\
                     the archive. Copying the archive alone leaves them behind — measured at\n\
                     123 ticks (~2 minutes at a 1s interval) on a 2000-tick recording, with\n\
                     a sidecar larger than the archive itself. The copy is not corrupt, it\n\
                     is simply older, which is the worse failure: it reads as a recording\n\
                     that stopped early.\n\n\
                     This reads the archive the way every other reader does — sidecar\n\
                     included — and writes one consistent file. The writer is never paused\n\
                     and the original is not modified.\n\n\
                     The result is what to hand to someone else, upload to the static-site\n\
                     viewer (a browser is given one file and cannot see a sidecar), or keep\n\
                     as an incident artifact. A v1/v2 tar archive has no sidecar, so this is\n\
                     a plain copy for those.\n\n\
                     EXAMPLES:\n    \
                     # Grab the last N minutes of a running hindsight buffer\n    \
                     rezolus recording snapshot /var/lib/rezolus/hindsight.rez -o incident.rez\n\n    \
                     # ...or of a `rezolus record` still in progress\n    \
                     rezolus recording snapshot rezolus.rez -o so-far.rez",
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("The .rez archive to snapshot (may be open for writing)")
                        .required(true)
                        .value_parser(value_parser!(PathBuf)),
                )
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output")
                        .value_name("REZ")
                        .required(true)
                        .help("Where to write the snapshot. Must not already exist.")
                        .value_parser(value_parser!(PathBuf)),
                ),
        )
}

/// Copy a live `.rez` into one complete standalone file.
///
/// **The point is SQLite's `-wal` sidecar.** A v3 archive is a SQLite database
/// in WAL mode: commits land in `<file>-wal` and are checkpointed into the
/// archive in batches, so the archive on its own is a consistent view as of
/// the last checkpoint and nothing newer. `cp` takes that older view — and it
/// is not detectably wrong, it just ends early. `VACUUM INTO` reads through a
/// normal read transaction (sidecar included, writer un-paused) and writes a
/// single file with everything in it.
///
/// A v1/v2 tar archive has no sidecar and no writer that appends in place, so
/// there the honest implementation is a plain copy.
fn snapshot_rez(
    path: &std::path::Path,
    output: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::recorder::rez::{detect_rez_format, RezFormat};

    if output.exists() {
        return Err(format!("{} already exists", output.display()).into());
    }
    match detect_rez_format(path).unwrap_or(RezFormat::NotRez) {
        RezFormat::V3Sqlite => {
            crate::recorder::rez_sqlite::RezDb::open(path)?.vacuum_into(output)?;
        }
        RezFormat::V2Tar => {
            std::fs::copy(path, output)?;
        }
        RezFormat::NotRez => {
            return Err(format!("{} is not a .rez archive", path.display()).into());
        }
    }
    println!("Wrote {}", output.display());
    Ok(())
}

/// Rewrite a v1/v2 (tar) `.rez` as a v3 (SQLite) one.
///
/// Refuses a v3 input rather than silently copying it: "upgrade" on something
/// already current is far more likely a mistaken path than a request for a
/// byte-for-byte duplicate.
fn upgrade_rez(
    path: &std::path::Path,
    output: Option<&std::path::Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::recorder::rez::{detect_rez_format, RezFormat};

    match detect_rez_format(path).unwrap_or(RezFormat::NotRez) {
        RezFormat::V2Tar => {}
        RezFormat::V3Sqlite => {
            return Err(format!(
                "{} is already a v3 (SQLite) archive; there is nothing to upgrade",
                path.display()
            )
            .into())
        }
        RezFormat::NotRez => return Err(format!("{} is not a .rez archive", path.display()).into()),
    }

    // Staged beside the destination so the rename that publishes it is atomic
    // and on the same filesystem, and so an in-place upgrade never leaves a
    // half-written archive where the original was.
    let dest = output.unwrap_or(path);
    let dir = dest.parent().filter(|p| !p.as_os_str().is_empty());
    let staging = match dir {
        Some(dir) => tempfile::tempdir_in(dir),
        None => tempfile::tempdir(),
    }?;
    let staged = staging.path().join("upgraded.rez");

    let recordings = crate::recorder::rez_v3_rewrite::upgrade_tar_to_v3(path, &staged)?;
    std::fs::rename(&staged, dest)?;
    println!(
        "upgraded {:?} to a v3 (SQLite) archive: {} recording(s)",
        dest, recordings
    );
    Ok(())
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
        Some(("upgrade", sub_args)) => {
            let path = sub_args.get_one::<PathBuf>("FILE").unwrap();
            let output = sub_args.get_one::<PathBuf>("output").map(|p| p.as_path());
            upgrade_rez(path, output)
        }
        Some(("snapshot", sub_args)) => {
            let path = sub_args.get_one::<PathBuf>("FILE").unwrap();
            let output = sub_args.get_one::<PathBuf>("output").unwrap();
            snapshot_rez(path, output)
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

#[cfg(test)]
mod command_name_tests {
    /// `parquet` keeps working as an alias for `recording`.
    ///
    /// The whole reason `snapshot` exists: a plain copy of a live v3 archive
    /// silently ends early, because SQLite's `-wal` sidecar holds commits not
    /// yet checkpointed into the archive. Measured at 123 ticks behind on a
    /// 2000-tick recording — with a sidecar LARGER than the archive.
    ///
    /// So this asserts the difference, not just that the snapshot opens: the
    /// snapshot must reach the last tick, and the `cp` must not. If a future
    /// change made a plain copy complete (an aggressive checkpoint, say), the
    /// second half fails and this command's rationale needs revisiting —
    /// which is exactly when someone should look.
    #[test]
    fn a_snapshot_carries_what_a_plain_copy_leaves_in_the_sidecar() {
        use ::rez::rez::recorder_tests_support::{counter, snap};
        use ::rez::rez_v3_writer::{ManifestSeed, RezArchive, StreamRecorderV3};

        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.rez");
        let seed = ManifestSeed {
            labels: [("source".to_string(), "rezolus".to_string())]
                .into_iter()
                .collect(),
            metadata: Default::default(),
            clock_anchor_wall_ns: 1_000_000_000,
        };
        let (archive, writer) = RezArchive::single(&live, seed).unwrap();
        let mut rec = StreamRecorderV3::new(writer);

        // Enough ticks, wide enough, to push past the WAL autocheckpoint and
        // leave a real sidecar behind. A handful of rows would all still be in
        // the sidecar and the copy would have no catalog at all — a different
        // (and already covered) failure.
        let ticks = 2_000u64;
        for t in 0..ticks {
            let ts = 1_000_000_000 * (t + 1);
            let cells: Vec<_> = (0..40)
                .map(|m| counter(&format!("m{m}"), "cpu_usage", t + m, None))
                .collect();
            rec.ingest(&snap(ts, cells), ts, 0).unwrap();
        }
        rec.sync().unwrap();

        let last_ts = 1_000_000_000 * ticks;
        let snapshot = dir.path().join("snap.rez");
        super::snapshot_rez(&live, &snapshot).unwrap();
        let plain_copy = dir.path().join("copy.rez");
        std::fs::copy(&live, &plain_copy).unwrap();

        // How far each one's newest row reaches.
        let reach = |p: &std::path::Path| -> Option<u64> {
            let db = ::rez::rez_sqlite::RezDb::open(p).ok()?;
            let rec = db.read_recordings().ok()?.into_iter().next()?;
            db.live_wal_span(rec.id, "cpu_usage").ok()?.last_ts
        };

        assert_eq!(
            reach(&snapshot),
            Some(last_ts),
            "a snapshot must reach the last committed tick"
        );
        let copied = reach(&plain_copy).expect("the copy is readable, just older");
        assert!(
            copied < last_ts,
            "the premise of this command: a plain copy should be behind, but it \
             reached {copied} of {last_ts}"
        );

        drop(rec);
        drop(archive);
    }

    /// Refuses to overwrite: the output is an incident artifact, and clobbering
    /// one silently is worse than making the caller pick a name.
    #[test]
    fn a_snapshot_will_not_overwrite_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.rez");
        ::rez::rez::recorder_tests_support::empty_v3_rez(&live);
        let out = dir.path().join("taken.rez");
        std::fs::write(&out, b"do not clobber me").unwrap();

        let err = super::snapshot_rez(&live, &out).expect_err("must refuse");
        assert!(err.to_string().contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&out).unwrap(), b"do not clobber me");
    }

    /// The rename is the point — these subcommands stopped being
    /// parquet-specific once `.rez` arrived — but every existing script and
    /// runbook types `rezolus recording ...`, and breaking those to make a name
    /// tidier is not a trade worth making. Both spellings must reach the same
    /// subcommand, and clap must resolve the alias to the canonical name so
    /// `main`'s dispatch needs only one arm.
    #[test]
    fn parquet_still_reaches_the_recording_subcommand() {
        let app = clap::Command::new("rezolus").subcommand(super::command());

        for spelling in ["recording", "parquet"] {
            let m = app
                .clone()
                .try_get_matches_from(["rezolus", spelling, "metadata", "-i", "x.parquet"])
                .unwrap_or_else(|e| panic!("`rezolus {spelling} metadata` must parse: {e}"));
            let (name, sub) = m.subcommand().expect("a subcommand matched");
            assert_eq!(
                name, "recording",
                "clap must report the canonical name for `{spelling}`, so `main` \
                 dispatches on one arm rather than two"
            );
            assert_eq!(sub.subcommand_name(), Some("metadata"));
        }
    }
}
