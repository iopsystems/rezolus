use super::*;

pub(crate) mod agent_probe;
mod child;
mod config;
mod endpoint;
/// The replication stream, read through `metriken-recorder`, and the wire
/// names older agents serve it under.
pub(crate) mod stream;
// The `.rez` format lives in its own crate (`rezolus` is binary-only, so
// nothing could depend on it), re-exported under the paths call sites
// already use. 6.x converts a `.rez` with `recording upgrade` and writes
// none.
pub(crate) use ::rez::{rez, wire};
#[cfg(test)]
pub(crate) use ::rez::{schema, wal};

/// The 5.x release the refusals of what 6.x no longer records (`.rez`,
/// parquet and raw output, `--separate`, an agent that cannot stream) point
/// at, as "5.27.0 or later": 5.27.0 does all of them, and naming a minimum
/// rather than the latest release keeps the messages true if another 5.x
/// release ships.
pub(crate) const RELEASE_5X: &str = "5.27.0";

use crate::parquet_metadata::KEY_EVENTS;
use crate::viewer::{Event, Events};
pub use config::RecordingConfig;
use endpoint::Protocol;
use metriken_recorder as mr;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub fn command() -> Command {
    Command::new("record")
        .about("On-demand recording of metrics to a .dendro archive")
        .long_about(format!(
            "Record one or more metrics endpoints at a fixed interval into a .dendro archive.\n\n\
             How each endpoint is recorded is detected (see DETECTION): from its\n\
             replication stream when it serves one (a Rezolus agent from 5.21.0, or any\n\
             producer serving /metrics/stream), or as a Prometheus-compatible endpoint,\n\
             scraped each interval.\n\n\
             WHAT TO RECORD (choose one): --url for a single endpoint (default\n\
             http://localhost:4241), --endpoint (repeatable) for several at once, or --config\n\
             for a TOML file. Exactly one: passing two of them is a parse error, not a\n\
             merge — and the deprecated positional URL counts as --url for that rule, as\n\
             positional OUTPUT counts as -o. Prefer the flags.\n\n\
             --url is shorthand for a single endpoint with no modifiers; reach for a single\n\
             --endpoint when you need source=, role= or protocol= on it. Several endpoints\n\
             become one archive holding a recording each (see WHAT IT WRITES). --config\n\
             conflicts only with --url/--endpoint: every other flag still applies alongside\n\
             it, and -o and --interval override the file\'s [recording] table. There is no\n\
             duration key — a bounded config-driven run passes --duration on the command line.\n\n\
             HOW LONG TO RECORD (choose one): --duration for a fixed window, nothing to run\n\
             until Ctrl-C, or `-- <command>` to record for exactly the lifetime of a wrapped\n\
             command (perf-record style) — it stops when the command exits.\n\n\
             A wrapped command keeps this terminal: its stdin/stdout/stderr pass straight\n\
             through, and rezolus exits with the command\'s own exit status, so\n\
             `rezolus record -o bench.dendro -- ./bench.sh && analyze bench.dendro` gates on\n\
             the benchmark exactly as it would without the wrapper. Two substitutions: the\n\
             --duration cap exits 124 if it kills the command, and a recording failure (an\n\
             endpoint refused mid-run, a write error) exits 1 whatever the command returned.\n\n\
             A wrapped run is also marked in the recording: a `run_start` event when the\n\
             command spawns and a `run_end` event when it exits, so the viewer can draw the\n\
             run\'s edges and align two recordings on them. Only the program name is stored\n\
             unless --record-command-line is given.\n\n\
             WHAT IT WRITES: a .dendro archive at -o/--output, rezolus.dendro by default. A\n\
             per-sampler archive: one table per acquisition group, each at its own cadence\n\
             (a \"sampler\" is one metric collector — cpu_usage, scheduler, blockio — and\n\
             each reads on its own schedule rather than on one global clock), carrying the\n\
             window each read covered so PromQL rate() queries in `rezolus view` and\n\
             `rezolus mcp` can report uncertainty bounds instead of a bare number. Groups\n\
             whose members come and go (threads, cgroups, CPUs) are stored one row per\n\
             member. Several endpoints become one archive holding a recording each, which\n\
             is what `rezolus view` reads as an A/B or multi-host comparison.\n\n\
             The output path must end in .dendro. rezolus 6.x writes no other format: an\n\
             output ending in .rez, .parquet or .raw (or in anything else) is refused, and\n\
             so is a format other than dendro; --separate (one file per endpoint) was removed.\n\
             A rezolus 5.x recorder ({RELEASE_5X} or later) still writes .rez, parquet and\n\
             raw, and `rezolus recording upgrade --to dendro in.rez -o out.dendro` converts\n\
             a .rez afterwards.\n\n\
             A Prometheus endpoint records into the archive like any other. One scrape is\n\
             one request and one response, so it becomes one acquisition group per target,\n\
             windowed by the real HTTP round trip.\n\n\
             OVERWRITING: the output path must NOT already exist — the recorder refuses\n\
             rather than truncate, because the archive is committed as it goes and has no\n\
             staging file. There is no --force; remove the old file or pick a new path.\n\n\
             EXAMPLES:\n    \
             # Record the local agent until ctrl-c (defaults: localhost:4241 -> rezolus.dendro)\n    \
             rezolus record\n\n    \
             # Record a local agent for 5 minutes\n    \
             rezolus record --url http://localhost:4241 -o out.dendro --duration 5m\n\n    \
             # Record only while a benchmark runs, then stop\n    \
             rezolus record -o bench.dendro -- ./bench.sh --iters 100\n\n    \
             # Tag a recording as one arm of an A/B comparison\n    \
             rezolus record -o redis.dendro --label arm=redis -- ./bench.sh\n\n    \
             # High-resolution capture: sample every 100ms for 30 seconds\n    \
             rezolus record -o out.dendro --interval 100ms --duration 30s\n\n    \
             # Record a Prometheus endpoint, tagging the source in the metadata\n    \
             rezolus record --url http://host:9090/metrics -o out.dendro --metadata source=llm-perf\n\n    \
             # Record two agents into ONE archive holding a recording each (multi-host / A/B)\n    \
             rezolus record --endpoint http://web-01:4241 --endpoint http://web-02:4241 -o fleet.dendro\n\n    \
             # Same host, two agents: give each a source= so the recordings are tellable apart\n    \
             rezolus record --endpoint http://localhost:4241,source=redis --endpoint http://localhost:4242,source=valkey -o ab.dendro\n\n    \
             # An agent and a Prometheus endpoint in one archive\n    \
             rezolus record --endpoint http://localhost:4241 --endpoint http://svc:9090/metrics,source=svc -o run.dendro\n\n    \
             # A scraped vLLM with its KPI template, so the viewer shows its KPI section\n    \
             rezolus record --endpoint http://gpu-01:8000/metrics,source=vllm,template=vllm.json -o run.dendro\n\n    \
             # Take the endpoints and the output from a file\n    \
             rezolus record --config rec.toml\n    \
             #   [recording]\n    \
             #   output = \"run.dendro\"    # required; must end in .dendro\n    \
             #   interval = \"1s\"          # optional\n    \
             #   [[endpoints]]\n    \
             #   url = \"http://localhost:4241\"\n    \
             #   [[endpoints]]\n    \
             #   url = \"http://svc:9090/metrics\"\n    \
             #   source = \"svc\"           # optional, as are role =, protocol = and template =\n\n\
             TAGGING: -m/--metadata k=v writes metadata on every recording in the archive.\n\
             -l/--label k=v tags the recordings inside the archive; source and host are\n\
             auto-populated. Recordings that carry an `arm` label (--endpoint url,arm=NAME)\n\
             open in the viewer as an A/B comparison, aliased by those labels, with the\n\
             recordings without one shown beside it in their own sections; an archive without\n\
             arms opens as one combined view.\n\n\
             A recording\'s source is, first found: source= on its --endpoint (or\n\
             --metadata source= with --url), the source label in its stream handshake,\n\
             rezolus for a Rezolus agent, or a name made from the URL\'s host, port and\n\
             path (with a warning). Its other labels are the user\'s --label, over host from\n\
             a Rezolus agent\'s /systeminfo, over the handshake\'s labels. Its metadata is\n\
             its --endpoint\'s role= and template= (as role and service_queries), over the\n\
             user\'s --metadata, over the keys the recorder writes (sampling_interval_ms,\n\
             producer_epoch), over a Rezolus agent\'s version, systeminfo and\n\
             descriptions, over the handshake\'s metadata: a per-endpoint key beats the\n\
             run-wide --metadata. So a producer that names itself\n\
             and its version in its handshake is recorded under that name with no source=.\n\n\
             KPI TEMPLATES: the viewer shows a service\'s KPI section from the template\n\
             in its recording\'s metadata (service_queries). A producer that sends one in\n\
             its stream handshake needs nothing here. For a scraped endpoint,\n\
             which has no handshake, template=FILE on its --endpoint (or\n\
             template = in its [[endpoints]] entry) writes FILE into its recording. The\n\
             file is a ServiceExtension JSON, the format `rezolus recording annotate\n\
             --queries` takes; the templates rezolus ships are crates/dashboard/templates/\n\
             in its source tree. A relative template= on --endpoint is read from the\n\
             working directory, and cannot contain a comma; a relative template = in a\n\
             --config file is read from that file\'s directory. The file is read and\n\
             checked before the recording starts: an unreadable file, one over 1 MiB, JSON\n\
             that is not a ServiceExtension, or an unknown unit_system refuses the run\n\
             (exit 2) and writes nothing. On a streamed\n\
             endpoint template= replaces the producer\'s own template, as any user\n\
             metadata outranks the handshake\'s, and it also replaces a --metadata\n\
             service_queries=. The template\'s queries are evaluated against this\n\
             endpoint\'s recording only, so they need not select by source.\n\n\
             --label applies to EVERY recording the run produces, so it names the run, not\n\
             one endpoint in it: --label arm=redis is how you tag a whole single-endpoint\n\
             capture as one arm, to be compared against another run. Within ONE invocation,\n\
             what distinguishes the recordings is source= on each --endpoint (plus host,\n\
             taken from each agent\'s system info) — so two agents on different hosts are\n\
             already distinct, while two on the same host need a source= each. Recordings\n\
             that end up with identical labels are warned about at startup: nothing\n\
             downstream can tell them apart.\n\n\
             To label a recording for a multi-node or multi-instance `rezolus recording\n\
             combine`, set the metadata keys it reads: --metadata node=web-01 and\n\
             --metadata instance=0. (`record --node` / `--instance` were removed: they were\n\
             never wired to anything, and these are what they were meant to set.)\n\n\
             ABOUT THE ARCHIVE:\n\n\
             The archive is written to disk as the recording runs, so stopping costs the same\n\
             whether the recording ran for a minute or a day. Ctrl-c and SIGTERM (e.g. a\n\
             docker stop) are clean stops: the signal interrupts the wait between samples\n\
             straight away, so finalizing costs only the write of the still-open segments —\n\
             at any --interval, comfortably inside a container\'s stop grace, and never\n\
             proportional to the recording\'s length.\n\n\
             An archive is a single SQLite file, valid at every instant. There is no .partial,\n\
             so the output path must not already exist, and every sample is committed as it\n\
             is taken: a SIGKILL or a power loss costs at most one sampling interval, for\n\
             every sampler. `rezolus recording metadata -i out.dendro` reports an interrupted\n\
             recording as \"not cleanly finalized\" and how many samples are still in its\n\
             write-ahead log.\n\n\
             DETECTION:\n\n\
             An endpoint whose path is / or /metrics/stream is asked for its stream first.\n\
             A stream handshake means it is streamed. Any other answer (a 404 or 409, a\n\
             content type that is not a stream, a 5xx, a timeout, no connection) asks the\n\
             endpoint\'s /status, then its / page: a Rezolus agent is refused (below), or\n\
             retried each tick on a 5xx, 408, 429, timeout or no connection; anything else\n\
             that answered is scraped as Prometheus at /metrics once /metrics answers with\n\
             a success, and retried each tick until then; when neither answered, the\n\
             endpoint is retried each tick. An endpoint with any other path (e.g.\n\
             http://host:9090/metrics) is scraped at that URL. protocol=stream on an\n\
             --endpoint records it from its stream only (rezolus and msgpack mean the\n\
             same), and protocol=prometheus scrapes it only.\n\n\
             STREAMING AND SCRAPING:\n\n\
             A streamed endpoint pushes one frame per --interval, carrying only the\n\
             acquisition groups it re-read since the last one, stamped when the producer\n\
             sampled rather than when the recorder asked. Prometheus endpoints cannot\n\
             stream, so they are scraped each tick, and one run can hold both kinds.\n\n\
             There is no scrape path for a Rezolus agent. An agent that cannot serve the\n\
             stream (older than 5.21.0, a V2 agent, a stream route that answers 404 or\n\
             another refusal, or a handshake that does not decode) is refused with its\n\
             version. At startup that refuses the run before anything is written; a rezolus\n\
             5.x recorder ({RELEASE_5X} or later) records such an agent by scraping it.\n\
             An agent that comes up later and is refused, or is refused on a\n\
             reconnect, is left out of the recording from then on (a reconnect keeps the\n\
             rows it had), the other endpoints keep recording, and the run exits 1 once\n\
             the archive is finalized. When every endpoint has been refused, the run ends.\n\n\
             An agent that is not reachable yet, or whose stream fails with an error that\n\
             can change (a 5xx, 408 or 429, a handshake timeout), is retried each tick. A\n\
             stream that drops mid-run is reconnected after one interval (at least a\n\
             second), as a failed scrape is, and one that goes silent for the scrape\n\
             timeout (twice the interval, between 2 s and 10 s) counts as dropped. A streamed endpoint is\n\
             never switched to scraping: one whose reconnect is answered with a 404, a 409\n\
             or a content type that is not a stream is refused.\n\n\
             A wrapped command that exits on its own waits for each agent's frame covering\n\
             the exit, at most one interval plus the scrape timeout (twice the interval,\n\
             between 2 s and 10 s)."
        ))
        .arg(
            clap::Arg::new("URL")
                .help("Deprecated positional form of --url; prefer --url")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(Url))
                .index(1),
        )
        .arg(
            clap::Arg::new("OUTPUT")
                .help("Deprecated positional form of -o/--output; prefer -o")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(PathBuf))
                .index(2),
        )
        .arg(
            clap::Arg::new("CONFIG_FILE")
                .long("config")
                .help("Record endpoints defined in a TOML file: a [recording] table (output, interval) plus [[endpoints]] entries mirroring the --endpoint fields (url, source, role, arm, protocol, template)")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(PathBuf))
                .conflicts_with_all(["URL"]),
        )
        .arg(
            clap::Arg::new("ENDPOINT")
                .long("endpoint")
                .help("Add an endpoint as url[,source=name][,role=label][,arm=name][,host=name][,protocol=stream|prometheus][,template=FILE]; host labels the recording's host (a Rezolus agent's own hostname otherwise); role is a free-form tag (conventionally service or loadgen); arm labels the endpoint's recording as one arm of an A/B comparison (conventionally baseline or experiment): an archive whose recordings carry arm opens in `rezolus view` as a comparison (one recording per arm), with unarmed recordings' sections beside it; one without arms opens as one combined view; template is a KPI template file (a ServiceExtension JSON, as `recording annotate --queries` takes) written into this endpoint's recording as service_queries, so the viewer shows that service's KPI section: it is checked before recording starts, an unreadable file, one over 1 MiB, JSON that is not a ServiceExtension or an unknown unit_system refuses the run (exit 2), and it replaces any template a streamed producer sends in its handshake; protocol skips detection: stream records it from its replication stream only, never scraping it (rezolus and msgpack are accepted for it too), prometheus scrapes it as text exposition (at /metrics for a URL whose path is /); without protocol, a URL whose path is / or /metrics/stream is asked for its stream first and any other URL is scraped (see DETECTION); repeat for several (e.g. http://host:9090/metrics,source=svc,role=service,protocol=prometheus)")
                .action(clap::ArgAction::Append)
                .conflicts_with_all(["URL", "CONFIG_FILE"]),
        )
        .arg(
            // Removed in 6.0. Hidden, and refused with a message naming a
            // 5.x recorder rather than with clap's unknown-argument error.
            clap::Arg::new("SEPARATE")
                .long("separate")
                .hide(true)
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            clap::Arg::new("VERBOSE")
                .long("verbose")
                .short('v')
                .help("Increase the verbosity")
                .action(clap::ArgAction::Count),
        )
        .arg(
            clap::Arg::new("INTERVAL")
                .long("interval")
                .short('i')
                .help("Time between samples, as a duration like 1s, 100ms, or 500us")
                .action(clap::ArgAction::Set)
                .default_value("1s")
                .value_parser(value_parser!(humantime::Duration)),
        )
        .arg(
            clap::Arg::new("DURATION")
                .long("duration")
                .short('d')
                .help("How long to record before stopping, as a duration like 30s or 5m; omit to record until Ctrl-C. When wrapping a command, acts as a time cap that also terminates the command if it exceeds it")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(humantime::Duration)),
        )
        .arg(
            // Hidden: `dendro` is the only value 6.x accepts, kept so a script
            // passing `--format dendro` keeps working. The 5.x values are
            // refused by name (`config::check_format`).
            clap::Arg::new("FORMAT")
                .long("format")
                .short('f')
                .hide(true)
                .action(clap::ArgAction::Set),
        )
        .arg(
            clap::Arg::new("METADATA")
                .long("metadata")
                .short('m')
                .help("Add a metadata tag as key=value (e.g. source=llm-perf) to every recording in the archive; it outranks the value the producer reports and is kept through a restart; producer_epoch and producer_epochs are the recorder's and are refused; repeat for multiple tags")
                .action(clap::ArgAction::Append),
        )
        .arg(
            clap::Arg::new("LABEL")
                .long("label")
                .short('l')
                .help("Tag the recording with a label as key=value (e.g. run=42, role=server); repeat for multiple. A value without `=` is ignored. `source` and `host` are auto-populated. Applies to EVERY recording in the run, so it cannot tell two endpoints apart — use --endpoint url,source=name for that, and --endpoint url,arm=name for the arms of an A/B")
                .action(clap::ArgAction::Append),
        )
        .arg(
            clap::Arg::new("URL_FLAG")
                .long("url")
                .help("Single metrics endpoint to record (default http://localhost:4241); a URL whose path is / is streamed when it serves /metrics/stream and otherwise scraped as Prometheus at /metrics (see DETECTION)")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(Url))
                .conflicts_with_all(["CONFIG_FILE", "ENDPOINT", "URL"]),
        )
        .arg(
            clap::Arg::new("OUTPUT_FLAG")
                .long("output")
                .short('o')
                .help("Path to the .dendro archive to write; it must end in .dendro and must not exist. Defaults to rezolus.dendro")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(PathBuf))
                .conflicts_with("OUTPUT"),
        )
        .arg(
            clap::Arg::new("RECORD_COMMAND_LINE")
                .long("record-command-line")
                .help("With `-- <command>`, also store the full command line (arguments joined by spaces) in the run_start event's details. Off by default because arguments can carry paths and tokens; only the program name is recorded then. See COMMAND for the run_start/run_end events")
                .action(clap::ArgAction::SetTrue)
                .requires("COMMAND"),
        )
        .arg(
            clap::Arg::new("COMMAND")
                .help("Wrap a command: record only while it runs, then stop when it exits. Give it after `--`, e.g. rezolus record -o out.dendro -- ./bench.sh --iters 100. The run is marked in the recording by two events: run_start (kind run_start, description = the program name, e.g. bench.sh) when the command spawns, and run_end (kind run_end, description = the program name plus `exited <code>`, `capped` or `interrupted`) when its exit is observed. Only the program name is recorded unless --record-command-line is given. A run that captured no samples writes no file at all")
                .action(clap::ArgAction::Set)
                .index(3)
                .num_args(1..)
                .last(true)
                .allow_hyphen_values(true)
                .value_parser(value_parser!(String)),
        )
}

/// Append `events` to the `KEY_EVENTS` payload in `metadata`, in place.
///
/// The existing payload is parsed and the result normalized, so an event
/// already present by id is not duplicated.
fn merge_events_into(
    metadata: &mut BTreeMap<String, String>,
    events: &[Event],
) -> Result<(), String> {
    if events.is_empty() {
        return Ok(());
    }
    let mut payload = match metadata.get(KEY_EVENTS) {
        Some(raw) => serde_json::from_str::<Events>(raw)
            .map_err(|e| format!("the recording's events payload is invalid: {e}"))?,
        None => Events::default(),
    };
    payload.events.extend_from_slice(events);
    payload.normalize();
    let encoded = serde_json::to_string(&payload)
        .map_err(|e| format!("failed to encode the recording's events: {e}"))?;
    metadata.insert(KEY_EVENTS.to_string(), encoded);
    Ok(())
}

/// The `events` key for every recording: the user's `--metadata events=`
/// payload with the wrapped run's events merged in. `None` when there is
/// nothing to write, or when the user's value does not parse, which is
/// kept as given and the run events dropped, with a warning.
fn events_patch(
    config: &RecordingConfig,
    run_events: &[Event],
) -> Option<BTreeMap<String, String>> {
    let mut m: BTreeMap<String, String> = config
        .metadata
        .iter()
        .filter(|(k, _)| k == KEY_EVENTS)
        .cloned()
        .collect();
    if let Err(e) = merge_events_into(&mut m, run_events) {
        warn!("not adding the run events to the recording's metadata: {e}");
        return None;
    }
    m.remove_entry(KEY_EVENTS).map(|kv| BTreeMap::from([kv]))
}

/// How a wrapped command's run ended, for its `run_end` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunEnding {
    /// The command exited on its own with this mapped exit code.
    Exited(i32),
    /// The command outlived `--duration` and was killed.
    Capped,
    /// The recording was stopped (ctrl-c, SIGTERM) while the command was
    /// still running, and the command was terminated with it. Carries the
    /// mapped exit code the termination produced.
    Interrupted(i32),
}

/// One wrapped run: the identity its `run_start` and `run_end` events share.
///
/// The id is minted once per `record` invocation, the same v4 uuid shape as
/// the agent's producer epoch, so the two events of one run pair up by id
/// (`run:<uuid>:start` / `run:<uuid>:end`) and a merge of recordings from
/// different runs keeps each run's pair distinct.
struct RunMarker {
    id: String,
    /// Basename of argv[0]: what the events name the run by. The full
    /// argument list is only stored on request (`--record-command-line`).
    program: String,
}

impl RunMarker {
    fn new(command: &[String]) -> Self {
        let program = command
            .first()
            .map(|argv0| {
                Path::new(argv0)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| argv0.clone())
            })
            .unwrap_or_default();
        Self {
            id: crate::agent::epoch::mint(),
            program,
        }
    }

    /// The `run_start` event. `timestamp` is the recorder-clock stamp taken
    /// when the spawn returned; `command_line` is the full argument list,
    /// present only when the user asked for it.
    fn start_event(&self, timestamp: u64, command_line: Option<String>) -> Event {
        Event {
            timestamp,
            description: self.program.clone(),
            kind: Some("run_start".to_string()),
            details: command_line,
            id: Some(format!("run:{}:start", self.id)),
            source: None,
            node: None,
            instance: None,
            labels: BTreeMap::new(),
            duration_ns: None,
            chart_id: None,
        }
    }

    /// The `run_end` event, stamped with the instant the exit was observed.
    fn end_event(&self, timestamp: u64, ending: RunEnding) -> Event {
        let (description, details) = match ending {
            RunEnding::Exited(code) => (
                format!("{} exited {code}", self.program),
                format!("The command exited on its own with exit code {code}."),
            ),
            RunEnding::Capped => (
                format!("{} capped", self.program),
                "The command outlived the --duration cap and was terminated.".to_string(),
            ),
            RunEnding::Interrupted(code) => (
                format!("{} interrupted", self.program),
                if code > 128 {
                    format!(
                        "The recording was interrupted while the command was still running; \
                         the command was terminated and ended with status {code} \
                         (128 + signal {}).",
                        code - 128
                    )
                } else {
                    format!(
                        "The recording was interrupted while the command was still running; \
                         the command was asked to stop and exited with code {code}."
                    )
                },
            ),
        };
        Event {
            timestamp,
            description,
            kind: Some("run_end".to_string()),
            details: Some(details),
            id: Some(format!("run:{}:end", self.id)),
            source: None,
            node: None,
            instance: None,
            labels: BTreeMap::new(),
            duration_ns: None,
            chart_id: None,
        }
    }
}

/// An endpoint as the user named it: its URL, and `(source)` when it has
/// one.
fn who(endpoint: Option<&str>, name: Option<&str>) -> String {
    let url = endpoint.unwrap_or("?");
    match name {
        Some(name) => format!("{url} ({name})"),
        None => url.to_string(),
    }
}

/// The end-of-run lines for endpoints refused mid-run, one per endpoint and
/// a count, or `None` when none was.
///
/// A reconnect refusal leaves the rows the endpoint had, so its line says
/// from when the archive has none (its last delivered interval, on this
/// host's clock); a first-activation refusal never had any.
fn refusal_summary(
    sources: &[mr::SourceSummary],
    output: &Path,
    finalized: bool,
) -> Option<String> {
    let refused: Vec<&mr::SourceSummary> = sources
        .iter()
        .filter(|s| s.state == mr::SourceState::Refused)
        .collect();
    if refused.is_empty() {
        return None;
    }
    let path = output.display();
    let mut lines: Vec<String> = refused
        .iter()
        .map(|s| {
            let who = format!(
                "{} ({})",
                s.endpoint.as_deref().unwrap_or("?"),
                s.name.as_deref().unwrap_or("?")
            );
            match s.last_wall_ns {
                Some(ns) => {
                    let at = std::time::UNIX_EPOCH + Duration::from_nanos(ns);
                    format!(
                        "error: {who} was refused mid-run and has no rows in {path} after {}",
                        humantime::format_rfc3339_seconds(at)
                    )
                }
                None => format!("error: {who} was refused mid-run and has no rows in {path}"),
            }
        })
        .collect();
    lines.push(format!(
        "error: {} of {} endpoints refused; {}",
        refused.len(),
        sources.len(),
        if finalized {
            "the archive holds the rest and is finalized"
        } else {
            "the archive holds the rest"
        }
    ));
    Some(lines.join("\n"))
}

/// One line for each endpoint the run ended without a recording for, other
/// than a refused one ([`refusal_summary`] names those): one still pending,
/// and one streamed whose metadata read had not returned, so it was never
/// added to the archive.
fn unrecorded_summary(sources: &[mr::SourceSummary], output: &Path) -> Vec<String> {
    sources
        .iter()
        .filter_map(|s| unrecorded_line(s.endpoint.as_deref(), s.name.as_deref(), s.state, output))
        .collect()
}

/// [`unrecorded_summary`]'s line for one endpoint, or `None` when it was
/// recorded or refused.
fn unrecorded_line(
    endpoint: Option<&str>,
    name: Option<&str>,
    state: mr::SourceState,
    output: &Path,
) -> Option<String> {
    let path = output.display();
    match state {
        mr::SourceState::Pending => Some(format!(
            "{} did not become recordable during the run and has no recording in {path}",
            who(endpoint, name)
        )),
        mr::SourceState::Activating => Some(format!(
            "{} opened its stream, but the run ended before its metadata arrived; it has no \
             recording in {path}",
            who(endpoint, name)
        )),
        _ => None,
    }
}

fn wall_now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// A moment on the recorder's timeline: `anchor + monotonic elapsed`, what
/// the run events are stamped with.
pub(crate) fn anchored_at(anchor_wall_ns: u64, elapsed: Duration) -> u64 {
    anchor_wall_ns.saturating_add(elapsed.as_nanos() as u64)
}

/// `sleep_until(deadline)` when there is one, otherwise a future that never
/// resolves — the `tokio::select!` arm shape for an optional deadline.
async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d.into()).await,
        None => std::future::pending().await,
    }
}

/// How long a single scrape, probe or stream connect may take: twice the
/// interval, between 2 s and 10 s (`metriken_recorder::default_timeout`).
pub(crate) fn tick_timeout(interval: Duration) -> Duration {
    mr::default_timeout(interval)
}

/// What the run has learned from the recorder's events.
struct Watch {
    /// Endpoint URL per source, for messages.
    urls: Vec<String>,
    /// Each endpoint's `--endpoint` argument as typed (its URL when it had
    /// none), for a suggested `--endpoint`.
    args: Vec<String>,
    names: Vec<Option<String>>,
    streaming: BTreeSet<mr::SourceId>,
    refused: BTreeSet<mr::SourceId>,
    failed: bool,
    output: std::path::PathBuf,
}

impl Watch {
    fn label(&self, id: mr::SourceId) -> String {
        let i = id.0 as usize;
        match self.names.get(i).cloned().flatten() {
            Some(name) => name,
            None => self.urls.get(i).cloned().unwrap_or_default(),
        }
    }

    /// Act on one event: say what the user needs to know, and track what
    /// the end of the run depends on.
    fn on(&mut self, event: mr::Event) {
        match event {
            mr::Event::Active {
                source,
                transport,
                name,
                inferred,
            } => {
                let i = source.0 as usize;
                if inferred {
                    eprintln!(
                        "warn: no source name specified for {}, using \"{name}\" \
                         (pass --endpoint URL,source=NAME or --metadata source=NAME to override)",
                        self.urls.get(i).map(String::as_str).unwrap_or("?")
                    );
                }
                if let Some(slot) = self.names.get_mut(i) {
                    *slot = Some(name);
                }
                if transport == mr::ActiveTransport::Stream {
                    self.streaming.insert(source);
                }
            }
            mr::Event::Refused { source, reason } => {
                eprintln!("error: {reason}");
                self.refused.insert(source);
                self.streaming.remove(&source);
            }
            mr::Event::Restarted {
                source, version, ..
            } => info!(
                "{}: the recording's metadata now records the agent restart \
                 (producer_epochs; version {})",
                self.label(source),
                version.as_deref().unwrap_or("not reported")
            ),
            mr::Event::LabelsNotApplied { source, labels } => {
                let i = source.0 as usize;
                let arg = self.args.get(i).cloned().unwrap_or_default();
                eprintln!(
                    "warn: {}",
                    labels_not_applied(&self.label(source), &arg, self.args.len() > 1, &labels)
                );
            }
            mr::Event::Failed { error } => {
                eprintln!("error: recording failed: {error}");
                eprintln!(
                    "note: the recording so far is readable at {}",
                    self.output.display()
                );
                self.failed = true;
            }
            _ => {}
        }
    }
}

/// The line for labels a late probe reply carried that could not be put on
/// the recording (`Event::LabelsNotApplied`), naming how to set them on the
/// next run: `arg`, the endpoint's `--endpoint` as typed, with each label
/// `--endpoint` takes (`source=`, `arm=`, `host=`) appended in place of any
/// value it gave for it, and any other label as a run-wide `--label`. With
/// `several` endpoints the line says which `--endpoint` it replaces. When
/// the rebuilt argument would not parse as an `--endpoint` (a value the
/// parser refuses), the line names the labels and suggests no command.
pub(crate) fn labels_not_applied(
    name: &str,
    arg: &str,
    several: bool,
    labels: &BTreeMap<String, String>,
) -> String {
    let keys: Vec<&str> = labels.keys().map(String::as_str).collect();
    let (url, options) = arg.split_once(',').unwrap_or((arg, ""));
    let endpoint_keys = ["source", "arm", "host"];
    let set_here: Vec<(&String, &String)> = labels
        .iter()
        .filter(|(k, _)| endpoint_keys.contains(&k.as_str()))
        .collect();
    let mut parts: Vec<String> = vec![url.to_string()];
    parts.extend(
        options
            .split(',')
            .filter(|p| !p.trim().is_empty())
            .filter(|p| {
                let key = p.split_once('=').map_or(*p, |(k, _)| k).trim();
                !set_here.iter().any(|(k, _)| k.as_str() == key)
            })
            .map(str::to_string),
    );
    parts.extend(set_here.iter().map(|(k, v)| format!("{k}={v}")));
    let rebuilt = parts.join(",");
    let noun = if keys.len() == 1 {
        "label arrived late and is"
    } else {
        "labels arrived late and are"
    };
    if crate::recorder::config::parse_endpoint_str(&rebuilt).is_err() {
        let shown: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        return format!(
            "{name}: its {} {noun} not on the recording ({})",
            keys.join("/"),
            shown.join(", ")
        );
    }
    let mut again = format!("--endpoint {}", crate::mcp::shell_word(&rebuilt));
    for (k, v) in labels
        .iter()
        .filter(|(k, _)| !endpoint_keys.contains(&k.as_str()))
    {
        again.push_str(&format!(
            " --label {}",
            crate::mcp::shell_word(&format!("{k}={v}"))
        ));
    }
    if several {
        again.push_str(&format!(
            " in place of --endpoint {}",
            crate::mcp::shell_word(arg)
        ));
    }
    format!(
        "{name}: its {} {noun} not on the recording; record again with {again}",
        keys.join("/"),
    )
}

/// Runs the Rezolus `recorder`, which records one or more endpoints into one
/// `.dendro` archive through `metriken-recorder`: Rezolus agents and other
/// producers from their replication stream, Prometheus endpoints by
/// scraping them, detected per endpoint.
pub fn run(config: RecordingConfig) {
    let _log_drain = configure_logging(verbosity_to_level(config.verbose));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    // Raised by the ctrl-c / SIGTERM handler alongside `STATE`: the first
    // stops the recording (and cuts short the wait for frames in flight
    // after a wrapped command exits); a second exits at once.
    let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
    let handler_shutdown = shutdown.clone();

    ctrlc::set_handler(move || {
        let state = STATE.load(Ordering::SeqCst);
        // A closed terminal makes this write fail; that must not panic the
        // handler before the recording is finalized.
        let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\n");
        if state == RUNNING {
            info!("finalizing recording... ctrl+c to terminate early");
            STATE.store(TERMINATING, Ordering::SeqCst);
            // `notify_one` leaves a permit if nothing waits yet, so a signal
            // that lands before the wait is never lost.
            handler_shutdown.notify_one();
        } else {
            info!("terminating immediately");
            std::process::exit(2);
        }
    })
    .expect("failed to set ctrl-c handler");

    let client = match Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error creating http client: {e}");
            std::process::exit(1);
        }
    };

    let interval_dur: Duration = config.interval.into();
    let timeout = tick_timeout(interval_dur);

    // The hook that recognises a Rezolus agent and reads what its handshake
    // does not carry (see `agent_probe`).
    let probe: std::sync::Arc<dyn mr::Probe> =
        std::sync::Arc::new(agent_probe::RezolusProbe::new(client.clone()));
    let mut detect_cfg = mr::DetectConfig::new(interval_dur, timeout);
    detect_cfg.wire_names = stream::ACCEPTED_WIRE_NAMES.to_vec();
    let specs: Vec<mr::EndpointSpec> = config
        .endpoints
        .iter()
        .map(|ep| endpoint_spec(&config, ep))
        .collect();

    // Detect every endpoint before the archive exists. A refusal is fatal
    // here, so nothing is left behind; an endpoint not known yet is the
    // ordinary retry-each-tick case.
    let detected = rt.block_on(mr::detect(&client, &detect_cfg, specs, probe.as_ref()));
    let mut any_active = false;
    for (i, d) in detected.iter().enumerate() {
        match d {
            mr::Detected::Streamed { spec, subscription } => {
                any_active = true;
                info!(
                    "endpoint {}: subscribed to its replication stream",
                    spec.url
                );
                if let Some(line) = template_override_line(&config.endpoints[i], subscription) {
                    info!("{line}");
                }
            }
            mr::Detected::Scraped { spec, url } => {
                any_active = true;
                info!("endpoint {}: scraped as Prometheus at {url}", spec.url);
            }
            mr::Detected::Pending { reason, .. } => warn!("{reason}"),
            mr::Detected::Refused { reason, .. } => {
                eprintln!("error: {reason}");
                std::process::exit(1);
            }
        }
    }
    if !any_active {
        eprintln!("error: no endpoints could be reached. Check your configuration.");
        std::process::exit(1);
    }

    let mut watch = Watch {
        urls: detected.iter().map(|d| d.spec().url.to_string()).collect(),
        args: config
            .endpoints
            .iter()
            .map(|ep| ep.arg.clone().unwrap_or_else(|| ep.url.to_string()))
            .collect(),
        names: detected
            .iter()
            .map(|d| d.spec().labels.get("source").cloned())
            .collect(),
        streaming: BTreeSet::new(),
        refused: BTreeSet::new(),
        failed: false,
        output: config.output.clone(),
    };
    let endpoints = detected.len();

    let mut recorder_cfg = mr::RecorderConfig::new(config.output.clone(), interval_dur);
    recorder_cfg.timeout = timeout;
    recorder_cfg.wire_names = stream::ACCEPTED_WIRE_NAMES.to_vec();
    let started = rt.block_on(mr::Recorder::start(
        recorder_cfg,
        client.clone(),
        probe,
        detected,
        Vec::new(),
    ));
    let (recorder, ids, mut events) = match started {
        Ok(started) => started,
        Err(e) => {
            eprintln!("error: failed to start the recording: {e}");
            std::process::exit(1);
        }
    };
    // The run events' timeline: anchored when the recording started.
    let clock_anchor_wall_ns = wall_now_ns();
    let clock_anchor_mono = Instant::now();

    if config.command.is_some() {
        info!("recording while command runs... ctrl-c to stop early");
    } else if config.duration.is_some() {
        info!("recording metrics... ctrl-c to terminate early");
    } else {
        info!("recording metrics... ctrl-c to end the recording");
    }

    let wrapped = config.command.is_some();
    let mut recording_failed = false;

    let outcome: Option<child::Outcome> = rt.block_on(async {
        // Spawn the wrapped command only once the recording has started, so
        // a failed setup never starts an expensive workload.
        let mut child = if let Some(ref cmd) = config.command {
            match child::spawn(cmd) {
                Ok(c) => Some(c),
                Err(e) => {
                    eprintln!("error: failed to start command: {e}");
                    // Nothing was recorded, and `exit` skips destructors.
                    recorder.discard().await;
                    std::process::exit(1);
                }
            }
        } else {
            None
        };
        let mut outcome: Option<child::Outcome> = None;

        // The wrapped run's marker events, on the recorder's timeline. Each
        // is written to every recording; one not yet added gets it when it
        // is.
        let run_marker = config.command.as_deref().map(RunMarker::new);
        let mut run_events: Vec<Event> = Vec::new();
        if let (Some(marker), Some(cmd)) = (&run_marker, config.command.as_deref()) {
            let command_line = config.record_command_line.then(|| cmd.join(" "));
            let spawned_at = anchored_at(clock_anchor_wall_ns, clock_anchor_mono.elapsed());
            run_events.push(marker.start_event(spawned_at, command_line));
            if let Some(patch) = events_patch(&config, &run_events) {
                for id in &ids {
                    if let Err(e) = recorder.patch_metadata(*id, patch.clone()).await {
                        warn!("failed to add the run_start event to the recording: {e}");
                    }
                }
            }
        }
        let mut run_end: Option<Event> = None;
        // The wall clock when the wrapped command was seen to exit on its
        // own: what the wait for streamed frames measures against.
        let mut exit_wall: Option<u64> = None;

        // In wrapped mode the cap is measured from the spawn; otherwise the
        // recording window starts one interval out, as the first tick does.
        let deadline: Option<Instant> = if wrapped {
            config.duration.map(|d| Instant::now() + Duration::from(d))
        } else {
            config
                .duration
                .map(|d| Instant::now() + interval_dur + Duration::from(d))
        };

        loop {
            tokio::select! {
                biased;
                _ = shutdown.notified() => break,
                status = async {
                    match child.as_mut() {
                        Some(c) => c.wait().await,
                        None => std::future::pending().await,
                    }
                } => {
                    let code = match status {
                        Ok(status) => child::map_exit_code(status),
                        Err(e) => {
                            warn!("failed to wait for the command: {e}");
                            1
                        }
                    };
                    info!("command exited (code {code}), finalizing recording");
                    outcome = Some(child::Outcome::Exited(code));
                    exit_wall = Some(wall_now_ns());
                    if let Some(marker) = &run_marker {
                        let at = anchored_at(clock_anchor_wall_ns, clock_anchor_mono.elapsed());
                        run_end = Some(marker.end_event(at, RunEnding::Exited(code)));
                    }
                    child = None;
                    break;
                }
                _ = sleep_until_opt(deadline) => {
                    if wrapped {
                        info!("--duration reached, stopping command");
                        if let Some(mut c) = child.take() {
                            child::terminate(&mut c, child::TERM_GRACE).await;
                        }
                        outcome = Some(child::Outcome::Capped);
                        if let Some(marker) = &run_marker {
                            let at = anchored_at(clock_anchor_wall_ns, clock_anchor_mono.elapsed());
                            run_end = Some(marker.end_event(at, RunEnding::Capped));
                        }
                    }
                    break;
                }
                event = events.recv() => {
                    let Some(event) = event else { break };
                    watch.on(event);
                    if watch.failed {
                        break;
                    }
                    // Every endpoint refused: nothing is left to record and
                    // a refused endpoint is never retried, so the run ends
                    // here instead of waiting for `--duration` or ctrl-c.
                    if watch.refused.len() == endpoints {
                        warn!("every endpoint has been refused; finalizing the recording");
                        break;
                    }
                }
            }
        }

        // From here on the events are read on a task of their own, so
        // nothing below (terminating the command, writing `run_end`,
        // finishing) runs while they go unread. The recorder never waits for
        // them to be read; this keeps them in order and on time for the
        // messages they print.
        let (failed_in_loop, streaming) = (watch.failed, !watch.streaming.is_empty());
        let drain = tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                watch.on(event);
            }
            watch
        });

        // A stop while the wrapped command is still alive: terminate and
        // reap it so it is never orphaned.
        if let Some(mut c) = child.take() {
            // A command that ended in the same instant as the stop signal
            // exited on its own: one poll before the terminate keeps it from
            // being labelled interrupted.
            let (status, ending) = match c.try_wait() {
                Ok(Some(status)) => (status, RunEnding::Exited(child::map_exit_code(status))),
                _ => {
                    let status = child::terminate(&mut c, child::TERM_GRACE).await;
                    (status, RunEnding::Interrupted(child::map_exit_code(status)))
                }
            };
            let code = child::map_exit_code(status);
            if outcome.is_none() {
                outcome = Some(child::Outcome::Exited(code));
            }
            if let Some(marker) = &run_marker {
                let at = anchored_at(clock_anchor_wall_ns, clock_anchor_mono.elapsed());
                run_end = Some(marker.end_event(at, ending));
            }
        }
        // `run_end` before finish: the writer must still be open.
        if let Some(event) = run_end.take() {
            run_events.push(event);
            if !failed_in_loop {
                if let Some(patch) = events_patch(&config, &run_events) {
                    for id in &ids {
                        if let Err(e) = recorder.patch_metadata(*id, patch.clone()).await {
                            warn!("failed to add the run_end event to the recording: {e}");
                        }
                    }
                }
            }
        }

        if failed_in_loop {
            // Reported when it happened; what is on disk is the recovery
            // artifact and is left as it is.
            drop(recorder);
            let _ = drain.await;
            recording_failed = true;
            return outcome;
        }

        // The interval in flight. A wrapped command that exited on its own
        // waits for every streamed endpoint's frame stamped at or after the
        // exit, bounded by one interval plus the timeout; any other stop
        // gets one interval's grace when something streams, at most two
        // seconds so a long interval cannot hold a `docker stop` past its
        // grace period. A ctrl-c during either ends it.
        let stop = match exit_wall {
            Some(wall_ns) => mr::Stop::AfterExit {
                wall_ns,
                bound: interval_dur + timeout,
            },
            None if !streaming => mr::Stop::Grace(Duration::ZERO),
            None => mr::Stop::Grace(interval_dur.min(Duration::from_secs(2))),
        };
        let cancel = mr::CancellationToken::new();
        let on_signal = {
            let (cancel, shutdown) = (cancel.clone(), shutdown.clone());
            tokio::spawn(async move {
                shutdown.notified().await;
                cancel.cancel();
            })
        };
        let finished = recorder.finish(stop, cancel).await;
        on_signal.abort();
        let watch = drain.await.expect("the event task does not panic");

        match finished {
            Ok(summary) if summary.discarded => {
                // Nothing was captured, so the archive was removed rather
                // than left as a stub.
                if wrapped {
                    warn!("command exited before any metrics were recorded");
                } else {
                    eprintln!("error: no data was recorded from any endpoint");
                }
                recording_failed = true;
            }
            Ok(summary) => {
                info!("wrote the archive to {}", config.output.display());
                for line in unrecorded_summary(&summary.sources, &config.output) {
                    warn!("{line}");
                }
                if let Some(summary) = refusal_summary(&summary.sources, &config.output, true) {
                    eprintln!("{summary}");
                    recording_failed = true;
                }
            }
            Err(e) => {
                if !watch.failed {
                    eprintln!("error saving the archive: {e}");
                }
                recording_failed = true;
            }
        }
        outcome
    });

    // Flush buffered logs before exiting the process: std::process::exit
    // skips destructors, so drop the log drain explicitly first.
    if recording_failed {
        // Outranks the wrapped command's own status: the command may well have
        // succeeded, but we failed to record it, and that is what this process
        // is here to do.
        drop(_log_drain);
        std::process::exit(1);
    }

    if let Some(o) = outcome {
        drop(_log_drain);
        std::process::exit(o.exit_code());
    }
}

/// The line saying a `template=` file replaces the template a streamed
/// producer sent in its handshake, or `None` when the endpoint has no
/// `template=` or the handshake carried none. Only startup detection sees a
/// handshake here; an endpoint first reached later is recorded the same way,
/// without the line.
fn template_override_line(
    ep: &endpoint::EndpointConfig,
    subscription: &mr::Subscription,
) -> Option<String> {
    let path = ep.template.as_ref()?;
    subscription
        .handshake()
        .metadata
        .contains_key(crate::parquet_metadata::KEY_SERVICE_QUERIES)
        .then(|| {
            format!(
                "endpoint {}: template={} replaces the KPI template its handshake sent",
                ep.url,
                path.display()
            )
        })
}

/// The library's view of one `--endpoint`: its labels (`source=`, then
/// every `--label`) and metadata (every `--metadata`, `role=`, and
/// `template=` as `service_queries`).
///
/// The metadata is the caller's, which outranks what a producer's handshake
/// carries, so a `template=` file replaces a streamed producer's own
/// template. A per-endpoint key (`role=`, `template=`) replaces the same key
/// from `--metadata`, which applies to every recording rather than this one.
fn endpoint_spec(config: &RecordingConfig, ep: &endpoint::EndpointConfig) -> mr::EndpointSpec {
    let mut labels = BTreeMap::new();
    if let Some(source) = &ep.source {
        labels.insert("source".to_string(), source.clone());
    }
    labels.extend(config.labels.iter().cloned());
    // The endpoint's own arm and host over a run-wide `--label`.
    if let Some(arm) = &ep.arm {
        labels.insert("arm".to_string(), arm.clone());
    }
    if let Some(host) = &ep.host {
        labels.insert("host".to_string(), host.clone());
    }
    let mut metadata: BTreeMap<String, String> = config.metadata.iter().cloned().collect();
    if let Some(role) = &ep.role {
        metadata.insert(
            crate::parquet_metadata::NESTED_ROLE.to_string(),
            role.clone(),
        );
    }
    if let Some(json) = &ep.template_json {
        metadata.insert(
            crate::parquet_metadata::KEY_SERVICE_QUERIES.to_string(),
            json.clone(),
        );
    }
    let mut spec = mr::EndpointSpec::new(ep.url.clone())
        .with_transport(Protocol::transport(ep.protocol.as_ref()));
    spec.labels = labels;
    spec.metadata = metadata;
    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_arg_graph_is_valid() {
        // Catches malformed clap wiring (e.g. positional index collisions)
        // at test time instead of panicking at runtime.
        command().debug_assert();
    }

    #[test]
    fn from_args_populates_command_from_trailing_args() {
        let matches = command()
            .try_get_matches_from(["record", "--", "echo", "hello"])
            .expect("parse");
        let config = RecordingConfig::from_args(&matches).expect("config");
        assert_eq!(
            config.command,
            Some(vec!["echo".to_string(), "hello".to_string()])
        );
        // Defaults apply when no --url/-o are given.
        assert_eq!(config.output, PathBuf::from("rezolus.dendro"));
        assert_eq!(config.endpoints.len(), 1);
        assert_eq!(config.endpoints[0].url.as_str(), "http://localhost:4241/");
    }

    #[test]
    fn from_args_without_command_is_none() {
        let matches = command()
            .try_get_matches_from(["record", "--url", "http://host:4241"])
            .expect("parse");
        let config = RecordingConfig::from_args(&matches).expect("config");
        assert!(config.command.is_none());
    }

    const TEST_ANCHOR: u64 = 1_700_000_000_000_000_000;
    const TEST_SECOND: u64 = 1_000_000_000;

    fn test_config(output: &Path) -> RecordingConfig {
        RecordingConfig {
            interval: humantime::Duration::from(Duration::from_secs(1)),
            duration: None,
            verbose: 0,
            output: output.to_path_buf(),
            metadata: Vec::new(),
            labels: vec![("arm".to_string(), "redis".to_string())],
            endpoints: Vec::new(),
            command: None,
            record_command_line: false,
        }
    }

    /// Merging into a map that already holds events keeps them, appends the
    /// new one, and dedups by id, so merging the same event twice is a no-op.
    #[test]
    fn merging_run_events_appends_and_dedups_by_id() {
        let marker = RunMarker::new(&["sh".to_string(), "-c".to_string(), "sleep 1".to_string()]);
        let start = marker.start_event(TEST_ANCHOR, Some("sh -c sleep 1".to_string()));
        let end = marker.end_event(TEST_ANCHOR + TEST_SECOND, RunEnding::Exited(0));

        let mut m = BTreeMap::new();
        merge_events_into(&mut m, std::slice::from_ref(&start)).unwrap();
        merge_events_into(&mut m, std::slice::from_ref(&start)).unwrap();
        merge_events_into(&mut m, std::slice::from_ref(&end)).unwrap();

        let payload: Events = serde_json::from_str(m.get(KEY_EVENTS).unwrap()).unwrap();
        assert_eq!(payload.events, vec![start.clone(), end.clone()]);
        assert_eq!(start.details.as_deref(), Some("sh -c sleep 1"));
        assert_eq!(end.description, "sh exited 0");
        assert_eq!(end.kind.as_deref(), Some("run_end"));
        assert_eq!(
            end.id.as_deref(),
            Some(format!("run:{}:end", marker.id).as_str())
        );
        assert!(end.details.is_some());
    }

    /// The run events merge into a user `--metadata events=...` value rather
    /// than replacing it, and a user value that does not parse is kept
    /// verbatim, with the run events the ones dropped.
    #[test]
    fn run_events_merge_into_the_users_events() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(&dir.path().join("out.dendro"));
        let marker = RunMarker::new(&["bench".to_string()]);
        let start = marker.start_event(TEST_ANCHOR + TEST_SECOND, None);

        let user =
            r#"{"events":[{"timestamp":1700000000000000000,"description":"deploy","id":"d1"}]}"#;
        config.metadata = vec![(KEY_EVENTS.to_string(), user.to_string())];
        let m = events_patch(&config, std::slice::from_ref(&start)).unwrap();
        let payload: Events = serde_json::from_str(m.get(KEY_EVENTS).unwrap()).unwrap();
        assert_eq!(payload.events.len(), 2);
        assert_eq!(payload.events[0].description, "deploy");
        assert_eq!(payload.events[1], start);

        config.metadata = vec![(KEY_EVENTS.to_string(), "not json".to_string())];
        assert!(
            events_patch(&config, std::slice::from_ref(&start)).is_none(),
            "the user's value is left as given"
        );
    }

    /// The other two endings name the run the same way and say what happened.
    #[test]
    fn run_end_descriptions_name_the_ending() {
        let marker = RunMarker::new(&["./bench".to_string()]);
        assert_eq!(
            marker.end_event(1, RunEnding::Capped).description,
            "bench capped"
        );
        assert_eq!(
            marker.end_event(1, RunEnding::Interrupted(143)).description,
            "bench interrupted"
        );
    }

    /// An endpoint `--endpoint url,source=x,role=y` with `--label` and
    /// `--metadata`: the endpoint's `source` and the labels are labels, the
    /// metadata and `role` metadata, and the endpoint's `role=` wins over a
    /// `--metadata role=`, as every per-endpoint key does.
    #[test]
    fn an_endpoint_spec_carries_the_users_labels_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(&dir.path().join("out.dendro"));
        config.metadata = vec![("note".to_string(), "x".to_string())];
        let ep = endpoint::EndpointConfig {
            url: Url::parse("http://localhost:4241").unwrap(),
            source: Some("web".to_string()),
            role: Some("service".to_string()),
            protocol: Some(Protocol::Prometheus),
            template: None,
            template_json: None,
            arm: None,
            host: None,
            arg: None,
        };
        let spec = endpoint_spec(&config, &ep);
        assert_eq!(spec.labels["source"], "web");
        assert_eq!(spec.labels["arm"], "redis");
        assert_eq!(spec.metadata["note"], "x");
        assert_eq!(spec.metadata["role"], "service");
        assert_eq!(spec.transport, mr::Transport::Prometheus);
        config.metadata = vec![("role".to_string(), "mine".to_string())];
        assert_eq!(endpoint_spec(&config, &ep).metadata["role"], "service");
        assert!(!spec.metadata.contains_key("service_queries"));
    }

    /// `template=` becomes the recording's `service_queries`, over a
    /// `--metadata service_queries=` given for every recording.
    #[test]
    fn an_endpoint_template_is_the_recordings_service_queries() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(&dir.path().join("out.dendro"));
        config.metadata = vec![("service_queries".to_string(), "run-wide".to_string())];
        let ep = endpoint::EndpointConfig {
            url: Url::parse("http://localhost:9090/metrics").unwrap(),
            source: Some("vllm".to_string()),
            role: None,
            protocol: None,
            template: Some(PathBuf::from("vllm.json")),
            template_json: Some(r#"{"service_name":"vllm","kpis":[]}"#.to_string()),
            arm: None,
            host: None,
            arg: None,
        };
        assert_eq!(
            endpoint_spec(&config, &ep).metadata["service_queries"],
            r#"{"service_name":"vllm","kpis":[]}"#
        );
    }

    /// The line for labels that arrived too late names them and the run
    /// that sets them: `source`/`arm`/`host` on the endpoint, which
    /// `--endpoint` parses back, any other as `--label`.
    #[test]
    fn late_labels_name_the_flags_that_set_them() {
        let labels: BTreeMap<String, String> = [
            ("host".to_string(), "web-01".to_string()),
            ("zone".to_string(), "a".to_string()),
        ]
        .into_iter()
        .collect();
        let arg = "http://a:4241,source=svc,arm=baseline,template=k.json";
        let line = labels_not_applied("svc", arg, false, &labels);
        assert_eq!(
            line,
            "svc: its host/zone labels arrived late and are not on the recording; record \
             again with --endpoint http://a:4241,source=svc,arm=baseline,template=k.json,\
             host=web-01 --label zone=a"
        );
        // A host= already given is replaced, and with several endpoints the
        // line names the one it replaces.
        let one: BTreeMap<String, String> = [("host".to_string(), "web-01".to_string())]
            .into_iter()
            .collect();
        let line = labels_not_applied("svc", "http://a:4241,host=old,source=svc", true, &one);
        assert_eq!(
            line,
            "svc: its host label arrived late and is not on the recording; record again \
             with --endpoint http://a:4241,source=svc,host=web-01 in place of --endpoint \
             http://a:4241,host=old,source=svc"
        );
        // The suggestion parses back, and sets the label.
        let suggested = "http://a:4241,source=svc,arm=baseline,template=k.json,host=web-01";
        let ep = crate::recorder::config::parse_endpoint_str(suggested).unwrap();
        assert_eq!(ep.arg.as_deref(), Some(suggested));
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(&dir.path().join("out.dendro"));
        assert_eq!(endpoint_spec(&config, &ep).labels["host"], "web-01");
        assert!(crate::recorder::config::parse_endpoint_str("http://a:4241,host= x").is_err());

        // A shell word is quoted.
        let line = labels_not_applied("svc", "http://a:4241,template=my kpis.json", false, &one);
        assert!(
            line.ends_with("--endpoint 'http://a:4241,template=my kpis.json,host=web-01'"),
            "{line}"
        );
        // A label the parser refuses gets no command.
        let padded: BTreeMap<String, String> = [("host".to_string(), " web-01".to_string())]
            .into_iter()
            .collect();
        let line = labels_not_applied("svc", "http://a:4241", false, &padded);
        assert_eq!(
            line,
            "svc: its host label arrived late and is not on the recording (host= web-01)"
        );
    }

    /// An endpoint's `arm=` labels its recording, over a run-wide
    /// `--label arm=`.
    #[test]
    fn an_endpoints_arm_labels_its_recording() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(&dir.path().join("out.dendro"));
        let ep = config::parse_endpoint_str("http://localhost:4241,arm=experiment").unwrap();
        assert_eq!(ep.arm.as_deref(), Some("experiment"));
        assert_eq!(endpoint_spec(&config, &ep).labels["arm"], "experiment");
        assert!(config::parse_endpoint_str("http://localhost:4241,arm=").is_err());
        assert!(config::parse_endpoint_str("http://localhost:4241,arm= baseline").is_err());
        let plain = config::parse_endpoint_str("http://localhost:4241").unwrap();
        // `test_config` carries `--label arm=redis`, which the endpoint's
        // own arm overrides and a plain endpoint keeps.
        assert_eq!(endpoint_spec(&config, &plain).labels["arm"], "redis");
    }

    /// An endpoint the run ends without recording is named by its URL, and
    /// by its source only when it has one.
    #[test]
    fn an_unrecorded_endpoint_is_named_without_a_placeholder_source() {
        let out = Path::new("out.dendro");
        let lines: Vec<String> = [
            (
                "http://localhost:4241/",
                Some("rezolus"),
                mr::SourceState::Activating,
            ),
            ("http://localhost:9090/", None, mr::SourceState::Pending),
            ("http://localhost:4242/", Some("b"), mr::SourceState::Active),
        ]
        .into_iter()
        .filter_map(|(url, name, state)| unrecorded_line(Some(url), name, state, out))
        .collect();
        assert_eq!(
            lines,
            [
                "http://localhost:4241/ (rezolus) opened its stream, but the run ended before \
                 its metadata arrived; it has no recording in out.dendro",
                "http://localhost:9090/ did not become recordable during the run and has no \
                 recording in out.dendro",
            ]
        );
    }
}
