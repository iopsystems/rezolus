pub(crate) mod annotate;
pub(crate) mod check;
pub(crate) mod combine;
mod convert;
pub(crate) mod events;
mod filter;
pub(crate) mod legacy;
pub(crate) mod metadata;

use arrow::datatypes::SchemaRef;
use clap::{value_parser, ArgMatches, Command};
use parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};
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
        .about("Inspect and transform Rezolus recordings (.dendro and .rez archives, .parquet files)")
        .long_about(
            "Offline operations on recordings: the .dendro archives `rezolus record` and\n\
             `rezolus hindsight` write, and the .rez archives and .parquet files that 5.x\n\
             releases wrote.\n\n\
             SUBCOMMANDS:\n    \
             metadata   Inspect a recording's metadata, schema, and geometry\n    \
             annotate   Embed service-extension KPIs or events into a recording\n    \
             check      Evaluate the KPI checks a recording carries and report pass/fail verdicts\n    \
             combine    Combine recordings into one .dendro archive, optionally labelled as an A/B\n    \
             convert    Turn a raw msgpack recording (from a 5.x `record -o out.raw`) into a .dendro\n    \
             filter     Shrink a recording to the samplers or metrics named\n    \
             snapshot   Copy an archive that is still being written into a complete .dendro\n    \
             upgrade    Convert a .rez archive or a .parquet file into a .dendro archive\n\n\
             6.x writes .dendro archives only. A command that would change a .rez or a\n\
             .parquet writes a new .dendro instead, given -o.\n\n\
             Run `rezolus recording <subcommand> --help` for per-subcommand examples.\n\n\
             `rezolus parquet ...` still works and does the same thing.",
        )
        .subcommand_required(true)
        .subcommand(
            Command::new("annotate")
                .about("Add service extension KPIs or events to a recording (.dendro; a .rez or .parquet into a new .dendro)")
                .long_about(
                    "Embed metadata the viewer reads into each recording of a .dendro archive,\n\
                     in place: service-extension KPIs (--queries) and timeline events\n\
                     (--event/--add-events/--clear-events). At least one is required.\n\n\
                     A .rez archive or a .parquet file is not changed: 6.x writes .dendro only.\n\
                     Give -o <new.dendro> and the recording is converted there and the edit\n\
                     written to the copy; without -o the command is refused, naming the command\n\
                     to run. A .dendro with -o is copied there and the copy edited.\n\n\
                     --undo, --node, --source, --systeminfo and --filter edited a parquet file's\n\
                     footer, which 6.x no longer writes; they are accepted and ignored with a\n\
                     warning.\n\n\
                     EXAMPLES:\n    \
                     # Embed KPIs into each recording of an archive\n    \
                     rezolus recording annotate run.dendro --queries kpis.json\n\n    \
                     # Add a timeline event\n    \
                     rezolus recording annotate run.dendro --event 'time=2026-05-12T15:23Z,kind=deploy,description=\"rollout\"'\n\n    \
                     # Annotate a 5.x recording: the result is a new .dendro\n    \
                     rezolus recording annotate old.rez --queries kpis.json -o old.dendro",
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("The recording to annotate (.dendro, or a .rez or .parquet with -o)")
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output")
                        .value_name("PATH")
                        .help("Write the annotated recording to this new .dendro instead of in place (required for a .rez or .parquet input)")
                        .value_parser(value_parser!(PathBuf)),
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
                        .help("Add a single event inline, e.g. 'time=2026-05-12T15:23Z,kind=restart,description=\"...\"'. \
                               Add duration=30s (or duration_ns=N) for a range that the viewer shades from time to \
                               time+duration. Repeatable.")
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
            Command::new("check")
                .about("Evaluate the checks a recording's KPIs carry and report a verdict per check")
                .long_about(
                    "Run every KPI check over a recording and print one line per check\n\
                     (PASS / WARN / FAIL / INDETERMINATE / ERROR), then a summary line.\n\n\
                     THE FILE: checks live on KPIs in a service-extension JSON file, the same\n\
                     file `recording annotate --queries` embeds for the viewer's KPI dashboard:\n\n    \
                     {\n      \
                       \"service_name\": \"myservice\",\n      \
                       \"kpis\": [\n        \
                         {\"role\": \"latency\", \"title\": \"p99 request latency\",\n         \
                          \"query\": \"request_latency_seconds\", \"type\": \"histogram\",\n         \
                          \"check\": {\"above\": 0.25, \"quantile\": 0.99, \"for\": \"10s\"}},\n        \
                         {\"role\": \"throughput\", \"title\": \"Request rate\",\n         \
                          \"query\": \"sum(irate(http_requests_total[10s]))\", \"type\": \"delta_counter\",\n         \
                          \"check\": {\"below\": 100, \"severity\": \"warn\"}}\n      \
                       ]\n    \
                     }\n\n\
                     `service_name` (required) names the service; `aliases` (a list) and\n\
                     `service_metadata` (a map) are optional. Each KPI needs `role` (any word;\n\
                     it groups charts on the dashboard), `title` (keep it unique in the file:\n\
                     it names the check in output and events), `query` (PromQL) and `type`:\n\
                     `gauge`, `histogram`, or a counter kind (`delta_counter`, `counter`).\n\
                     Any other `type` is charted as a counter and evaluated as written.\n\
                     Optional: `description`, `unit_system`, `subtype`, `percentiles`,\n\
                     `subgroup`, `subgroup_description`, `full_width`, `denominator`, `check`.\n\
                     A KPI without `check` is a chart only and is skipped here.\n\n\
                     THE CHECK: exactly one of `above` / `below` is the threshold, compared in\n\
                     the query's own unit (seconds for the histogram above, requests per\n\
                     second for the rate); the comparison is strict, so a value equal to the\n\
                     threshold passes. `for` (default 0s) is the shortest run that counts,\n\
                     written as 500ms, 10s, 1m30s, 2h, 1d, 1.5s, or a bare number of seconds.\n\
                     `severity` is `fail` (default) or `warn`. A histogram KPI needs `quantile`\n\
                     in (0, 1]: its query names the raw histogram and the check evaluates\n\
                     histogram_quantile(<quantile>, <query>) itself, so leave\n\
                     histogram_quantile out of the query. `quantile` on any other type is an\n\
                     ERROR. Unknown keys inside `check` are rejected.\n\n\
                     WHICH CHECKS RUN: the KPIs embedded in the recording (by\n\
                     `recording annotate --queries`); else the built-in template whose\n\
                     `service_name` or `aliases` match the recording's `source` metadata, the\n\
                     same lookup the viewer makes. --templates <DIR> uses the template JSON\n\
                     files in DIR instead of the built-in set for that fallback. --queries\n\
                     <file> runs that file's checks instead of either, and writes nothing.\n\n\
                     HOW A CHECK IS EVALUATED: the query runs over the whole recording through\n\
                     the same engine as `rezolus mcp query`, on a grid of one point per step\n\
                     (the recording's sampling interval, at most 1s), and must yield exactly\n\
                     one series (aggregate with sum(...) or add label matchers if it yields\n\
                     several). A query that matches nothing is an ERROR, not a pass. Each\n\
                     point is classified: where a value carries an acquisition-window\n\
                     uncertainty band (rate(), irate(), histogram quantiles) the band is\n\
                     compared, so `above` fires only when the whole band is above the\n\
                     threshold and `below` only when it is entirely below; a point whose band\n\
                     straddles the threshold, or one interpolated across a span the recording\n\
                     never observed, is INDETERMINATE. A run is consecutive points that do\n\
                     not pass; a passing point, or a gap of more than 1.5 steps between\n\
                     points (the engine emits no point where the recording has no data),\n\
                     ends it. A run violates only if every point in it violates; one\n\
                     indeterminate point makes the whole run indeterminate, so a long\n\
                     violation with an intermittent straddle is one long INDETERMINATE window\n\
                     rather than short violations that never reach `for`. The step is the grid\n\
                     step or the series' own point spacing, whichever is coarser (a 10 s\n\
                     sampler on a 1 s grid yields a point per 10 s, each spanning 10 s). A run's\n\
                     span is (last point - first point + step); it is a window when\n\
                     span >= for. A check with a violating window is FAIL (WARN for severity\n\
                     warn) even if it also has indeterminate windows; with only indeterminate\n\
                     windows it is INDETERMINATE; otherwise PASS.\n\n\
                     EXIT STATUS: 2 if any check could not be evaluated (or the command itself\n\
                     failed), else 1 if any check with severity `fail` failed, else 0. WARN and\n\
                     INDETERMINATE never fail the run. No checks at all is 0, with a note on\n\
                     stderr.\n\n\
                     MULTI-RECORDING ARCHIVE: with no --recording every recording is checked and\n\
                     each line is prefixed with its labels; --recording k=v (the selector\n\
                     `rezolus mcp` uses) narrows to one. A recording and its copies (one\n\
                     source copied, or recorded twice from one agent process) are one\n\
                     recording.\n\n\
                     --annotate writes each FAIL/WARN window into the recording as a range\n\
                     event: kind=check, timestamp=window start, duration_ns=span (the event\n\
                     ends one step after the last violating point), description=the KPI title,\n\
                     details=the condition and, on a second line, the check as JSON. The event\n\
                     id is a hash of the title, the evaluated query, the condition and the\n\
                     window start: running again rewrites a window that grew and adds nothing\n\
                     for one that did not, and a window that no longer fires keeps its old\n\
                     event. The events go into the recording they were evaluated against (each\n\
                     recording its own when no --recording is given), in its sources' metadata.\n\
                     A .dendro is annotated in place, or into a copy with -o <new.dendro>. A .rez\n\
                     or .parquet is not changed: give -o <new.dendro> and it is converted there\n\
                     and the copy annotated; without -o the annotation is refused, naming the\n\
                     command to run. With --json the annotation report goes to stderr so stdout\n\
                     stays one JSON array.\n\n\
                     --json prints an array with one object per check: `recording` (labels,\n\
                     .rez only), `title`, `query` (as evaluated), `check`, `status`, `windows`\n\
                     and `indeterminate` (each with `start` and `end` as RFC 3339, `start_ns`,\n\
                     `duration_ns`, `points`), and `error` (ERROR only).\n\n\
                     EXAMPLES:\n    \
                     # Run the checks a recording already carries\n    \
                     rezolus recording check run.dendro\n\n    \
                     # Run checks from a file instead (nothing is written to the recording)\n    \
                     rezolus recording check run.parquet --queries kpis.json\n\n    \
                     # CI: fail the job on a violated check, and store the verdicts as events\n    \
                     rezolus recording check run.dendro --annotate || exit 1\n\n    \
                     # One recording of a multi-recording archive, machine-readable\n    \
                     rezolus recording check fleet.dendro --recording host=web-01 --json\n\n    \
                     # Annotate a 5.x recording's verdicts into a new .dendro\n    \
                     rezolus recording check old.rez --annotate -o old.dendro",
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("The recording to check (.parquet or .rez, told apart by content)")
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("queries")
                        .long("queries")
                        .value_name("PATH")
                        .help("Service-extension JSON whose KPI checks to run, instead of the recording's own")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("RECORDING")
                        .long("recording")
                        .value_name("KEY=VALUE")
                        .help(
                            "Which recording of a multi-recording .rez to check, as key=value (e.g. --recording source=redis)\n\
                             Repeatable: the pairs are ANDed and must together name exactly one\n\
                             recording. Without it every recording is checked. Not accepted for\n\
                             a .parquet file.",
                        )
                        .action(clap::ArgAction::Append),
                )
                .arg(
                    clap::Arg::new("annotate")
                        .long("annotate")
                        .help("Write each FAIL/WARN window into the recording as a kind=check range event")
                        .action(clap::ArgAction::SetTrue),
                )
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output")
                        .value_name("PATH")
                        .help("With --annotate, write the events into this new .dendro instead of the recording (required for a .rez or .parquet input)")
                        .requires("annotate")
                        .value_parser(value_parser!(PathBuf)),
                )
                .arg(
                    clap::Arg::new("json")
                        .long("json")
                        .help("Print the verdicts as a JSON array instead of one line per check")
                        .action(clap::ArgAction::SetTrue),
                )
                .arg(
                    clap::Arg::new("templates")
                        .long("templates")
                        .value_name("DIR")
                        .help("Directory of service-extension template JSON files to use instead of the built-in set, for a recording with no embedded KPIs (matched to the recording's source metadata)")
                        .value_parser(value_parser!(PathBuf))
                        .action(clap::ArgAction::Set),
                ),
        )
        .subcommand(
            Command::new("combine")
                .about("Combine recordings into one multi-recording .dendro archive")
                .long_about(
                    "Assemble two or more recordings into one .dendro archive, a recording per\n\
                     input recording, which the viewer opens as one combined view. Requires at\n\
                     least two inputs (one archive with --ab, below) and an output (-o) ending\n\
                     in .dendro. 6.x writes .dendro only: an output ending in .parquet (the\n\
                     row-merge), .rez or .parquet.ab.tar is refused.\n\n\
                     Inputs may be any mix of .dendro archives, .rez archives (v1, v2 or v3)\n\
                     and .parquet files; a .rez or parquet input is converted to dendro on the\n\
                     way in, and is not changed. A parquet file becomes one recording labelled\n\
                     with its `source`. A recording and its copy (a snapshot, a hindsight dump)\n\
                     given together are read as one recording, as `rezolus view` reads them.\n\n\
                     With `--ab baseline=<input> experiment=<input>` each input's recordings\n\
                     are labelled arm=baseline or arm=experiment and the viewer opens a\n\
                     baseline/experiment comparison; an input recording that already carries\n\
                     the other arm is refused, naming the input. With ONE archive input,\n\
                     `--ab baseline=0 experiment=1` labels its recordings by position (from 0,\n\
                     in the order `rezolus mcp describe-recording` lists them): the way to\n\
                     compare the two recordings of an archive combined before --ab existed,\n\
                     whose labels may be identical.\n\n\
                     --pinned, --category and --bypass-time-check applied to the parquet\n\
                     row-merge and the A/B tarball, which 6.x does not write; they are refused.\n\n\
                     EXAMPLES:\n    \
                     # Two hosts recorded separately in one run: one combined view\n    \
                     rezolus recording combine web-01.dendro web-02.dendro -o run.dendro\n\n    \
                     # A/B compare two captures\n    \
                     rezolus recording combine a.dendro b.dendro --ab baseline=a.dendro experiment=b.dendro -o ab.dendro\n\n    \
                     # 5.x captures: a .rez and two parquet files into one archive\n    \
                     rezolus recording combine agent.rez redis.parquet valkey.parquet -o run.dendro\n\n    \
                     # Label the two recordings of an older archive as an A/B, by position\n    \
                     rezolus recording combine old.rez --ab baseline=0 experiment=1 -o ab.dendro",
                )
                .arg(
                    clap::Arg::new("FILES")
                        .help("Input recordings: .dendro and .rez archives and .parquet files, in any mix")
                        .value_parser(value_parser!(PathBuf))
                        .required(true)
                        .num_args(1..)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output")
                        .help("The .dendro archive to write; must not exist")
                        .value_parser(value_parser!(PathBuf))
                        .required(true),
                )
                .arg(
                    clap::Arg::new("bypass-time-check")
                        .long("bypass-time-check")
                        .help("Applied to the parquet row-merge, which 6.x does not write; refused")
                        .action(clap::ArgAction::SetTrue),
                )
                .arg(
                    clap::Arg::new("pinned")
                        .long("pinned")
                        .help("Applied to the parquet row-merge, which 6.x does not write; refused")
                        .value_parser(clap::value_parser!(String)),
                )
                .arg(
                    clap::Arg::new("ab")
                        .long("ab")
                        .help(
                            "Mark the two inputs as the two arms of an A/B comparison: \
                             `--ab baseline=<input> experiment=<input>`. Each value names \
                             one input (the path as typed, or its file name), and every \
                             recording of that input is labelled arm=baseline or \
                             arm=experiment, so the viewer opens the archive as a \
                             comparison. An input recording that already carries a \
                             different arm is refused; the same arm is kept. With one \
                             archive input, each value is the position of one of its \
                             recordings. Without --ab the archive opens as one combined \
                             view.",
                        )
                        .value_parser(value_parser!(String))
                        .num_args(2)
                        .action(clap::ArgAction::Append),
                )
                .arg(
                    clap::Arg::new("category")
                        .long("category")
                        .value_name("NAME")
                        .help("Applied to the A/B tarball, which 6.x does not write; refused")
                        .value_parser(value_parser!(String))
                        .requires("ab"),
                ),
        )
        .subcommand(
            Command::new("metadata")
                .about("Display the metadata of a recording (.dendro or .rez archive, .parquet file)")
                .long_about(
                    "Describe a recording. For a .dendro or .rez archive: its recordings, their\n\
                     labels and metadata, and each table with its cadence; --json emits the\n\
                     description as JSON.\n\n\
                     For a .parquet file: file-level key/values (source, sampling interval,\n\
                     systeminfo, descriptions, …), the column schema with each metric's type and\n\
                     labels, and table geometry (row/column counts and row-group layout). With no\n\
                     filter flag all sections are shown. Narrow with --file, --schema, or\n\
                     --geometry; pull a single file-level value with --field <KEY>; add --json\n\
                     for machine-readable output.\n\n\
                     EXAMPLES:\n    \
                     # Describe an archive\n    \
                     rezolus recording metadata -i rezolus.dendro\n\n    \
                     # The same, as JSON\n    \
                     rezolus recording metadata -i rezolus.dendro --json\n\n    \
                     # Only a parquet file's file-level metadata, as JSON\n    \
                     rezolus recording metadata -i rezolus.parquet --file --json\n\n    \
                     # Just the value of one parquet metadata key\n    \
                     rezolus recording metadata -i rezolus.parquet --field source",
                )
                .arg(
                    clap::Arg::new("input")
                        .short('i')
                        .long("input")
                        .help("The recording (.dendro, .rez or .parquet)")
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
                .about("Convert a raw msgpack recording into a .dendro archive")
                .long_about(
                    "Convert a recording made with a 5.x `rezolus record -o out.raw` (concatenated\n\
                     msgpack snapshots, one per sampling tick) into a .dendro archive that the\n\
                     viewer, the MCP tools and the rest of `rezolus recording` can read. 6.x\n\
                     records .dendro only, so this is for raw recordings made before 6.0.\n\n\
                     The input may be plain or zstd-compressed; which one it is is detected from\n\
                     the file's contents, not its name. The output path defaults to the input\n\
                     with a trailing .zst and then a trailing .raw stripped, and .dendro\n\
                     appended: rezolus.raw.zst becomes rezolus.dendro, and a name with neither\n\
                     suffix just gains one (capture.msgpack becomes capture.msgpack.dendro). An\n\
                     -o that does not end in .dendro is refused.\n\n\
                     The archive holds one source labelled source=rezolus (or the value of\n\
                     --metadata source=...). The sampling interval stamped into its metadata is\n\
                     inferred from the median gap between snapshot timestamps unless --interval\n\
                     says otherwise. A recording with fewer than two snapshots falls back to 1s,\n\
                     so pass --interval if such a recording was made at another cadence.\n\
                     Inference warns on stderr when the sampled gaps have no dominant cadence, or\n\
                     when they are closer together than the whole milliseconds the stamped value\n\
                     can hold; the conversion still succeeds in both cases.\n\n\
                     A raw recording carries no systeminfo or metric descriptions: the recorder\n\
                     fetches those over HTTP from the running rezolus agent while recording (its\n\
                     /systeminfo and /metrics/descriptions endpoints) and they never enter the\n\
                     snapshot stream. Without them the conversion still succeeds and every metric\n\
                     still queries normally; what you lose is the viewer's hardware panel and the\n\
                     help text beside each metric. Stamp them here if you saved them -- and if\n\
                     that agent is still running, the same two endpoints can be curled now, at\n\
                     conversion time, to recover them. They can only be supplied here, which\n\
                     means reconverting the original raw input over the output (--force).\n\n\
                     EXAMPLES:\n    \
                     # The producing side, with a 5.x binary, for reference\n    \
                     rezolus record --url http://localhost:4241 -o rezolus.raw\n\n    \
                     # Convert a compressed recording (writes rezolus.dendro)\n    \
                     rezolus recording convert rezolus.raw.zst\n\n    \
                     # Choose the output path\n    \
                     rezolus recording convert rezolus.raw -o run7.dendro\n\n    \
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
                        .help("Output .dendro path (default: input with a trailing .zst then .raw stripped and .dendro appended). Refuses to overwrite an existing file unless --force")
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
                .about("Shrink a recording to the samplers or metrics named (.dendro; a .rez or .parquet into a new .dendro)")
                .long_about(
                    "Shrink a recording: --samplers keeps whole SAMPLERS (a sampler's group\n\
                     tables go together) and --metrics keeps metric COLUMNS (re-encoding each\n\
                     table's segments, always keeping the timestamp and acquisition-window\n\
                     sidecars; a table left with none of the kept metrics is dropped). Use\n\
                     either alone or together; at least one is required, except for a parquet\n\
                     file, which with neither keeps what its service-extension KPIs need: the\n\
                     metrics its KPI queries name (from the file's annotation, a matching\n\
                     built-in template, or --queries) and every metric of the agent's columns.\n\n\
                     A .dendro is rewritten in place, or with -o/--output written to a new\n\
                     .dendro, leaving the original untouched. A .rez or .parquet is not changed:\n\
                     6.x writes .dendro only, so it needs -o <new.dendro>; it is converted and\n\
                     the filtered copy written there.\n\n\
                     EXAMPLES:\n    \
                     # Keep only two samplers of a .dendro, writing a slimmed copy\n    \
                     rezolus recording filter capture.dendro --samplers cpu_usage,scheduler -o slim.dendro\n\n    \
                     # Keep only specific metric columns, in place\n    \
                     rezolus recording filter capture.dendro --metrics cpu_usage,cpu_frequency\n\n    \
                     # A 5.x .rez: the filtered recording is a new .dendro\n    \
                     rezolus recording filter out.rez --samplers cpu_usage -o slim.dendro\n\n    \
                     # A parquet file, keeping what its KPIs need\n    \
                     rezolus recording filter service.parquet --queries ext.json -o slim.dendro",
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("Recording to filter: a .dendro, or a .rez or .parquet with -o")
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
                        .help("Write the filtered recording to this .dendro (default: rewrite a .dendro input in place; required for a .rez or .parquet)")
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
                        .help("Comma-separated sampler names to KEEP; every other sampler's tables are dropped. Filtering is by sampler, so a sampler's group tables go together")
                        .value_parser(value_parser!(String))
                        .action(clap::ArgAction::Set),
                )
                .arg(
                    clap::Arg::new("metrics")
                        .long("metrics")
                        .value_name("A,B,...")
                        .help("Comma-separated metric names to KEEP as COLUMNS; every other metric's columns are dropped and re-encoded. Timestamp and acquisition-window sidecars are always kept; a table left with none of the kept metrics is dropped. Combine with --samplers, or use either alone")
                        .value_parser(value_parser!(String))
                        .action(clap::ArgAction::Set),
                ),
        )
        .subcommand(
            Command::new("upgrade")
                .about("Convert a .rez archive or a .parquet recording to a .dendro archive")
                .long_about(
                    "Write a new .dendro archive holding a recording an earlier release wrote:\n\
                     a v1, v2 or v3 `.rez`, or a standalone `.parquet`. 6.x writes .dendro only\n\
                     and reads every older format; converting is how an old recording becomes\n\
                     one the `recording` tools edit in place.\n\n\
                     A v3 `.rez`'s recordings become sources and its tables streams; segment,\n\
                     WAL and caller-row bytes are copied unchanged. A tar `.rez`'s recordings\n\
                     become sources with their labels, metadata, clock anchor and `complete`\n\
                     flag. A parquet file becomes one source, labelled with the file's\n\
                     `source` as the viewer labels it. Tar and parquet tables are re-encoded.\n\n\
                     The input is never changed. -o defaults to the input with its extension\n\
                     replaced by .dendro, and an existing output is refused.\n\n\
                     EXAMPLES:\n    \
                     # Writes old.dendro beside old.rez\n    \
                     rezolus recording upgrade old.rez\n\n    \
                     # A parquet recording, to a chosen name\n    \
                     rezolus recording upgrade capture.parquet -o capture.dendro",
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("The recording to convert: a .rez archive or a .parquet file")
                        .required(true)
                        .value_parser(value_parser!(PathBuf)),
                )
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output")
                        .value_name("DENDRO")
                        .help("The .dendro to write (default: the input with a .dendro extension); must not exist")
                        .value_parser(value_parser!(PathBuf)),
                )
                .arg(
                    clap::Arg::new("to")
                        .long("to")
                        .value_name("FORMAT")
                        .help("Target container: `dendro`, the only one 6.x writes (accepted for scripts that pass it)")
                        .value_parser(clap::builder::NonEmptyStringValueParser::new())
                        .default_value("dendro"),
                ),
        )
        .subcommand(
            Command::new("snapshot")
                .about("Copy an archive that is still being written, without stopping the writer")
                .long_about(
                    "Take a complete, standalone .dendro copy of an archive while a recorder\n\
                     or `hindsight` is still appending to it.\n\n\
                     WHY NOT `cp`: an archive is a SQLite database in WAL mode, so recent\n\
                     commits live in a `<file>-wal` SIDECAR until they are checkpointed into\n\
                     the archive. Copying the archive alone leaves them behind, and the copy\n\
                     reads as a recording that stopped early.\n\n\
                     A .dendro is copied in one read snapshot, its live tail sealed into the\n\
                     copy, so the copy holds no WAL rows. A .rez (v1, v2 or v3, which a 5.x\n\
                     recorder may still be writing) is converted to the .dendro, its sidecar\n\
                     read through. The writer is never paused and the original is not\n\
                     modified. The output must end in .dendro and must not exist.\n\n\
                     The result is what to hand to someone else, upload to the static-site\n\
                     viewer (a browser is given one file and cannot see a sidecar), or keep\n\
                     as an incident artifact.\n\n\
                     EXAMPLES:\n    \
                     # Grab the last N minutes of a running hindsight buffer\n    \
                     rezolus recording snapshot /var/lib/rezolus/hindsight.dendro -o incident.dendro\n\n    \
                     # ...or of a `rezolus record` still in progress\n    \
                     rezolus recording snapshot rezolus.dendro -o so-far.dendro",
                )
                .arg(
                    clap::Arg::new("FILE")
                        .help("The archive to snapshot (.dendro or .rez; may be open for writing)")
                        .required(true)
                        .value_parser(value_parser!(PathBuf)),
                )
                .arg(
                    clap::Arg::new("output")
                        .short('o')
                        .long("output")
                        .value_name("DENDRO")
                        .required(true)
                        .help("The .dendro to write. Must not already exist.")
                        .value_parser(value_parser!(PathBuf)),
                ),
        )
}

/// Copy an archive that may still be written into one complete standalone
/// `.dendro` at `output`, which must end in `.dendro` and not exist.
///
/// **The point is SQLite's `-wal` sidecar.** An archive is a SQLite database
/// in WAL mode: commits land in `<file>-wal` and are checkpointed into the
/// archive in batches, so the archive on its own is a consistent view as of
/// the last checkpoint and nothing newer. `cp` takes that older view, and it
/// is not detectably wrong, it just ends early. A dendro archive is copied in
/// one read snapshot (sidecar included, writer un-paused) with its live tail
/// sealed; a `.rez` is converted, read through a normal connection, which
/// reads the sidecar too.
pub(crate) fn snapshot_rez(
    path: &std::path::Path,
    output: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use legacy::Container;
    legacy::check_new_dendro(output)?;
    // Staged beside the output so a failed copy leaves nothing there.
    let dir = output.parent().filter(|p| !p.as_os_str().is_empty());
    let staging = match dir {
        Some(dir) => tempfile::tempdir_in(dir),
        None => tempfile::tempdir(),
    }?;
    let staged = staging.path().join("snapshot.dendro");
    match Container::of(path) {
        Container::Dendro => {
            let src = dendro::archive::Archive::open(path)?;
            crate::dendro_copy::copy(&src, &staged, &dendro::rewrite::CopySpec::everything())?;
        }
        Container::RezV3 | Container::RezTar => {
            println!("{}", legacy::convert_for(path, &staged, output)?);
        }
        c => {
            return Err(format!(
                "{} is a {}, not an archive (.dendro or .rez)",
                path.display(),
                c.name()
            )
            .into())
        }
    }
    std::fs::rename(&staged, output)?;
    println!("Wrote {}", output.display());
    Ok(())
}

/// Convert the legacy recording at `path` (any `.rez`, or a parquet file)
/// into a new dendro archive at `output`, or beside `path` with a `.dendro`
/// extension. Never in place and never over an existing file.
fn upgrade_to_dendro(
    path: &std::path::Path,
    output: Option<&std::path::Path>,
    to: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if to != "dendro" {
        return Err(format!(
            "--to {to}: 6.x writes .dendro only; `recording upgrade` converts to dendro \
             (a v3 .rez is read as it is, so there is nothing to upgrade a tar to)"
        )
        .into());
    }
    let output = output
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| legacy::dendro_beside(path));
    legacy::check_new_dendro(&output)?;
    println!("{}", legacy::convert(path, &output)?);
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
        Some(("check", sub_args)) => {
            let registry = load_template_registry(
                sub_args
                    .get_one::<PathBuf>("templates")
                    .map(|p| p.as_path()),
            );
            std::process::exit(check::run(sub_args, &registry));
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
            let to = sub_args
                .get_one::<String>("to")
                .map(String::as_str)
                .unwrap_or("dendro");
            upgrade_to_dendro(path, output, to)
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

#[cfg(test)]
mod upgrade_to_dendro_tests {
    use super::upgrade_to_dendro;
    use dendro::archive::Archive;

    /// The committed v2 tar archive (`rez::fixtures::committed_rez`): one
    /// recording, tables `a` and `b`, written whole by the old recorder.
    fn tar_rez(dir: &std::path::Path) -> std::path::PathBuf {
        use crate::recorder::rez::{detect_rez_format, RezFormat};
        let path = ::rez::fixtures::committed_rez("v2.rez", dir);
        assert_eq!(detect_rez_format(&path).unwrap(), RezFormat::V2Tar);
        path
    }

    #[test]
    fn a_tar_archive_converts_to_dendro() {
        let dir = tempfile::tempdir().unwrap();
        let src = tar_rez(dir.path());
        let dest = dir.path().join("out.dendro");
        upgrade_to_dendro(&src, Some(&dest), "dendro").unwrap();

        // Before anything opens the output: a reader's connection leaves
        // `-wal`/`-shm` files of its own, and the claim here is that the
        // conversion leaves neither those nor its staging directory.
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, vec!["out.dendro".to_string(), "v2.rez".to_string()]);

        let out = Archive::open(&dest).unwrap();
        let sources = out.read_sources().unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].complete);
        assert_eq!(
            out.all_streams(sources[0].id).unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn an_existing_output_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let src = tar_rez(dir.path());
        let dest = dir.path().join("out.dendro");
        std::fs::write(&dest, b"keep me").unwrap();
        let err = upgrade_to_dendro(&src, Some(&dest), "dendro")
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"keep me");
    }

    /// Without `-o` the output is the input with a `.dendro` extension, and
    /// `--to v3` is refused: 6.x writes .dendro only.
    #[test]
    fn the_default_output_is_beside_the_input_and_v3_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let src = tar_rez(dir.path());
        let err = upgrade_to_dendro(&src, None, "v3").unwrap_err().to_string();
        assert!(err.contains(".dendro only"), "{err}");
        upgrade_to_dendro(&src, None, "dendro").unwrap();
        assert!(dir.path().join("v2.dendro").exists());
    }

    /// Every legacy fixture converts, and the archive answers what the
    /// original answers.
    #[test]
    fn each_legacy_fixture_converts_and_answers_the_same() {
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let pool = || metriken_storage::BufferPool::new(64 * 1024 * 1024);
        for name in ["v2.rez", "v3.rez", "v3-live.rez", "v3-multi.rez"] {
            let src = ::rez::fixtures::committed_rez(name, dir.path());
            let dest = dir.path().join(format!("{name}.dendro"));
            upgrade_to_dendro(&src, Some(&dest), "dendro").unwrap();
            let a = crate::rez_reader::RezReader::open_recordings(&src, pool()).unwrap();
            let b = crate::rez_reader::RezReader::open_recordings(&dest, pool()).unwrap();
            assert_eq!(a.len(), b.len(), "{name}");
            for ((la, ra), (lb, rb)) in a.iter().zip(&b) {
                assert_eq!(la, lb, "{name}");
                let (start, end) = ra.time_range().unwrap();
                for q in ["mem_free", "irate(ops[2s])", "histogram_count(lat)"] {
                    let answer = |r: &dyn MetricsSource| {
                        r.query_range(q, start, end, 1.0)
                            .map(|v| serde_json::to_value(v).unwrap())
                            .ok()
                    };
                    assert_eq!(answer(ra), answer(rb), "{name}: {q}");
                }
            }
        }
    }
}

#[cfg(test)]
mod dendro_command_tests {
    use super::{snapshot_rez, upgrade_to_dendro};
    use crate::dendro_copy::fixtures;
    use dendro::archive::Archive;

    /// `snapshot` of a dendro archive with rows still in its WAL: the copy
    /// holds every row, all sealed, and nothing is left beside it.
    #[test]
    fn a_dendro_snapshot_is_sealed_and_whole() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.dendro");
        fixtures::recorded(&live, 10, false);
        let out = dir.path().join("snap.dendro");
        snapshot_rez(&live, &out).unwrap();
        for suffix in ["-wal", "-shm"] {
            assert!(!dir.path().join(format!("snap.dendro{suffix}")).exists());
        }

        let (src, snap) = (Archive::open(&live).unwrap(), Archive::open(&out).unwrap());
        let (a, b) = (
            src.read_sources().unwrap()[0].id,
            snap.read_sources().unwrap()[0].id,
        );
        let streams = crate::dendro_copy::streams(&src).unwrap();
        assert!(streams.iter().any(|s| s.ends_with("/occupants")));
        for stream in streams {
            let (_, sealed) = src.segment_span(a, &stream).unwrap();
            let wal = src.live_wal_span(a, &stream).unwrap();
            let (_, got) = snap.segment_span(b, &stream).unwrap();
            assert_eq!(got.rows, sealed.rows + wal.rows, "{stream}");
            assert_eq!(snap.live_wal_span(b, &stream).unwrap().rows, 0, "{stream}");
        }
        assert!(
            snapshot_rez(&live, &out).is_err(),
            "never over an existing file"
        );
    }

    /// `upgrade` names a dendro input for what it is.
    #[test]
    fn upgrade_names_a_dendro_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rec.dendro");
        fixtures::recorded(&path, 4, true);
        let err = upgrade_to_dendro(&path, Some(&dir.path().join("out.dendro")), "dendro")
            .unwrap_err()
            .to_string();
        assert!(err.contains("already a dendro archive"), "{err}");
    }

    /// `snapshot` of a `.rez` (one a 5.x recorder may still be writing)
    /// writes a `.dendro` holding every row the `.rez` reads, its live tail
    /// included; an output not ending in `.dendro`, or one that exists, is
    /// refused.
    #[test]
    fn a_rez_snapshot_is_a_dendro() {
        use metriken_query::MetricsSource;
        let dir = tempfile::tempdir().unwrap();
        let live = ::rez::fixtures::committed_rez("v3-live.rez", dir.path());
        let err = snapshot_rez(&live, &dir.path().join("snap.rez"))
            .unwrap_err()
            .to_string();
        assert!(err.contains(".dendro archives only"), "{err}");
        let out = dir.path().join("snap.dendro");
        snapshot_rez(&live, &out).unwrap();
        let pool = || metriken_storage::BufferPool::new(16 * 1024 * 1024);
        let a = crate::rez_reader::RezReader::open_recordings(&live, pool()).unwrap();
        let b = crate::rez_reader::RezReader::open_recordings(&out, pool()).unwrap();
        assert_eq!(a[0].1.time_range_ns(), b[0].1.time_range_ns());
        std::fs::write(dir.path().join("taken.dendro"), b"do not clobber me").unwrap();
        let err = snapshot_rez(&live, &dir.path().join("taken.dendro"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(
            std::fs::read(dir.path().join("taken.dendro")).unwrap(),
            b"do not clobber me"
        );
    }
}
