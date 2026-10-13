use super::*;

mod agent_probe;
mod child;
mod config;
pub(crate) mod detect;
mod endpoint;
mod prometheus;
pub(crate) mod restart;
/// Consuming an agent replication stream — #1224 Phase 3, how a `.dendro`
/// records a Rezolus agent.
pub(crate) mod stream;
// The `.rez` format lives in its own crate so the WASM viewer can read the
// archives this binary writes (`rezolus` is binary-only, so nothing could
// depend on it). Re-exported under the paths call sites already use.
/// The tar (v1/v2) `.rez` writer, kept only so tests can build v1/v2 fixtures.
///
/// Nothing ships that writes this container any more: `record` writes v3, and
/// `combine`/`filter`/`annotate`/`parquet upgrade` convert a tar archive
/// rather than producing one. Proving that tar archives still READ, though,
/// requires being able to construct one — which is what the `rez` crate's
/// `test-support` feature (a dev-dependency here) exists for.
#[cfg(test)]
pub(crate) use ::rez::rez_stream;
pub(crate) use ::rez::{
    parquet_ingest, rez, rez_sqlite, rez_v3_rewrite, rez_v3_writer, seal_policy, wire,
};
#[cfg(test)]
pub(crate) use ::rez::{schema, wal};

/// The 5.x release the refusals of what 6.x no longer records (`.rez`,
/// parquet and raw output, `--separate`, an agent that cannot stream) point
/// at, as "5.27.0 or later": 5.27.0 does all of them, and naming a minimum
/// rather than the latest release keeps the messages true if another 5.x
/// release ships.
pub(crate) const RELEASE_5X: &str = "5.27.0";

use crate::parquet_metadata;
use crate::parquet_metadata::KEY_EVENTS;
use crate::viewer::{Event, Events};
pub(crate) use agent_probe::{fetch_restarted_agent, parse_root_version};
pub use config::RecordingConfig;
use endpoint::{EndpointState, EndpointStatus, Protocol};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

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
             # Take the endpoints and the output from a file\n    \
             rezolus record --config rec.toml\n    \
             #   [recording]\n    \
             #   output = \"run.dendro\"    # required; must end in .dendro\n    \
             #   interval = \"1s\"          # optional\n    \
             #   [[endpoints]]\n    \
             #   url = \"http://localhost:4241\"\n    \
             #   [[endpoints]]\n    \
             #   url = \"http://svc:9090/metrics\"\n    \
             #   source = \"svc\"           # optional, as are role = and protocol =\n\n\
             TAGGING: -m/--metadata k=v writes metadata on every recording in the archive.\n\
             -l/--label k=v tags the recordings inside the archive; source and host are\n\
             auto-populated, and a two-recording archive drives the viewer\'s A/B comparison,\n\
             which aliases the arms off each recording\'s arm/host labels.\n\n\
             A recording\'s source is, first found: source= on its --endpoint (or\n\
             --metadata source= with --url), the source label in its stream handshake,\n\
             rezolus for a Rezolus agent, or a name made from the URL\'s host, port and\n\
             path (with a warning). Its other labels are the user\'s --label, over host from\n\
             a Rezolus agent\'s /systeminfo, over the handshake\'s labels. Its metadata is\n\
             the user\'s --metadata, over the keys the recorder writes (sampling_interval_ms,\n\
             producer_epoch, role), over a Rezolus agent\'s version, systeminfo and\n\
             descriptions, over the handshake\'s metadata. So a producer that names itself\n\
             and its version in its handshake is recorded under that name with no source=.\n\n\
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
             retried each tick on a 5xx, timeout or no connection; anything else that\n\
             answered is scraped as Prometheus at /metrics; when neither answered, the\n\
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
             can change (a 5xx, a handshake timeout), is retried each tick. A stream that\n\
             drops mid-run is reconnected after one interval (at least a second), as a\n\
             failed scrape is, and one that goes silent for the scrape timeout (twice the\n\
             interval, between 2 s and 10 s) counts as dropped. A streamed endpoint is\n\
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
                .help("Record endpoints defined in a TOML file: a [recording] table (output, interval) plus [[endpoints]] entries mirroring the --endpoint fields (url, source, role, protocol)")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(PathBuf))
                .conflicts_with_all(["URL"]),
        )
        .arg(
            clap::Arg::new("ENDPOINT")
                .long("endpoint")
                .help("Add an endpoint as url[,source=name][,role=label][,protocol=stream|prometheus]; role is a free-form tag (conventionally service or loadgen); protocol skips detection: stream records it from its replication stream only, never scraping it (rezolus and msgpack are accepted for it too), prometheus scrapes it as text exposition (at /metrics for a URL whose path is /); without protocol, a URL whose path is / or /metrics/stream is asked for its stream first and any other URL is scraped (see DETECTION); repeat for several (e.g. http://host:9090/metrics,source=svc,role=service,protocol=prometheus)")
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
                .help("Add a metadata tag as key=value (e.g. source=llm-perf) to every recording in the archive; repeat for multiple tags")
                .action(clap::ArgAction::Append),
        )
        .arg(
            clap::Arg::new("LABEL")
                .long("label")
                .short('l')
                .help("Tag the recording with a label as key=value (e.g. arm=redis, role=server); repeat for multiple. A value without `=` is ignored. `source` and `host` are auto-populated. Applies to EVERY recording in the run, so it cannot tell two endpoints apart — use --endpoint url,source=name for that")
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

/// The settings `record` detects endpoints with: the subscription a
/// detected stream is opened with is the one the recording reads.
fn detect_config(interval: Duration, timeout: Duration) -> detect::DetectConfig {
    detect::DetectConfig {
        interval,
        timeout,
        layout: stream::Layout::Long,
        wire_names: stream::ACCEPTED_WIRE_NAMES,
    }
}

/// The epoch an endpoint's source now carries, against the one on record:
/// the uuid of each stream handshake, the first and every reconnect's (see
/// [`adopt_source`]).
fn note_epoch(ep: &mut EndpointState, seen: &str) {
    match ep.agent.producer_epoch.as_deref() {
        // The first handshake: adopt its epoch. Nothing restarted.
        None => ep.agent.producer_epoch = Some(seen.to_string()),
        Some(known) if known == seen => {}
        Some(known) => {
            warn!(
                "{}: the agent restarted mid-recording (producer epoch {known} -> {seen}); \
                 every cumulative counter reset at this point",
                ep.config.source_label()
            );
            ep.agent.producer_epoch = Some(seen.to_string());
            ep.restarts.push(restart::Restart {
                epoch: seen.to_string(),
                from_ts: None,
                rows_before: ep.rows_staged,
            });
            // The fetches so far were for an earlier process; the newest
            // gets its own attempts.
            ep.restart_fetches = 0;
        }
    }
}

/// Write each agent restart into its recording's metadata once a row from the
/// new process has been staged: the epoch history, the version, systeminfo
/// and descriptions the probe hook reads from the new process (its `enrich`,
/// over the new handshake's metadata), and the new handshake's other keys
/// (see [`restart::metadata_patch_with`]). A hook reply that is missing or
/// describes another process is asked for on up to
/// [`restart::FETCH_ATTEMPTS`] ticks in all, and the epochs are then written
/// without it; on the `last` call, at shutdown, there is no next tick, so
/// they are written at once. A key set with `--metadata` is kept. Without an
/// archive there is nothing to amend, and the restarts are dropped;
/// [`note_epoch`] has already logged them.
async fn apply_restarts(
    endpoints: &mut [EndpointState],
    mut archive: Option<&mut Archive>,
    probe: &dyn detect::Probe,
    user_metadata: &[(String, String)],
    timeout: Duration,
    last: bool,
) {
    let version_pinned = user_metadata
        .iter()
        .any(|(k, _)| k == parquet_metadata::KEY_VERSION);
    for (idx, ep) in endpoints.iter_mut().enumerate() {
        let Some(archive) = archive.as_deref_mut() else {
            ep.restarts.clear();
            continue;
        };
        let Some(newest) = ep.restarts.last() else {
            continue;
        };
        if ep.restarts.iter().any(|r| r.from_ts.is_none()) {
            continue;
        }
        let label = ep.config.source_label().to_string();
        let hook = match detect::enrich_within(probe, &ep.config.url, Some(&newest.epoch), timeout)
            .await
        {
            Ok((_, metadata)) => metadata,
            Err(_) if !last && ep.restart_fetches + 1 < restart::FETCH_ATTEMPTS => {
                ep.restart_fetches += 1;
                continue;
            }
            Err(why) => {
                warn!(
                    "{label}: could not read the restarted agent's version, systeminfo \
                     and descriptions ({why}); the recording's metadata records the restart \
                     without them"
                );
                BTreeMap::new()
            }
        };
        ep.restart_fetches = 0;
        let mut reported = ep.identity.handshake_metadata.clone();
        reported.extend(hook);
        let agent = restart::Agent::from_metadata(&reported);
        let restarts = std::mem::take(&mut ep.restarts);
        match archive.record_restarts(
            idx,
            &restarts,
            &agent,
            &ep.identity.handshake_metadata,
            user_metadata,
            version_pinned,
            probe,
        ) {
            Ok(()) => info!(
                "{label}: the recording's metadata now records the agent restart \
                 (producer_epochs; version {})",
                agent.version.as_deref().unwrap_or("not reported")
            ),
            Err(e) => warn!(
                "{label}: the agent restart could not be written into the recording's \
                 metadata: {e}"
            ),
        }
    }
}

/// Mark `ep` active and streamed over `sub`, a subscription detection opened,
/// and take the handshake's word on its timeline, epoch, labels and metadata.
///
/// The handshake is the authority on the rows that will arrive: the
/// recording's anchor and epoch are its, and the probe hook's `enrich`, which
/// runs after this, never sets either.
fn begin_stream(ep: &mut EndpointState, sub: &stream::Subscription, interval: Duration) {
    let source = sub
        .source()
        .cloned()
        .expect("connect returns only once the handshake has been applied");
    if let Some(floor) = sub.update_floor().filter(|floor| interval < *floor) {
        // The agent serves the interval asked for, but nothing in it can be
        // new more often than its TTL: the frames between are empty. Said
        // once, at connect, because the recording's `sampling_interval_ms`
        // will claim the asked-for cadence and the data will not have it.
        warn!(
            "{}: --interval {} is shorter than the agent's snapshot TTL of {}; a frame \
             can only carry new readings every {}, and the rest will be empty",
            ep.config.url,
            humantime::format_duration(interval),
            humantime::format_duration(floor),
            humantime::format_duration(floor)
        );
    }
    if sub.layout() != stream::Layout::Long {
        // Recorded all the same: the decoder reads either layout. Said
        // because a group of slots then resends its member list on every
        // change of occupant.
        info!(
            "{}: the agent serves only the wide stream layout; groups of slots carry \
             their whole member list on each change",
            ep.config.url
        );
    }
    ep.identity.handshake_labels = source.labels.clone();
    adopt_source(ep, &source);
    ep.scrape_url = Some(ep.config.url.clone());
    ep.detected_protocol = Some(Protocol::Stream);
    ep.status = EndpointStatus::Active;
    ep.streaming = true;
}

/// Mark `ep` active and scraped as Prometheus at `url`.
fn begin_scrape(ep: &mut EndpointState, url: Url) {
    ep.scrape_url = Some(url);
    ep.detected_protocol = Some(Protocol::Prometheus);
    ep.status = EndpointStatus::Active;
}

/// Take the probe hook's first `enrich` reply for a streamed endpoint, or say
/// why the recording is added without it.
fn adopt_enrich(ep: &mut EndpointState, reply: Result<detect::Enriched, String>) {
    match reply {
        Ok((labels, metadata)) => {
            ep.identity.hook_labels = labels;
            ep.identity.hook_metadata = metadata;
        }
        Err(why) => warn!(
            "{}: {why}; its recording is added with the labels and metadata of its \
             stream handshake only (for a Rezolus agent: no host label, version, \
             systeminfo or descriptions)",
            ep.config.url
        ),
    }
}

/// Fix the `source` the endpoint's recording is added with (see
/// `detect::Identity::source`), and say so when it is a name inferred from
/// the URL.
fn resolve_source(config: &RecordingConfig, ep: &mut EndpointState) {
    if ep.config.source.is_some() {
        return;
    }
    let source = identity(config, ep, BTreeMap::new()).source();
    let named = ep.identity.handshake_labels.contains_key("source")
        || ep.identity.hook_labels.contains_key("source");
    if !named {
        eprintln!(
            "warn: no source name specified for {}, using \"{source}\" \
             (pass --endpoint URL,source=NAME or --metadata source=NAME to override)",
            ep.config.url,
        );
    }
    ep.config.source = Some(source);
}

/// Where an endpoint's labels and metadata come from, in precedence order.
fn identity<'a>(
    config: &'a RecordingConfig,
    ep: &'a EndpointState,
    recorder_metadata: BTreeMap<String, String>,
) -> detect::Identity<'a> {
    detect::Identity {
        user_source: ep.config.source.as_deref(),
        user_labels: &config.labels,
        user_metadata: &config.metadata,
        recorder_metadata,
        hook_labels: &ep.identity.hook_labels,
        hook_metadata: &ep.identity.hook_metadata,
        handshake_labels: &ep.identity.handshake_labels,
        handshake_metadata: &ep.identity.handshake_metadata,
        url: &ep.config.url,
    }
}

/// Leave one endpoint out of the rest of the run: print the refusal, mark it
/// [`EndpointStatus::Refused`] so it is never retried, and stop treating it
/// as streamed. The other endpoints keep recording; the run counts refused
/// endpoints at the end and exits 1 (see `run`). A recording already open for
/// it keeps its rows and is finalized with the rest.
fn refuse_mid_run(ep: &mut EndpointState, refusal: &str) {
    eprintln!("error: {refusal}");
    ep.status = EndpointStatus::Refused;
    ep.streaming = false;
}

/// The end-of-run lines for endpoints refused mid-run, one per endpoint and
/// a count, or `None` when none was.
///
/// A reconnect refusal leaves the rows the endpoint had, so its line says
/// from when the archive has none (its last delivered interval, on this
/// host's clock); a first-activation refusal never had any.
fn refusal_summary(endpoints: &[EndpointState], output: &Path, finalized: bool) -> Option<String> {
    let refused: Vec<&EndpointState> = endpoints
        .iter()
        .filter(|ep| ep.status == EndpointStatus::Refused)
        .collect();
    if refused.is_empty() {
        return None;
    }
    let path = output.display();
    let mut lines: Vec<String> = refused
        .iter()
        .map(|ep| {
            let who = format!("{} ({})", ep.config.url, ep.config.source_label());
            match ep.last_success_ns {
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
        endpoints.len(),
        if finalized {
            "the archive holds the rest and is finalized"
        } else {
            "the archive holds the rest"
        }
    ));
    Some(lines.join("\n"))
}

/// Take the handshake's word on the endpoint's timeline, epoch and metadata.
///
/// Also the reconnect path: a new connection's handshake names the source
/// again, and a different uuid is a restarted agent, which `note_epoch`
/// records as a restart. Its metadata is the new process's, which
/// [`apply_restarts`] writes; its labels are not taken, since a recording's
/// labels are fixed when it is added.
fn adopt_source(ep: &mut EndpointState, source: &stream::Source) {
    // Zero is what no agent anchors at (1970), so it is not an anchor.
    if source.clock_anchor_wall_ns != 0 {
        ep.agent.clock_anchor_wall_ns = Some(source.clock_anchor_wall_ns);
    }
    ep.identity.handshake_metadata = source.metadata.clone();
    if let Some(uuid) = source.uuid.as_deref().filter(|u| !u.is_empty()) {
        note_epoch(ep, uuid);
    }
}

/// What handling a stream event may need to ask: the probe hook, for the
/// wording of a reconnect refusal, and how long it may take.
#[derive(Clone, Copy)]
struct Hook<'a> {
    probe: &'a dyn detect::Probe,
    timeout: Duration,
}

/// Stage everything the stream pumps have delivered so far.
///
/// Returns the first failure that ends the recording: an interval that could
/// not be staged. Never waits for a pump (they deliver for as long as they
/// run); it waits only on the probe hook when an agent came back unable to
/// serve the stream. Called from the tick and once more on the way out.
async fn drain_stream_events(
    rx: &mut tokio::sync::mpsc::Receiver<(usize, stream::StreamEvent)>,
    endpoints: &mut [EndpointState],
    mut archive: Option<&mut Archive>,
    wall_ns: u64,
    hook: Hook<'_>,
) -> Option<String> {
    let mut failed: Option<String> = None;
    while let Ok((idx, event)) = rx.try_recv() {
        if let Some(e) =
            handle_stream_event(idx, event, endpoints, archive.as_deref_mut(), wall_ns, hook).await
        {
            failed.get_or_insert(e);
        }
    }
    failed
}

/// Act on one event from a pump. `Some` is a failure that ends the recording:
/// an interval that could not be staged.
async fn handle_stream_event(
    idx: usize,
    event: stream::StreamEvent,
    endpoints: &mut [EndpointState],
    archive: Option<&mut Archive>,
    wall_ns: u64,
    hook: Hook<'_>,
) -> Option<String> {
    let mut failed: Option<String> = None;
    let label = endpoints[idx].config.source_label().to_string();
    match event {
        stream::StreamEvent::Interval(applied) => {
            endpoints[idx].record_success(wall_ns);
            endpoints[idx].frames += 1;
            endpoints[idx].last_frame_wall_ns = applied
                .rows
                .iter()
                .map(|r| r.ts.saturating_add(r.wall_offset).max(0) as u64)
                .max();
            if applied.gap {
                // Every interval gets a frame, so a jump is a lost
                // reading rather than a quiet one — the distinction
                // empty frames exist to preserve.
                warn!(
                    "{label}: the stream jumped to interval {}; the intervals before it \
                         produced no frame (the subscription was starved, or frames were lost)",
                    applied.seq
                );
            }
            let first_ts = applied
                .rows
                .iter()
                .filter_map(|r| u64::try_from(r.ts).ok())
                .min();
            if let Some(rec) = archive {
                match rec.stage_stream(idx, &endpoints[idx].config.url, applied) {
                    Ok(()) => {
                        if let Some(ts) = first_ts {
                            endpoints[idx].note_row_staged(ts);
                        }
                    }
                    Err(e) => {
                        failed.get_or_insert(e);
                    }
                }
            }
        }
        stream::StreamEvent::Dropped(e) => {
            warn!(
                "{label} ({}): the stream ended ({e}); reconnecting",
                endpoints[idx].config.url
            );
        }
        stream::StreamEvent::Connected(source) => {
            info!(
                "{label} ({}): stream reconnected",
                endpoints[idx].config.url
            );
            adopt_source(&mut endpoints[idx], &source);
        }
        stream::StreamEvent::Refused(refusal) => {
            // The agent came back unable to serve the stream. Refused as a
            // mid-run first activation is: this endpoint only, never
            // scraped instead, never retried (its pump has stopped). Its
            // recording keeps what it had and is finalized with the rest.
            // The hook is asked for the wording only; a verdict other than
            // a refusal leaves the route's answer as the reason.
            let url = endpoints[idx].config.url.clone();
            let verdict = tokio::time::timeout(
                hook.timeout,
                hook.probe
                    .not_streamable(&url, detect::At::Reconnect, &refusal.answer),
            )
            .await
            .unwrap_or(detect::Verdict::Pending);
            let reason = match verdict {
                detect::Verdict::Refuse { reason } => reason,
                _ => format!("{refusal}. {}", agent_probe::RECONNECT_REFUSED),
            };
            refuse_mid_run(&mut endpoints[idx], &reason);
        }
    }
    failed
}

/// `sleep_until(deadline)` when there is one, otherwise a future that never
/// resolves — the `tokio::select!` arm shape for an optional deadline.
async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d.into()).await,
        None => std::future::pending().await,
    }
}

/// One scrape's body, bracketed by when the request went out and when the
/// response was fully read.
///
/// **The bracket is the point for a Prometheus endpoint.** A scrape is one
/// acquisition — one request, one response — and every value in it was read
/// somewhere inside that interval. Nothing narrower is knowable from here: the
/// exporter does not say when it sampled, only what it read. A Rezolus agent
/// stamps each metric's own window and this bracket is ignored for it.
struct Scraped {
    body: Vec<u8>,
    /// When the request was sent, on the recorder's own anchored timeline.
    ///
    /// Anchored rather than a raw wall reading because these two become the
    /// scrape's acquisition window, and a window is stored in the archive as
    /// an OFFSET from its row's `ts` (`window_offset_columns` subtracts one
    /// from the other). The row's `ts` is anchored, so a raw-wall window made
    /// that offset carry the wall-versus-anchor divergence — exactly the
    /// quantity `wall_offset` exists to record — instead of the read's
    /// position within the tick. The error is zero on a freshly started
    /// recorder and grows with every step or slew, and the window is what
    /// `rate()` prices its uncertainty band from.
    request_ns: u64,
    /// When the response finished arriving, same timeline.
    response_ns: u64,
}

fn wall_now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

async fn scrape_one(
    client: &Client,
    url: &Url,
    anchor_wall_ns: u64,
    anchor_mono: Instant,
) -> Result<Scraped, String> {
    let request_ns = anchored_at(anchor_wall_ns, anchor_mono.elapsed());
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| format!("{e}"))?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let body = response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("{e}"))?;
    Ok(Scraped {
        body,
        request_ns,
        // Read AFTER the body, not after the headers: the values are in the
        // body, so a bracket that closed at the response line would exclude
        // part of the interval they were actually read over.
        response_ns: anchored_at(anchor_wall_ns, anchor_mono.elapsed()),
    })
}

/// Append `events` to the `KEY_EVENTS` payload in `metadata`, in place.
///
/// The existing payload is parsed and the result normalized, so an event
/// already present by id is not duplicated. Shared by the seed (a recording
/// opened after the run started already carries `run_start`) and by the
/// live update that adds an event after the seed was written.
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
                // `map_exit_code` gives 128 + signal for a signal death, which
                // is what a terminated command normally reports; a command
                // that caught SIGTERM and exited reports its own code.
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

/// One recording's metadata in the archive: `sampling_interval_ms`, `source`,
/// `version`, `producer_epoch`, user `--metadata`, `systeminfo`,
/// `descriptions`, and the run events.
///
/// `run_events` are the wrapped run's events known so far, so a recording
/// opened after the command spawned (a late-joining endpoint) carries
/// `run_start` from its seed rather than waiting for an update that already
/// happened.
fn build_metadata(
    config: &RecordingConfig,
    ep: &EndpointState,
    run_events: &[Event],
) -> std::collections::BTreeMap<String, String> {
    // The recorder's own keys: above the probe hook's and the handshake's,
    // below the user's `--metadata`.
    let mut recorder = std::collections::BTreeMap::new();
    recorder.insert(
        "sampling_interval_ms".to_string(),
        config.interval.as_millis().to_string(),
    );
    if let Some(ref epoch) = ep.agent.producer_epoch {
        recorder.insert(
            parquet_metadata::KEY_PRODUCER_EPOCH.to_string(),
            epoch.clone(),
        );
    }
    // `--endpoint url,role=...`. Parquet carried it in its per-source map,
    // which a `.dendro` has no equivalent of; one recording per endpoint
    // makes it a key of that recording's metadata.
    if let Some(ref role) = ep.config.role {
        recorder.insert(parquet_metadata::NESTED_ROLE.to_string(), role.clone());
    }
    let mut m = identity(config, ep, recorder).metadata();
    // Cannot fail on a map with no `events` key yet, and a user `--metadata
    // events=...` that does not parse is the user's to fix: their value is
    // kept and the run events are the ones dropped, with a warning.
    if let Err(e) = merge_events_into(&mut m, run_events) {
        warn!("not adding the run events to the recording's metadata: {e}");
    }
    m
}

/// The recording's label set: the handshake's, under the probe hook's (for
/// an agent, `host` from its systeminfo), under the user's `--label k=v`,
/// with `source` resolved as `detect::Identity::source` says.
fn build_labels(
    config: &RecordingConfig,
    ep: &EndpointState,
) -> std::collections::BTreeMap<String, String> {
    identity(config, ep, BTreeMap::new()).labels()
}

/// The `.dendro` archive the recording loop feeds, through metriken-storage's
/// writer: groups with slots are written long, with their occupants in a
/// stream beside each.
///
/// A wrapper rather than a bare writer because the recording loop needs
/// things the writer has no opinion about: what to tell the user after a
/// mid-recording failure, and how to leave nothing behind when a run captured
/// no samples at all.
///
/// The writer is **declared after the recorders deliberately.** Fields drop
/// in declaration order, and dropping the writer joins its thread. Dropping
/// it first would not block (`join` sends a shutdown before releasing its own
/// sender, and the writer honours it whoever still holds a clone), but it
/// would stop the writer while the recordings could still queue their final
/// seals, silently losing them.
struct Archive {
    /// One recorder per recording, keyed by the endpoint index it serves:
    /// sparse rather than a `Vec` parallel to `endpoints`, because an
    /// endpoint gets one only once it is active.
    recs: BTreeMap<usize, metriken_storage::SourceRecorder>,
    /// This tick's rows, per recording, waiting for one commit. Cleared by
    /// `commit_tick`.
    staged: Vec<metriken_storage::writer::Staged>,
    /// Streamed endpoints only: per endpoint, the decoder that turns its
    /// rows back into the groups the writer stages.
    decoders: BTreeMap<usize, metriken_storage::StreamDecoder>,
    writer: metriken_storage::ArchiveWriter,
    /// The archive's path; the writer does not report it.
    path: std::path::PathBuf,
    /// Each recording's metadata map as last written, keyed by endpoint
    /// index like the recorders. Kept so a run event can be merged into the
    /// events payload already there; the writer does not hand it back.
    metadata: BTreeMap<usize, BTreeMap<String, String>>,
    /// The first stamp each recording ingested: where the epoch it opened
    /// with begins, should the agent restart. See `recorder::restart`.
    first_stamp: BTreeMap<usize, u64>,
    /// The last stamp each recording ingested, for its closing clock
    /// observation.
    ///
    /// Per recording rather than one for the archive: a recording that keeps
    /// the agent's timestamps must close on the agent's clock, and one that
    /// keeps the recorder's must close on the recorder's. One value for all of
    /// them would be right for at most one.
    last_stamp: std::collections::BTreeMap<usize, (u64, i64)>,
    /// Canonical label key -> the endpoint URL that claimed it first, for the
    /// indistinguishable-labels warning.
    ///
    /// Held here rather than locally in `start_recorder` because an endpoint
    /// that was down at startup opens its recording later, and it has to be
    /// checked against the recordings already open.
    seen_labels: BTreeMap<String, String>,
}

/// A metriken-storage error as the recorder reports errors.
fn archive_err(e: Box<dyn std::error::Error + Send + Sync>) -> String {
    e.to_string()
}

impl Archive {
    /// Stage one endpoint's scrape. Nothing reaches the archive until
    /// [`commit_tick`](Self::commit_tick).
    ///
    /// A snapshot with no recording to land in is an ERROR, not a skip:
    /// returning `Ok` would decode a scrape and drop it, every tick, for the
    /// whole run, and the archive would finalize one recording short. Every
    /// endpoint opens its recording first, at startup or on activation, so
    /// reaching that arm means that invariant broke.
    ///
    /// Staged rather than committed because the archive commits the whole tick
    /// at once: at `synchronous=FULL` every commit is an fsync, and the
    /// hand-off is a blocking send from inside the scrape loop, so a commit per
    /// endpoint put a linear-in-endpoint-count fsync bill on the tick.
    fn stage(
        &mut self,
        endpoint: usize,
        url: &Url,
        snapshot: &metriken_exposition::Snapshot,
        anchored_ts: u64,
        wall_offset_ns: i64,
    ) -> Result<(), String> {
        self.first_stamp.entry(endpoint).or_insert(anchored_ts);
        self.last_stamp
            .insert(endpoint, (anchored_ts, wall_offset_ns));
        let rec = self.recs.get_mut(&endpoint).ok_or_else(|| {
            format!(
                "{url} was scraped with no recording open for it; its samples would be \
                 discarded"
            )
        })?;
        self.staged.push(
            rec.stage(snapshot, anchored_ts, wall_offset_ns)
                .map_err(archive_err)?,
        );
        Ok(())
    }

    /// Stage one interval off an endpoint's replication stream.
    ///
    /// [`stage`](Self::stage) for the stream path: each pass's rows are
    /// decoded by the endpoint's `metriken_storage::StreamDecoder` and staged
    /// with `stage_streamed`.
    ///
    /// The stamp is the producer's: an interval carries when the agent
    /// sampled, and there is no recorder-side reading to prefer over it.
    fn stage_stream(
        &mut self,
        endpoint: usize,
        url: &Url,
        applied: stream::Applied,
    ) -> Result<(), String> {
        let passes = applied.for_writer()?;
        let Some(rec) = self.recs.get_mut(&endpoint) else {
            return Err(format!(
                "{url} streamed an interval with no recording open for it; its rows \
                 would be discarded"
            ));
        };
        let decoder = self.decoders.entry(endpoint).or_default();
        let before = decoder.unresolved;
        for pass in passes {
            let ts = u64::try_from(pass.ts)
                .map_err(|_| format!("{url} stamped a pass at {} ns, before the epoch", pass.ts))?;
            let wall_offset = pass.wall_offset;
            self.first_stamp.entry(endpoint).or_insert(ts);
            self.last_stamp.insert(endpoint, (ts, wall_offset));
            let groups = decoder.decode(pass.rows)?;
            self.staged.push(
                rec.stage_streamed(groups, ts, wall_offset)
                    .map_err(archive_err)?,
            );
        }
        if decoder.unresolved > before {
            warn!(
                "{url}: {} streamed rows named a schema this connection had not sent; \
                 skipped",
                decoder.unresolved - before
            );
        }
        Ok(())
    }

    /// Commit every endpoint staged this tick, as one transaction.
    ///
    /// Called once per tick whether or not anything staged: an empty commit
    /// does not write, but it does check the writer is still alive, so a run
    /// whose endpoints all went quiet cannot sit on a dead writer unnoticed.
    fn commit_tick(&mut self) -> Result<(), String> {
        self.writer
            .commit(std::mem::take(&mut self.staged))
            .map_err(archive_err)
    }

    /// Open a recording for endpoint `idx`: at startup, or for an endpoint
    /// that became reachable after the archive was created.
    ///
    /// The writer thread is still running, so a recording can join an open
    /// archive at any point. The endpoint's `systeminfo` must already be
    /// fetched — that is what supplies the `host` label — which is why the
    /// caller does this after the metadata fetch rather than at probe time.
    ///
    /// `run_events` are the wrapped run's events so far; a recording opened
    /// after the command spawned gets `run_start` in its seed.
    fn add_endpoint(
        &mut self,
        idx: usize,
        config: &RecordingConfig,
        ep: &EndpointState,
        clock_anchor_wall_ns: u64,
        run_events: &[Event],
    ) -> Result<(), String> {
        let labels = build_labels(config, ep);
        warn_if_indistinguishable(&mut self.seen_labels, &labels, &ep.config.url);
        let metadata = build_metadata(config, ep, run_events);
        // The SOURCE's anchor where there is one. This recording's rows carry
        // the agent's timestamps, so anchoring it on the recorder's clock
        // instead would make `ts + wall_offset` resolve against a reading
        // neither party took — and the two clocks are on different hosts, so
        // the error is the skew between them, not a rounding.
        //
        // A Prometheus source keeps the recorder's: that recording's rows are
        // the recorder's stamps too, so the pair stays coherent either way.
        // Coherence is per recording, which is what lets one archive hold
        // both.
        let anchor = ep
            .agent
            .clock_anchor_wall_ns
            .and_then(|a| u64::try_from(a).ok())
            .unwrap_or(clock_anchor_wall_ns);
        let rec = self
            .writer
            .add_source(labels, metadata.clone(), anchor)
            .map_err(archive_err)?;
        self.recs.insert(idx, rec);
        self.metadata.insert(idx, metadata);
        Ok(())
    }

    /// Add `events` to every open recording's `KEY_EVENTS` payload.
    ///
    /// Each recording's stored map is cloned and the events merged in
    /// (`merge_events_into`, which parses what is there and dedups by id),
    /// and the one key is written through [`patch_metadata`](Self::patch_metadata).
    /// Every recording is attempted even if an earlier one failed; the first
    /// error is reported.
    fn merge_events(&mut self, events: &[Event]) -> Result<(), String> {
        let mut first_err = None;
        let indices: Vec<usize> = self.recs.keys().copied().collect();
        for idx in indices {
            let Some(current) = self.metadata.get(&idx) else {
                first_err.get_or_insert(format!(
                    "the recording for endpoint {idx} has no metadata to add events to"
                ));
                continue;
            };
            let mut merged = current.clone();
            let result = merge_events_into(&mut merged, events).and_then(|()| {
                let patch = merged
                    .remove_entry(crate::parquet_metadata::KEY_EVENTS)
                    .into_iter()
                    .collect();
                self.patch_metadata(idx, patch)
            });
            if let Err(e) = result {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Replace the keys in `patch` in one recording's metadata, keeping every
    /// other key.
    ///
    /// Sent to the writer as the patch itself, since dendro merges a patch
    /// key by key. The stored copy is updated only once the writer accepts
    /// it, so a failed update can be retried with the same input.
    fn patch_metadata(
        &mut self,
        idx: usize,
        patch: BTreeMap<String, String>,
    ) -> Result<(), String> {
        let Some(current) = self.metadata.get(&idx) else {
            return Err(format!(
                "the recording for endpoint {idx} has no metadata to update"
            ));
        };
        let mut merged = current.clone();
        merged.extend(patch.clone());
        let rec = self
            .recs
            .get_mut(&idx)
            .ok_or_else(|| format!("no recording is open for endpoint {idx}"))?;
        rec.update_metadata(patch).map_err(archive_err)?;
        self.metadata.insert(idx, merged);
        Ok(())
    }

    /// Record the agent restarts waiting on endpoint `idx` in its recording's
    /// metadata: see [`restart::metadata_patch_with`]. `version_pinned` means
    /// the user set `version` with `--metadata`, which a restart does not
    /// overwrite.
    ///
    /// `handshake` is the new process's handshake metadata. Its keys other
    /// than the ones the patch already sets, the recorder's own and the
    /// user's are written too, each combined with the value on record by the
    /// hook's [`merge`](detect::Probe::merge).
    #[allow(clippy::too_many_arguments)]
    fn record_restarts(
        &mut self,
        idx: usize,
        restarts: &[restart::Restart],
        agent: &restart::Agent,
        handshake: &BTreeMap<String, String>,
        user_metadata: &[(String, String)],
        version_pinned: bool,
        probe: &dyn detect::Probe,
    ) -> Result<(), String> {
        let Some(current) = self.metadata.get(&idx) else {
            return Err(format!(
                "the recording for endpoint {idx} has no metadata to update"
            ));
        };
        let first_ts = self.first_stamp.get(&idx).copied();
        let merge = |key: &str, old: &str, new: &str| probe.merge(key, old, new);
        let mut patch = restart::metadata_patch_with(
            current,
            first_ts,
            restarts,
            agent,
            version_pinned,
            &merge,
        );
        for (key, value) in handshake {
            let owned = matches!(
                key.as_str(),
                "source"
                    | "sampling_interval_ms"
                    | parquet_metadata::KEY_PRODUCER_EPOCH
                    | parquet_metadata::KEY_PRODUCER_EPOCHS
                    | parquet_metadata::KEY_VERSION
                    | parquet_metadata::KEY_SYSTEMINFO
                    | parquet_metadata::KEY_DESCRIPTIONS
                    | parquet_metadata::NESTED_ROLE
                    | parquet_metadata::KEY_EVENTS
            );
            if owned || patch.contains_key(key) || user_metadata.iter().any(|(k, _)| k == key) {
                continue;
            }
            let merged = match current.get(key) {
                Some(old) => probe.merge(key, old, value),
                None => value.clone(),
            };
            patch.insert(key.clone(), merged);
        }
        self.patch_metadata(idx, patch)
    }

    /// Run every recording's seal check.
    ///
    /// All of them every tick, not just the ones that delivered rows: a seal
    /// decision that were ingest-driven would leave an unreachable endpoint's
    /// pre-outage rows unsealed forever, and the age bound would stop bounding
    /// the kill-loss window. Reports the first failure but still checks the
    /// rest, so a failure is attributed to the recording that caused it rather
    /// than to whichever happened to be checked first — note this is about
    /// reporting, not survival: the recordings share one writer thread, and a
    /// failure in any of its arms tears that writer down for all of them.
    fn maybe_seal(&mut self) -> Result<(), String> {
        let mut first_err = None;
        for rec in self.recs.values_mut() {
            if let Err(e) = rec.maybe_seal() {
                first_err.get_or_insert(archive_err(e));
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Mark the recording complete and stop the writer, reporting either
    /// failure.
    ///
    /// Both halves matter. A recorder's `finalize` only *queues* the
    /// completion — the writer owns the thread — and the final seal it
    /// triggers runs after that hand-off returns. So the writer is joined
    /// here, unconditionally, and its result is folded in: without it a
    /// failure while sealing the last segments would surface only as a `Drop`
    /// warning and the recording would report success.
    fn finalize(self, clock_offset: (u64, i64)) -> Result<(), String> {
        let Archive {
            recs,
            mut writer,
            last_stamp,
            ..
        } = self;
        // This recording's own last observation. The argument is the fallback
        // for one that ingested nothing, which has no clock of its own to
        // close on.
        let close = |idx: usize| last_stamp.get(&idx).copied().unwrap_or(clock_offset);
        // Every recording is finalized, even if an earlier one failed: they
        // are independent rows in one archive, and stopping at the first
        // failure would leave the rest marked incomplete for a fault that was
        // not theirs. The join is unconditional, and after every handle has
        // been consumed: it can only complete once they have all released
        // their senders.
        let mut first_err = None;
        for (idx, rec) in recs {
            if let Err(e) = rec.finalize(close(idx)) {
                first_err.get_or_insert(archive_err(e));
            }
        }
        let joined = writer.join().map_err(archive_err);
        first_err.map_or(joined, Err)
    }

    /// What to tell the user after a mid-recording failure: where the data
    /// captured so far can still be read from, which is the output path
    /// itself, since the file is a valid archive from the moment it is
    /// created.
    fn recovery_note(&self) -> String {
        format!(
            "note: the recording so far is readable at {}",
            self.path.display()
        )
    }

    /// Stop the writer and leave nothing behind. Only for the paths where the
    /// recording captured no samples at all — a stub is not a recovery
    /// artifact, and the writer refuses to overwrite, so leaving one behind
    /// would also block the retry.
    fn discard(self) {
        let path = self.path.clone();
        // Drop first: joining the writer thread is what guarantees nothing is
        // still appending to the file we are about to unlink. There is no
        // abort — a dropped writer leaves a valid recording, which is exactly
        // why this path has to remove it explicitly rather than rely on a
        // staging convention. The recorders drop before the writer (see
        // `Archive`).
        drop(self);
        // Safe to remove: the writer claimed this path with O_EXCL during
        // THIS run, so it cannot be a file that was already there.
        //
        // Sidecars included. A SQLite archive is three files while it is
        // open, and SQLite only cleans `-wal`/`-shm` up on a CLEAN close — so
        // removing the main file alone can leave a stray `-wal` beside a path
        // the next run believes is free. That is worse than a stale main
        // file: `O_EXCL` catches the main file and says so, while a stray
        // sidecar is adopted silently by the newly created database.
        rez_sqlite::RezDb::remove_archive(&path);
    }
}

/// Open the archive and one recording per endpoint in `eps`, creating the
/// output file.
///
/// `eps` carries each endpoint's index alongside it so the returned archive
/// can route a scrape or a streamed interval back to the recording that owns
/// it.
///
/// Every recording lands in ONE archive, which is why the arms of an A/B
/// captured this way share a clock anchor and a load environment rather than
/// differing in both, as two sequential single-endpoint runs would.
fn start_recorder(
    config: &RecordingConfig,
    eps: &[(usize, &EndpointState)],
    clock_anchor_wall_ns: u64,
) -> Result<Archive, String> {
    let writer = metriken_storage::ArchiveWriter::create(
        &config.output,
        metriken_storage::WriterConfig::default(),
    )
    .map_err(|e| format!("failed to create {}: {e}", config.output.display()))?;
    let mut archive = Archive {
        recs: BTreeMap::new(),
        staged: Vec::new(),
        decoders: BTreeMap::new(),
        writer,
        path: config.output.clone(),
        metadata: BTreeMap::new(),
        first_stamp: BTreeMap::new(),
        last_stamp: BTreeMap::new(),
        seen_labels: BTreeMap::new(),
    };

    for (idx, ep) in eps {
        // No run events yet: the archive is opened before the wrapped command
        // is spawned, so `run_start` reaches these recordings by update.
        if let Err(e) = archive.add_endpoint(*idx, config, ep, clock_anchor_wall_ns, &[]) {
            // `ArchiveWriter::create` claimed the path with O_EXCL moments
            // ago, so the half-built archive is unambiguously ours to remove.
            // Leaving it would both look like a recording and block the
            // retry, since the writer refuses to overwrite an existing path.
            archive.discard();
            return Err(e);
        }
    }

    Ok(archive)
}

/// Warn when a recording's labels match one already open.
///
/// Two recordings with identical label sets are indistinguishable to every
/// consumer — the viewer aliases A/B off their labels, and the seal stagger
/// keys on the label set, so they would also seal in lockstep. Warn rather
/// than refuse: the recording is still valid and the operator may not care,
/// but nothing downstream can tell the arms apart and they should know before
/// the run rather than after.
fn warn_if_indistinguishable(
    seen: &mut BTreeMap<String, String>,
    labels: &BTreeMap<String, String>,
    url: &Url,
) {
    let key = seal_policy::recording_stagger_key(labels);
    if let Some(warning) = indistinguishable_warning(seen, &key, url) {
        eprintln!("{warning}");
    }
    seen.insert(key, url.to_string());
}

/// What is wrong with this recording's labels against the ones already seen,
/// or `None` when nothing is.
///
/// Split from the printing so the DECISION is testable: whether a pair warns,
/// and which of the two warnings it earns, is the part with rules in it.
fn indistinguishable_warning(
    seen: &BTreeMap<String, String>,
    key: &str,
    url: &Url,
) -> Option<String> {
    if let Some(other) = seen.get(key) {
        return Some(format!(
            "warning: {other} and {url} carry identical labels ({key:?}), so nothing \
             downstream can tell their recordings apart — give each --endpoint its own \
             source=NAME to distinguish them"
        ));
    }
    // Distinguishable to a reader, but NOT to the seal stagger: two keys
    // differing only in bit 5 of an even number of bytes draw the same bucket
    // for every sampler (in printable ASCII, the same labels in different
    // capitalisation). Their recordings would then seal in permanent lockstep,
    // doubling the co-seal batch exactly when the archive holds twice the
    // tables — the failure the stagger exists to prevent, reached by two names
    // an operator would read as different.
    //
    // Warned rather than hashed around: every hash that closes this aliasing
    // spreads a real sampler set at or worse than random, where this one
    // spreads it perfectly. See `seal_policy::stagger_bucket`.
    if let Some((other_key, other_url)) = seen
        .iter()
        .find(|(k, _)| seal_policy::staggers_identically(k, key))
    {
        return Some(format!(
            "warning: {other_url} ({other_key:?}) and {url} ({key:?}) differ only in \
             capitalisation, which the seal stagger cannot tell apart — their recordings \
             will seal in lockstep. Give them labels that differ by more than case"
        ));
    }
    None
}

/// Derive one tick's row stamp from the recording's clock anchor.
///
/// Returns `(anchored_ns, wall_offset_ns)`. The anchored stamp is
/// `anchor + monotonic elapsed`, so row timestamps are strictly increasing for
/// as long as the recording runs — a recorder-side clock step cannot bake a
/// decreasing timestamp into an immutable sealed segment (which would feed
/// `rate()` a dt <= 0). The raw wall reading is not discarded: its difference
/// from the anchored stamp rides along as the per-row `:wall_offset` sidecar,
/// so a step locates to the exact tick.
pub(crate) fn anchored_stamp(anchor_wall_ns: u64, elapsed: Duration, wall_ns: u64) -> (u64, i64) {
    let anchored_ns = anchored_at(anchor_wall_ns, elapsed);
    (anchored_ns, wall_ns as i64 - anchored_ns as i64)
}

/// A moment on the recording's timeline: `anchor + monotonic elapsed`.
///
/// Everything the recorder timestamps goes through here, so a row's `ts` and
/// the window edges it is stored relative to come from one clock. Mixing the
/// two was the defect this exists to prevent.
pub(crate) fn anchored_at(anchor_wall_ns: u64, elapsed: Duration) -> u64 {
    anchor_wall_ns.saturating_add(elapsed.as_nanos() as u64)
}

/// Upper bound on a single scrape or endpoint probe, whatever the interval.
/// The bound exists to keep the tick responsive (age seals, ctrl-c), and that
/// is a human/container-teardown timescale — a long `--interval` must not buy a
/// correspondingly long stall.
const MAX_SCRAPE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a single scrape, probe or stream connect may take before the tick
/// gives up on it.
///
/// Without a bound a *hung* endpoint (stalled server, SYN blackhole) parks the
/// tick for TCP-timeout scales, and the tick is what drives age seals
/// and the ctrl-c check (`STATE` is only re-checked at the loop top) — so one
/// hung endpoint would stall durability and shutdown, not just this sample.
///
/// Deliberately generous rather than exactly one interval: this must catch a
/// *hung* endpoint, not a merely slow one. A local agent already takes ~75 ms
/// to answer, so at `--interval 5ms` a one-interval bound would time out every
/// single scrape and record nothing at all, where the honest outcome is
/// sampling at the endpoint's pace. Floored so short intervals stay
/// recordable, capped so a long interval still hands back a bounded tick.
pub(crate) fn tick_timeout(interval: Duration) -> Duration {
    (interval * 2).clamp(Duration::from_secs(2), MAX_SCRAPE_TIMEOUT)
}

/// Intervals a stream pump may have queued for the tick loop before its send
/// blocks, per endpoint.
///
/// The loop drains the queue every tick, so this only fills when the loop
/// falls behind the agent's frame rate — a slow commit, or an `--interval`
/// shorter than the writer can keep up with. Bounded so that case pushes back
/// on the socket rather than growing without limit.
pub(crate) const STREAM_QUEUE_PER_ENDPOINT: usize = 16;

/// Runs the Rezolus `recorder`, which records one or more endpoints into one
/// `.dendro` archive: Rezolus agents from their replication stream,
/// Prometheus endpoints by scraping them, detected per endpoint.
pub fn run(config: RecordingConfig) {
    let _log_drain = configure_logging(verbosity_to_level(config.verbose));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    // Raised by the ctrl-c / SIGTERM handler alongside `STATE`. The recording
    // loop only re-reads `STATE` at the loop top, so without a way to cut the
    // tick wait short a clean stop costs up to a full `--interval`.
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
            // Store-then-notify: `notify_one` leaves a permit if the loop is
            // not parked yet, so a signal that lands between the loop-top
            // `STATE` read and the tick wait is never lost.
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

    let mut endpoints: Vec<EndpointState> = config
        .endpoints
        .iter()
        .map(|ep| EndpointState::new(ep.clone()))
        .collect();

    let interval_dur: Duration = config.interval.into();
    let connect_timeout = tick_timeout(interval_dur);

    // Subscriptions opened at startup, one slot per endpoint, handed to their
    // pumps once the archive they feed exists. Only streamed endpoints fill
    // any.
    let mut opened: Vec<Option<stream::Subscription>> = endpoints.iter().map(|_| None).collect();

    // The hook that recognises a Rezolus agent and reads what its handshake
    // does not carry (see `agent_probe`).
    let probe: std::sync::Arc<dyn detect::Probe> =
        std::sync::Arc::new(agent_probe::RezolusProbe::new(client.clone()));
    let detect_cfg = detect_config(interval_dur, connect_timeout);

    // Detect every endpoint before the archive exists (see `detect`). A
    // refusal is fatal here, so nothing is left behind; an endpoint not known
    // yet is the ordinary retry-each-tick case.
    rt.block_on(async {
        for (idx, ep) in endpoints.iter_mut().enumerate() {
            let url = ep.config.url.clone();
            let transport = Protocol::transport(ep.config.protocol.as_ref());
            match detect::detect(
                &client,
                &detect_cfg,
                &url,
                transport,
                probe.as_ref(),
                detect::At::Startup,
            )
            .await
            {
                detect::Detected::Streamed(sub) => {
                    begin_stream(ep, &sub, interval_dur);
                    info!("endpoint {url}: subscribed to its replication stream");
                    opened[idx] = Some(*sub);
                }
                detect::Detected::Scraped(scrape) => {
                    info!("endpoint {url}: scraped as Prometheus at {scrape}");
                    begin_scrape(ep, scrape);
                }
                detect::Detected::Pending(why) => warn!("{why}"),
                detect::Detected::Refused(reason) => {
                    eprintln!("error: {reason}");
                    std::process::exit(1);
                }
            }
        }

        // The probe hook's first `enrich` of every streamed endpoint, at
        // once, each bounded by the timeout: a recording's labels are fixed
        // when it is added, so they are read before the archive opens.
        let enriched =
            futures::future::join_all(opened.iter().enumerate().filter_map(|(idx, sub)| {
                let sub = sub.as_ref()?;
                let epoch = sub.source().and_then(|s| s.uuid.clone());
                let url = endpoints[idx].config.url.clone();
                let probe = probe.clone();
                Some(async move {
                    let reply = detect::enrich_within(
                        probe.as_ref(),
                        &url,
                        epoch.as_deref(),
                        connect_timeout,
                    )
                    .await;
                    (idx, reply)
                })
            }))
            .await;
        for (idx, reply) in enriched {
            adopt_enrich(&mut endpoints[idx], reply);
        }
    });
    for ep in endpoints
        .iter_mut()
        .filter(|ep| ep.status == EndpointStatus::Active)
    {
        resolve_source(&config, ep);
    }
    if !endpoints
        .iter()
        .any(|ep| ep.status == EndpointStatus::Active)
    {
        eprintln!("error: no endpoints could be reached. Check your configuration.");
        std::process::exit(1);
    }

    // ONE clock anchor for the whole recording, never re-anchored: rows are
    // stamped `clock_anchor_wall_ns + clock_anchor_mono.elapsed()` so they are
    // strictly increasing even across a recorder-side NTP step. Tick
    // *scheduling* is already monotonic (`aligned_interval`); this makes the
    // stamps consistent with it. See `anchored_stamp`.
    let clock_anchor_wall_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let clock_anchor_mono = Instant::now();

    // The archive is opened here — when the endpoints become active, not
    // lazily on the first snapshot — because creating the output and
    // spawning the writer thread are both fallible. Every endpoint active at
    // this point becomes a recording in it; one that activates later through
    // the Pending path is added when it does.
    let active: Vec<(usize, &EndpointState)> = endpoints
        .iter()
        .enumerate()
        .filter(|(_, ep)| ep.status == EndpointStatus::Active)
        .collect();
    let mut archive: Option<Archive> = match start_recorder(&config, &active, clock_anchor_wall_ns)
    {
        Ok(archive) => Some(archive),
        Err(e) => {
            eprintln!("error: failed to start the recording: {e}");
            std::process::exit(1);
        }
    };

    // Per-endpoint Prometheus converters: one per endpoint, because each
    // holds the (name, labels) -> id map that keeps a column's identity
    // stable across scrapes, and two endpoints' id spaces must not mix.
    let mut prom_converters: Vec<Option<prometheus::PrometheusConverter>> = endpoints
        .iter()
        .map(|ep| {
            (ep.status == EndpointStatus::Active && ep.protocol() == Some(&Protocol::Prometheus))
                .then(prometheus::PrometheusConverter::new)
        })
        .collect();

    if config.command.is_some() {
        info!("recording while command runs... ctrl-c to stop early");
    } else if config.duration.is_some() {
        info!("recording metrics... ctrl-c to terminate early");
    } else {
        info!("recording metrics... ctrl-c to end the recording");
    }

    let wrapped = config.command.is_some();

    // A fatal mid-recording write failure must not look like success: a
    // supervisor, CI job, or docker healthcheck can only see the exit code.
    let recording_failed = AtomicBool::new(false);

    let outcome: Option<child::Outcome> = rt.block_on(async {
        // Spawn the wrapped command only after probing/writers succeeded, so a
        // failed setup never starts an expensive workload.
        let mut child = if let Some(ref cmd) = config.command {
            match child::spawn(cmd) {
                Ok(c) => Some(c),
                Err(e) => {
                    eprintln!("error: failed to start command: {e}");
                    // Nothing was recorded, and `exit` skips destructors: stop
                    // the writer thread and remove the empty recording
                    // explicitly.
                    if let Some(rec) = archive.take() {
                        rec.discard();
                    }
                    std::process::exit(1);
                }
            }
        } else {
            None
        };
        let mut outcome: Option<child::Outcome> = None;

        // The wrapped run's marker events. `run_start` is stamped now, on the
        // recorder's clock (the same `anchored_at` the rows use), not from
        // the child's own start time; it goes to the open recordings at once
        // and rides in the seed of any recording opened later.
        let run_marker = config.command.as_deref().map(RunMarker::new);
        let mut run_events: Vec<Event> = Vec::new();
        if let (Some(marker), Some(cmd)) = (&run_marker, config.command.as_deref()) {
            let command_line = config.record_command_line.then(|| cmd.join(" "));
            let spawned_at = anchored_at(clock_anchor_wall_ns, clock_anchor_mono.elapsed());
            run_events.push(marker.start_event(spawned_at, command_line));
            if let Some(rec) = archive.as_mut() {
                // Not fatal: the samples are the product and they are
                // unaffected. A writer that has died surfaces on the next
                // tick's commit with the writer's own error.
                if let Err(e) = rec.merge_events(&run_events) {
                    warn!("failed to add the run_start event to the recording: {e}");
                }
            }
        }
        // The `run_end` event, built at whichever site observes the exit so
        // its stamp is that instant, and merged after the loop.
        let mut run_end: Option<Event> = None;
        // The instant the select arm below saw the child exit, if it did;
        // the loop top's poll stamps `run_end` with it rather than with its
        // own, later, reading.
        let mut child_exit_seen: Option<u64> = None;
        // The wall clock when the wrapped command was seen to exit on its
        // own, and each endpoint's frame count at that moment: what the
        // post-exit wait for streamed frames measures against. Not set for a
        // capped or interrupted command.
        let mut child_exit_wall: Option<(u64, Vec<u64>)> = None;
        // The exit arm fires once. After that the tick governs, whether the
        // wait succeeded (the loop top takes it from here) or failed (a
        // failed wait would fire every iteration and spin the loop).
        let mut exit_arm_armed = true;
        // Set once the body has run its one pass after the child exited.
        let mut final_scrape_done = false;
        // True when the previous pass ran the body to its end, so the loop
        // top can tell an exit that landed during that scrape (already
        // sampled) from one that landed while waiting for the tick (not).
        // Cleared on every `continue`, since those passes scrape nothing.
        let mut scraped_last_pass = false;

        let start = Instant::now() + interval_dur;
        // In wrapped mode the cap is intentionally measured from command spawn
        // (`Instant::now()`), which differs from the non-wrapped path's `start`
        // reference (now + interval). Do not unify these — they are distinct by
        // design: the cap bounds the child's lifetime, `start` bounds recording.
        let cap_deadline: Option<Instant> =
            config.duration.map(|d| Instant::now() + Duration::from(d));
        let mut interval = crate::common::aligned_interval(interval_dur);
        // See `tick_timeout` for why this is generous rather than one interval.
        let scrape_timeout = tick_timeout(interval_dur);

        // The stream pumps. One task per subscribed endpoint, each holding
        // its connection and sending intervals down one channel the loop
        // drains every tick; see `stream::pump` for why the loop does not
        // await the connections itself. Spawned here rather than at startup
        // so nothing is in flight before the archive they feed exists.
        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::channel::<(usize, stream::StreamEvent)>(
            STREAM_QUEUE_PER_ENDPOINT * endpoints.len().max(1),
        );
        let spawn_pump = |idx: usize, sub: stream::Subscription, url: Url| {
            tokio::spawn(stream::pump(
                idx,
                sub,
                client.clone(),
                url,
                interval_dur,
                stream::Layout::Long,
                scrape_timeout,
                stream_tx.clone(),
            ));
        };
        for (idx, sub) in opened.iter_mut().enumerate() {
            if let Some(sub) = sub.take() {
                spawn_pump(idx, sub, endpoints[idx].config.url.clone());
            }
        }
        let hook = Hook {
            probe: probe.as_ref(),
            timeout: scrape_timeout,
        };
        // Endpoints detected as streamed after startup, waiting for the
        // probe hook's `enrich`, which runs on its own task so no tick waits
        // on it: the subscription detection opened (unread meanwhile, which
        // costs at most a gap) and the task. One slot per endpoint.
        type Enriching = (
            stream::Subscription,
            tokio::task::JoinHandle<Result<detect::Enriched, String>>,
        );
        let mut enriching: Vec<Option<Enriching>> = endpoints.iter().map(|_| None).collect();
        // The last tick's clock observation, handed to finalization so
        // the manifest's `clock_offsets` series covers the tail of the
        // recording. Seeded with the anchor itself (offset 0 by definition).
        let mut last_clock: (u64, i64) = (clock_anchor_wall_ns, 0);

        // The same stop deadline the loop top already enforces, hoisted so the
        // tick wait can be cut short by it rather than overshooting it by up to
        // one interval. Wrapped mode uses the child's cap (measured from spawn),
        // everything else the recording window (measured from `start`); the
        // decision itself stays at the loop top, this only wakes it on time.
        let loop_deadline: Option<Instant> = if wrapped {
            cap_deadline
        } else {
            config.duration.map(|d| start + Duration::from(d))
        };
        let mut deadline_fired = false;

        while STATE.load(Ordering::Relaxed) == RUNNING {
            if wrapped {
                // Poll the wrapped command: exit ends recording, cap kills it.
                if let Some(c) = child.as_mut() {
                    match c.try_wait() {
                        Ok(Some(status)) => {
                            let code = child::map_exit_code(status);
                            info!("command exited (code {code}), finalizing recording");
                            outcome = Some(child::Outcome::Exited(code));
                            if child_exit_wall.is_none() {
                                child_exit_wall = Some((
                                    wall_now_ns(),
                                    endpoints.iter().map(|ep| ep.frames).collect(),
                                ));
                            }
                            if let Some(marker) = &run_marker {
                                // The instant the exit arm saw it, when it
                                // did; this poll's reading otherwise.
                                let observed_at = child_exit_seen.unwrap_or_else(|| {
                                    anchored_at(clock_anchor_wall_ns, clock_anchor_mono.elapsed())
                                });
                                run_end =
                                    Some(marker.end_event(observed_at, RunEnding::Exited(code)));
                            }
                            child = None;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            warn!("failed to poll command: {e}");
                        }
                    }
                }
                if child.is_some() {
                    if let Some(deadline) = cap_deadline {
                        if Instant::now() >= deadline {
                            info!("--duration reached, stopping command");
                            if let Some(mut c) = child.take() {
                                child::terminate(&mut c, child::TERM_GRACE).await;
                            }
                            outcome = Some(child::Outcome::Capped);
                            if let Some(marker) = &run_marker {
                                let observed_at =
                                    anchored_at(clock_anchor_wall_ns, clock_anchor_mono.elapsed());
                                run_end = Some(marker.end_event(observed_at, RunEnding::Capped));
                            }
                            break;
                        }
                    }
                } else {
                    // The command has exited. The body runs once more, without
                    // waiting for the tick, so the interval the command exited
                    // in is sampled; the pass after that stops. Without this
                    // an exit noticed here ended the recording with the last
                    // partial interval unsampled, and a command that exited
                    // before the first tick was recorded as nothing at all.
                    //
                    // Not when the exit landed during the scrape that just
                    // ran: the tick fired, the child exited while the body
                    // was scraping, and the exit arm never got to fire. That
                    // interval was sampled a few milliseconds ago, and a
                    // second pass would only add a redundant row right after
                    // it. An exit seen on the very first pass, before any
                    // scrape, still gets its one scrape.
                    if final_scrape_done || scraped_last_pass {
                        break;
                    }
                    final_scrape_done = true;
                }
            } else if let Some(duration) = config.duration.map(Into::<Duration>::into) {
                if start.elapsed() >= duration {
                    break;
                }
            }

            // The tick is this loop's only await point and `STATE` is only read
            // at the loop top, so an uninterruptible `tick()` bounds
            // ctrl-c/SIGTERM → exit by a full `--interval`: at `--interval 30s`
            // a `docker stop` (10s default grace) is SIGKILLed long before the
            // loop notices, losing every still-unsealed segment — which
            // is exactly the tear-down this feature exists for. Wake early on
            // the shutdown signal and on the stop deadline; both then take the
            // decision at the loop top, unchanged.
            //
            // Branch order is load-bearing (`biased`): the tick outranks the
            // deadline so that a `--duration` landing exactly on a tick — the
            // common case, a whole number of intervals — still takes that final
            // sample, exactly as the loop-top check did before. The deadline
            // only ever cuts a PARTIAL interval short.
            //
            // The wrapped command's exit wakes the loop too, so `run_end` is
            // stamped when the exit happened rather than up to one interval
            // later. The arm only records the instant and goes back to the
            // top, where `try_wait` takes the decision (tokio's `Child` caches
            // the status once `wait` has completed, so `try_wait` then returns
            // it) and schedules the final scrape. `Child::wait` is
            // cancel-safe, so dropping it on every tick loses nothing. The
            // async block borrows `child` mutably only for the duration of
            // the select, which is why the arm can sit beside the others.
            //
            // No wait at all on the pass after the exit: that pass exists to
            // sample the interval the command exited in, now.
            if !(wrapped && child.is_none()) {
                tokio::select! {
                    biased;
                    _ = shutdown.notified() => {
                        scraped_last_pass = false;
                        continue;
                    }
                    exited = async {
                        match child.as_mut() {
                            Some(c) => c.wait().await.is_ok(),
                            None => std::future::pending::<bool>().await,
                        }
                    }, if wrapped && exit_arm_armed => {
                        exit_arm_armed = false;
                        if exited {
                            child_exit_seen = Some(anchored_at(
                                clock_anchor_wall_ns,
                                clock_anchor_mono.elapsed(),
                            ));
                            child_exit_wall = Some((
                                wall_now_ns(),
                                endpoints.iter().map(|ep| ep.frames).collect(),
                            ));
                        }
                        // The exit came while waiting, after the last scrape,
                        // so the loop top must schedule the final one.
                        scraped_last_pass = false;
                        continue;
                    }
                    _ = interval.tick() => {}
                    _ = sleep_until_opt(loop_deadline), if !deadline_fired => {
                        // Latched so a deadline already in the past cannot spin the
                        // loop if the top declines to stop for some reason.
                        deadline_fired = true;
                        scraped_last_pass = false;
                        continue;
                    }
                }
            }
            // Both clocks, every tick. `wall_ns` is the raw reading (endpoint
            // success stamps); a scraped row is stamped on the anchored
            // monotonic timeline and carries the difference as a per-row
            // observation instead. The prometheus converter's scrape window
            // is stored relative to an anchored row `ts`, so it is anchored
            // too.
            let wall_ns = wall_now_ns();
            let (anchored_ns, wall_offset_ns) =
                anchored_stamp(clock_anchor_wall_ns, clock_anchor_mono.elapsed(), wall_ns);
            last_clock = (anchored_ns, wall_offset_ns);

            // An ingest failure for this tick. Held rather than acted on where
            // it happens: that is inside the per-endpoint result loop, whose
            // `break` would only leave that loop. Surfaced below, alongside
            // `maybe_seal`'s — the same class of failure at the same cadence,
            // and the earlier and more specific of the two wins.
            let mut ingest_failed: Option<String> = None;
            // An endpoint that activated mid-run and could not be given a
            // recording. Surfaced on the same path as an ingest failure, below,
            // so the partial archive is named and the run exits non-zero rather
            // than finalizing one recording short.
            let mut late_endpoint_failure: Option<String> = None;

            // Scrape all active Prometheus endpoints concurrently. A streamed
            // agent is not scraped: its pump delivers, and the loop stages
            // what arrived alongside the scrapes, below.
            let active_indices: Vec<usize> = endpoints
                .iter()
                .enumerate()
                .filter(|(_, ep)| ep.status == EndpointStatus::Active && !ep.streaming)
                .map(|(i, _)| i)
                .collect();

            let scrape_futures: Vec<_> = active_indices
                .iter()
                .map(|&idx| {
                    let client = client.clone();
                    let url = endpoints[idx].scrape_url.clone().unwrap();
                    async move {
                        let result = match tokio::time::timeout(
                            scrape_timeout,
                            scrape_one(&client, &url, clock_anchor_wall_ns, clock_anchor_mono),
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => Err(format!(
                                "timed out after {}",
                                humantime::format_duration(scrape_timeout)
                            )),
                        };
                        (idx, result)
                    }
                })
                .collect();

            let results = futures::future::join_all(scrape_futures).await;

            for (idx, result) in results {
                match result {
                    Ok(Scraped {
                        body,
                        request_ns,
                        response_ns,
                    }) => {
                        endpoints[idx].record_success(wall_ns);
                        // Only a Prometheus endpoint is scraped, and each has
                        // a converter from its activation.
                        let Some(conv) = prom_converters[idx].as_mut() else {
                            continue;
                        };
                        // The real round trip, not the tick's clock: every
                        // value in this body was read somewhere inside it.
                        // See `Scraped`.
                        let text = String::from_utf8_lossy(&body);
                        let snapshot = conv.convert(&text, request_ns, response_ns);
                        if let Some(rec) = archive.as_mut() {
                            match rec.stage(
                                idx,
                                &endpoints[idx].config.url,
                                &snapshot,
                                anchored_ns,
                                wall_offset_ns,
                            ) {
                                Ok(()) => endpoints[idx].note_row_staged(anchored_ns),
                                Err(e) => {
                                    ingest_failed.get_or_insert(e);
                                }
                            }
                            // The scrape's HELP lines, when it brought a name
                            // not yet stored. dendro's `update_metadata` only
                            // hands the patch to the writer thread, which logs
                            // and skips a patch it cannot apply. Only a failed
                            // hand-off is seen here; it is retried after the
                            // next scrape rather than ending the run.
                            if let Some(json) = conv.pending_descriptions() {
                                let patch = BTreeMap::from([(
                                    parquet_metadata::KEY_DESCRIPTIONS.to_string(),
                                    json,
                                )]);
                                match rec.patch_metadata(idx, patch) {
                                    Ok(()) => conv.descriptions_stored(),
                                    Err(e) => warn!(
                                        "not storing the HELP text of {} ({}) yet: {e}",
                                        endpoints[idx].config.source_label(),
                                        endpoints[idx].config.url
                                    ),
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "scrape failed for {} ({}): {e}",
                            endpoints[idx].config.source_label(),
                            endpoints[idx].config.url
                        );
                    }
                }
            }

            // Everything the pumps delivered since the last tick. Drained
            // rather than awaited: a tick commits what has arrived, and an
            // endpoint whose frame is late is committed next tick, exactly
            // as a scrape that missed the tick would be.
            if let Some(e) = drain_stream_events(
                &mut stream_rx,
                &mut endpoints,
                archive.as_mut(),
                wall_ns,
                hook,
            )
            .await
            {
                ingest_failed.get_or_insert(e);
            }
            apply_restarts(
                &mut endpoints,
                archive.as_mut(),
                probe.as_ref(),
                &config.metadata,
                scrape_timeout,
                false,
            )
            .await;

            // Late activations, in two steps so that no tick waits on the
            // probe hook's `enrich`. A streamed endpoint whose `enrich` has
            // returned (or timed out) gets its recording now; one detected
            // below starts its `enrich` on its own task and is added on a
            // later tick. A scraped endpoint has no `enrich` and is added at
            // once.
            //
            // Startup only exits when no endpoint at all was reachable, so a
            // run with one endpoint up and one still starting commits to the
            // archive with a recording for the first and reaches this path
            // for the second, which needs a recording opened on the live
            // archive. That is what keeps the "will retry each tick" warning
            // honest. When `archive` is `None` the recording already failed
            // and was reported; the loop is about to exit.
            let mut added: Vec<(usize, Option<stream::Subscription>)> = Vec::new();
            for idx in 0..endpoints.len() {
                let finished = enriching[idx]
                    .as_ref()
                    .is_some_and(|(_, task)| task.is_finished());
                if !finished {
                    continue;
                }
                let Some((sub, task)) = enriching[idx].take() else {
                    continue;
                };
                let reply = task
                    .await
                    .unwrap_or_else(|e| Err(format!("its metadata fetch failed: {e}")));
                adopt_enrich(&mut endpoints[idx], reply);
                endpoints[idx].status = EndpointStatus::Active;
                endpoints[idx].streaming = true;
                resolve_source(&config, &mut endpoints[idx]);
                added.push((idx, Some(sub)));
            }

            let pending_indices: Vec<usize> = endpoints
                .iter()
                .enumerate()
                .filter(|(_, ep)| ep.status == EndpointStatus::Pending)
                .map(|(i, _)| i)
                .collect();

            for idx in pending_indices {
                // The same detection as startup, with one difference: an
                // endpoint refused now is refused on its own. The archive
                // already holds the other endpoints' recordings, so they keep
                // recording and finalize as usual; the refused endpoint is
                // never retried, and the run exits 1 at the end.
                let url = endpoints[idx].config.url.clone();
                let transport = Protocol::transport(endpoints[idx].config.protocol.as_ref());
                let detected = detect::detect(
                    &client,
                    &detect_cfg,
                    &url,
                    transport,
                    probe.as_ref(),
                    detect::At::Activation,
                )
                .await;
                match detected {
                    detect::Detected::Pending(why) => debug!("{why}"),
                    detect::Detected::Refused(reason) => {
                        refuse_mid_run(&mut endpoints[idx], &reason);
                    }
                    detect::Detected::Streamed(sub) => {
                        info!(
                            "endpoint {url} now reachable, subscribed to its replication \
                             stream; reading its metadata"
                        );
                        begin_stream(&mut endpoints[idx], &sub, interval_dur);
                        // Not streaming until it is added and its pump runs:
                        // nothing waits for its frames before then.
                        endpoints[idx].status = EndpointStatus::Activating;
                        endpoints[idx].streaming = false;
                        let epoch = sub.source().and_then(|s| s.uuid.clone());
                        let probe = probe.clone();
                        let task = tokio::spawn(async move {
                            detect::enrich_within(
                                probe.as_ref(),
                                &url,
                                epoch.as_deref(),
                                scrape_timeout,
                            )
                            .await
                        });
                        enriching[idx] = Some((*sub, task));
                    }
                    detect::Detected::Scraped(scrape) => {
                        // Deliberately says "now reachable", not "starting
                        // capture": the block below can still fail to open a
                        // recording for it, and claiming capture had started
                        // one line before that error is how the silent-drop
                        // bug read in the logs.
                        info!("endpoint {url} now reachable, scraped as Prometheus at {scrape}");
                        begin_scrape(&mut endpoints[idx], scrape);
                        resolve_source(&config, &mut endpoints[idx]);
                        // A late Prometheus endpoint gets a converter now, the
                        // same as one present at startup.
                        if prom_converters[idx].is_none() {
                            prom_converters[idx] = Some(prometheus::PrometheusConverter::new());
                        }
                        added.push((idx, None));
                    }
                }
            }

            for (idx, sub) in added {
                if let Some(rec) = archive.as_mut() {
                    match rec.add_endpoint(
                        idx,
                        &config,
                        &endpoints[idx],
                        clock_anchor_wall_ns,
                        &run_events,
                    ) {
                        Ok(()) => {
                            if let Some(sub) = sub {
                                spawn_pump(idx, sub, endpoints[idx].config.url.clone());
                            }
                        }
                        Err(e) => {
                            // First failure wins, as `ingest_failed` does:
                            // two endpoints can activate in one tick.
                            late_endpoint_failure.get_or_insert(format!(
                                "failed to open a recording for {}: {e}",
                                endpoints[idx].config.url
                            ));
                        }
                    }
                }
            }

            // Commit the whole tick — every endpoint staged above — as ONE
            // transaction, before the seal check. One commit means one fsync at
            // `synchronous=FULL` however many endpoints the archive holds; a
            // commit per endpoint made the tick's cost scale with their count,
            // on the loop that has to keep up with the sampling interval.
            //
            // Every tick, scrape or not, for the same reason `maybe_seal` runs
            // unconditionally: an empty commit writes nothing but does check
            // the writer is alive, so a run whose endpoints all went quiet
            // cannot sit on a dead writer unnoticed.
            let committed = match archive.as_mut() {
                Some(rec) => rec.commit_tick(),
                None => Ok(()),
            };

            // Seal checks run every tick, scrape or not: if they were
            // ingest-driven an unreachable endpoint would leave its pre-outage
            // rows unsealed forever and the age bound would stop bounding the
            // kill-loss window.
            let sealed = match archive.as_mut() {
                Some(rec) => rec.maybe_seal(),
                None => Ok(()),
            };
            if let Some(e) = late_endpoint_failure
                .take()
                .or(ingest_failed)
                .or_else(|| committed.err())
                .or_else(|| sealed.err())
            {
                eprintln!("error: recording failed: {e}");
                recording_failed.store(true, Ordering::SeqCst);
                if let Some(rec) = archive.take() {
                    // Dropped, not discarded: what is on disk holds everything
                    // written before the failure and is the recovery artifact —
                    // in v2 every sealed segment, in v3 every committed tick.
                    eprintln!("{}", rec.recovery_note());
                }
                break;
            }
            scraped_last_pass = true;

            // Every endpoint refused mid-run: nothing is left to record, and
            // nothing will come back (a refused endpoint is never retried),
            // so the run ends here instead of waiting for `--duration` or
            // ctrl-c. The archive is finalized with what it holds and the
            // summary below says what was refused.
            if endpoints
                .iter()
                .all(|ep| ep.status == EndpointStatus::Refused)
            {
                warn!("every endpoint has been refused; finalizing the recording");
                break;
            }
        }

        // If the loop ended via ctrl-c (STATE flip) while the wrapped command
        // is still alive, terminate and reap it so we never orphan the child.
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
                let observed_at = anchored_at(clock_anchor_wall_ns, clock_anchor_mono.elapsed());
                run_end = Some(marker.end_event(observed_at, ending));
            }
        }
        // Every exit path of a wrapped run has produced its `run_end` by now.
        // It is merged into each recording right before finalizing.
        if let Some(event) = run_end.take() {
            run_events.push(event);
        }

        // ── Finalization ──────────────────────────────────────────────────

        // The interval in flight. The agent frames on its own boundary, not
        // on the recorder's tick, so the frame covering the moment the loop
        // ended arrives after it did. Without a wait, that frame is never
        // committed and the recording ends one interval short of the window
        // it was asked for.
        //
        // A wrapped command that exited on its own waits for frames: until
        // every streamed endpoint has delivered one stamped at or after the
        // exit, bounded by one interval plus the scrape timeout, and cut short
        // by ctrl-c. That
        // is what makes `-o out.dendro -- <short command>` record the
        // interval the command exited in, as the scrape path's final pass
        // does; a fixed grace shorter than the agent's interval recorded
        // nothing for a command that exited before the first frame.
        //
        // Every other stop (`--duration`, ctrl-c, a capped command) gets one
        // interval's grace, at most two seconds so a long interval cannot
        // hold a `docker stop` past its grace period: an unconditional sleep,
        // because a `recv` would return at once on anything already queued
        // and the frame still in flight would be dropped after all.
        //
        // Skipped when the recording already failed: it was reported when it
        // did, and there is nothing left to commit into; and when no endpoint
        // streams, since a scraped endpoint has nothing in flight.
        if endpoints.iter().any(|ep| ep.streaming) && archive.is_some() {
            let mut failed = None;
            if let Some((exit_wall, frames_at_exit)) = child_exit_wall.as_ref() {
                // By stamp, including an endpoint's first frame: a frame the
                // agent sent before the exit can still be queued when the
                // exit is seen, and counting it would end the wait one
                // interval early. An interval with no rows has no stamp and
                // counts once it arrives after the exit. The agent's clock is
                // compared with this host's, so skew between the two can only
                // lengthen the wait, up to its bound, or end it early by the
                // skew.
                let delivered = |ep: &EndpointState, before: u64| {
                    ep.frames > before
                        && ep
                            .last_frame_wall_ns
                            .is_none_or(|stamp| stamp >= *exit_wall)
                };
                let deadline = Instant::now() + interval_dur + scrape_timeout;
                loop {
                    let waiting: Vec<usize> = endpoints
                        .iter()
                        .enumerate()
                        .filter(|(i, ep)| ep.streaming && !delivered(ep, frames_at_exit[*i]))
                        .map(|(i, _)| i)
                        .collect();
                    if waiting.is_empty() {
                        break;
                    }
                    tokio::select! {
                        biased;
                        _ = shutdown.notified() => break,
                        event = stream_rx.recv() => {
                            let Some((idx, event)) = event else { break };
                            if let Some(e) = handle_stream_event(
                                idx,
                                event,
                                &mut endpoints,
                                archive.as_mut(),
                                wall_now_ns(),
                                hook,
                            )
                            .await
                            {
                                failed = Some(e);
                                break;
                            }
                        }
                        _ = tokio::time::sleep_until(deadline.into()) => {
                            for idx in waiting {
                                warn!(
                                    "{} ({}): no stream frame stamped after the command's exit \
                                     arrived within {}; the recording ends before the exit",
                                    endpoints[idx].config.source_label(),
                                    endpoints[idx].config.url,
                                    humantime::format_duration(interval_dur + scrape_timeout)
                                );
                            }
                            break;
                        }
                    }
                }
            } else {
                tokio::time::sleep(interval_dur.min(Duration::from_secs(2))).await;
            }
            let failed = match failed {
                Some(e) => Some(e),
                None => {
                    drain_stream_events(
                        &mut stream_rx,
                        &mut endpoints,
                        archive.as_mut(),
                        wall_now_ns(),
                        hook,
                    )
                    .await
                }
            };
            apply_restarts(
                &mut endpoints,
                archive.as_mut(),
                probe.as_ref(),
                &config.metadata,
                scrape_timeout,
                true,
            )
            .await;
            let failed =
                failed.or_else(|| archive.as_mut().and_then(|rec| rec.commit_tick().err()));
            if let Some(e) = failed {
                eprintln!("error: recording failed: {e}");
                recording_failed.store(true, Ordering::SeqCst);
                if let Some(rec) = archive.take() {
                    eprintln!("{}", rec.recovery_note());
                }
            }
        }

        let active_count = endpoints
            .iter()
            .filter(|ep| ep.first_success_ns.is_some())
            .count();

        if active_count == 0 {
            // Nothing was captured, so the recording holds no recoverable data:
            // stop the writer and remove it rather than leaving a stub behind.
            // Explicit because the `exit` below skips destructors.
            if let Some(rec) = archive.take() {
                rec.discard();
            }
            if wrapped {
                // Same class as a failed write: no output file exists, so
                // handing back the wrapped command's (possibly zero) status
                // would tell a supervisor the recording succeeded when the one
                // thing this process exists to produce was never produced.
                warn!("command exited before any metrics were recorded");
                recording_failed.store(true, Ordering::SeqCst);
                return outcome;
            }
            eprintln!("error: no data was recorded from any endpoint");
            std::process::exit(1);
        }

        // The segments are already on disk, so this only seals the (small)
        // open ones and marks each recording complete.
        //
        // `None` means the recording already failed mid-run and reported it
        // (what was on disk was left in place there, and its path printed);
        // nothing to add here.
        let mut finalized = false;
        if let Some(mut rec) = archive.take() {
            // `run_end` before finalize: `update_metadata` needs a live
            // writer, and finalize is what stops it. Not fatal for the same
            // reason the `run_start` merge is not.
            if let Some(event) = run_events
                .iter()
                .find(|e| e.kind.as_deref() == Some("run_end"))
            {
                if let Err(e) = rec.merge_events(std::slice::from_ref(event)) {
                    warn!("failed to add the run_end event to the recording: {e}");
                }
            }
            if let Err(e) = rec.finalize(last_clock) {
                // Must flip `recording_failed`: without it a failed tail seal
                // (ENOSPC at the end of a long capture) exits 0, and the
                // writer refuses an existing output, so the PREVIOUS run's
                // archive at that path would be sitting there for the next
                // command in the pipeline to analyze.
                eprintln!("error saving the archive: {e}");
                recording_failed.store(true, Ordering::SeqCst);
            } else {
                info!("wrote the archive to {}", config.output.display());
                finalized = true;
            }
        }
        // Endpoints refused after the archive opened, on first activation or
        // on a reconnect (a refusal at startup exits before the archive
        // exists). The recording went on without them; the exit status says
        // so.
        if let Some(summary) = refusal_summary(&endpoints, &config.output, finalized) {
            eprintln!("{summary}");
            recording_failed.store(true, Ordering::SeqCst);
        }
        outcome
    });

    // Every path out of the block above already finalized or discarded the
    // archive; this is the backstop for the ones below that skip destructors
    // (`std::process::exit`). Dropping joins the writer thread and leaves what
    // is on disk alone, so a missed path costs a recoverable archive, never a
    // detached thread appending to it after we exit.
    drop(archive);

    // Flush buffered logs before exiting the process: std::process::exit
    // skips destructors, so drop the log drain explicitly first.
    if recording_failed.load(Ordering::SeqCst) {
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

#[cfg(test)]
mod tests {
    use super::*;

    // Replaced the `inject_provenance` test. That function decoded every
    // scraped snapshot, wrote `source` and `endpoint` into every MetricDesc of
    // every group schema, and recomputed each `schema_hash` so the producer
    // contract held for the rewritten schema.
    //
    // It is gone because provenance describes a recording, not a metric, and
    // three things followed from that being the wrong level. Rewriting the
    // schema is the opposite of the row passthrough #1237 bought. Recomputing
    // `schema_hash` in the RECORDER breaks the content address a receiver
    // caches parsed schemas by — two recorders with different `--source`
    // labels would hash identical schemas differently. And it was N copies of
    // one fact, the same pattern the slot index exists to remove.
    //
    // Counted before removing it: per-metric `endpoint` had NO reader in the
    // tree, and per-column `source` had exactly one, in `parquet_tools::filter`.
    // What survives is the file-level `source` and `endpoint` keys a parquet
    // carries, and the label set a `.rez` manifest carries. See #1224.
    #[test]
    fn a_scraped_snapshot_reaches_the_recording_unmodified() {
        use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc, Snapshot, SnapshotV3};
        use std::collections::BTreeMap;
        use std::time::{Duration, SystemTime};

        let schema = GroupSchema {
            counters: vec![MetricDesc {
                name: "0x0".to_string(),
                metadata: [("metric".to_string(), "cpu_usage".to_string())]
                    .into_iter()
                    .collect::<BTreeMap<_, _>>(),
            }],
            gauges: Vec::new(),
            histograms: Vec::new(),
        };
        let group = GroupSnapshot {
            name: "cpu_usage/usage".to_string(),
            schema_hash: schema.hash(),
            schema: Some(std::sync::Arc::new(schema)),
            window: None,
            counters: vec![Some(1)],
            gauges: Vec::new(),
            histograms: Vec::new(),
        };
        let snapshot = Snapshot::V3(SnapshotV3 {
            systemtime: SystemTime::UNIX_EPOCH,
            duration: Duration::from_secs(1),
            metadata: Default::default(),
            groups: vec![group],
        });

        // The bytes a recorder writes are the bytes it received. Encoding both
        // sides rather than comparing the values proves the whole payload is
        // untouched, which is what makes a row passthrough a passthrough.
        let before = rmp_serde::encode::to_vec(&snapshot).unwrap();
        let decoded = metriken_exposition::Snapshot::from_msgpack(&before).unwrap();
        let after = rmp_serde::encode::to_vec(&decoded).unwrap();
        assert_eq!(before, after);

        let Snapshot::V3(v3) = decoded else {
            panic!("expected V3")
        };
        let desc = &v3.groups[0].schema.as_ref().unwrap().counters[0];
        for key in ["source", "endpoint"] {
            assert!(
                !desc.metadata.contains_key(key),
                "`{key}` describes the recording, not the metric, and must not be \
                 written into a column"
            );
        }
        assert_eq!(
            v3.groups[0].validate(),
            Ok(()),
            "the producer's schema_hash still matches its schema, because nothing \
             here rewrote either"
        );
    }

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

    // The stamps `.rez` rows carry come from the anchor plus monotonic elapsed,
    // never from the wall clock directly — a sealed segment is immutable, so a
    // decreasing stamp there would permanently feed `rate()` a dt <= 0.
    #[test]
    fn anchored_stamps_are_strictly_increasing_and_offset_records_the_wall_clock() {
        const ANCHOR: u64 = 1_700_000_000_000_000_000;
        // Ticks a second apart on the monotonic clock, with a wall clock that
        // runs 1 ms ahead of the anchored timeline.
        let ticks: Vec<(u64, i64)> = (0..5u64)
            .map(|i| {
                let elapsed = Duration::from_secs(i);
                let wall_ns = ANCHOR + elapsed.as_nanos() as u64 + 1_000_000;
                anchored_stamp(ANCHOR, elapsed, wall_ns)
            })
            .collect();

        for (i, w) in ticks.windows(2).enumerate() {
            assert!(
                w[1].0 > w[0].0,
                "tick {i} did not advance: {:?} -> {:?}",
                w[0],
                w[1]
            );
        }
        assert_eq!(ticks[0].0, ANCHOR, "the first stamp is the anchor itself");
        assert_eq!(ticks[4].0, ANCHOR + 4_000_000_000);
        for (ts, offset) in &ticks {
            assert_eq!(*offset, 1_000_000, "wall - anchored, at stamp {ts}");
        }
    }

    #[test]
    fn a_wall_clock_step_moves_the_offset_not_the_timeline() {
        const ANCHOR: u64 = 1_700_000_000_000_000_000;
        let before = anchored_stamp(ANCHOR, Duration::from_secs(10), ANCHOR + 10_000_000_000);
        // NTP steps the wall clock back 5 s between two ticks 1 s apart.
        let after = anchored_stamp(ANCHOR, Duration::from_secs(11), ANCHOR + 6_000_000_000);

        assert_eq!(before, (ANCHOR + 10_000_000_000, 0));
        assert!(
            after.0 > before.0,
            "the timeline is immune to the step: {before:?} -> {after:?}"
        );
        assert_eq!(after.0, ANCHOR + 11_000_000_000);
        // The step is not lost, it is data about the clock: -5 s at this tick.
        assert_eq!(after.1, -5_000_000_000);
    }

    // ── the archive ─────────────────────────────────────────────────────────
    //
    // These drive `start_recorder` and the `Archive` it returns rather than
    // `run()`, which owns `std::process::exit` and a tokio runtime. What they
    // cover is the wiring: that the result is readable end to end, and that a
    // discarded recording leaves nothing behind. What they do NOT cover is the
    // loop around it — the tick scheduling, the scrape timeouts and the exit
    // paths are exercised by `tests/record_lifecycle.rs` against the real
    // binary.

    // ── agent version capture (issue #1195) ─────────────────────────────────

    #[test]
    fn metadata_carries_the_agent_version() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(&dir.path().join("out.dendro"));
        let mut ep = test_endpoint();
        ep.identity
            .hook_metadata
            .insert("version".to_string(), "5.19.2".to_string());

        let m = build_metadata(&config, &ep, &[]);
        assert_eq!(
            m.get(parquet_metadata::KEY_VERSION).map(String::as_str),
            Some("5.19.2")
        );
    }

    /// `--endpoint url,role=...` is kept as the recording's `role`, and a
    /// `--metadata role=...` overrides it, as for `version`.
    #[test]
    fn metadata_carries_the_endpoint_role() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(&dir.path().join("out.dendro"));
        let mut ep = test_endpoint();
        assert!(!build_metadata(&config, &ep, &[]).contains_key("role"));

        ep.config.role = Some("loadgen".to_string());
        let m = build_metadata(&config, &ep, &[]);
        assert_eq!(m.get("role").map(String::as_str), Some("loadgen"));

        config.metadata = vec![("role".to_string(), "service".to_string())];
        let m = build_metadata(&config, &ep, &[]);
        assert_eq!(m.get("role").map(String::as_str), Some("service"));
    }

    // ── producer epoch (#1224) ──────────────────────────────────────────────

    #[test]
    fn metadata_carries_the_producer_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(&dir.path().join("out.dendro"));
        let mut ep = test_endpoint();
        ep.agent.producer_epoch = Some("11111111-2222-4333-8444-555555555555".to_string());

        let m = build_metadata(&config, &ep, &[]);
        assert_eq!(
            m.get(parquet_metadata::KEY_PRODUCER_EPOCH)
                .map(String::as_str),
            Some("11111111-2222-4333-8444-555555555555")
        );
    }

    /// Absent, not empty. An empty epoch would read as "this recording knows
    /// its counters did not restart", which is the opposite of not knowing.
    #[test]
    fn metadata_omits_the_epoch_when_the_agent_reported_none() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(&dir.path().join("out.dendro"));
        let ep = test_endpoint();
        let m = build_metadata(&config, &ep, &[]);
        assert!(!m.contains_key(parquet_metadata::KEY_PRODUCER_EPOCH));
    }

    // ── run events ──────────────────────────────────────────────────────────

    /// The seed carries the run events it is given under `KEY_EVENTS`, so a
    /// recording opened after the wrapped command spawned starts with
    /// `run_start` rather than waiting for an update that already happened.
    #[test]
    fn metadata_carries_the_run_events_when_there_are_any() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(&dir.path().join("out.dendro"));
        let ep = test_endpoint();
        let marker = RunMarker::new(&["/usr/local/bin/bench.sh".to_string()]);
        let start = marker.start_event(TEST_ANCHOR, None);

        let m = build_metadata(&config, &ep, std::slice::from_ref(&start));
        let payload: Events = serde_json::from_str(m.get(KEY_EVENTS).expect("events key")).unwrap();
        assert_eq!(payload.events, vec![start]);
        assert_eq!(payload.events[0].description, "bench.sh");
        assert_eq!(payload.events[0].kind.as_deref(), Some("run_start"));
        assert_eq!(
            payload.events[0].id.as_deref(),
            Some(format!("run:{}:start", marker.id).as_str())
        );
        assert!(payload.events[0].details.is_none());
    }

    /// No key at all without run events: an unwrapped run must not write
    /// `{"events":[]}`, which `annotate` treats as a payload to keep.
    #[test]
    fn metadata_omits_the_events_key_without_run_events() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(&dir.path().join("out.dendro"));
        let ep = test_endpoint();
        let m = build_metadata(&config, &ep, &[]);
        assert!(!m.contains_key(KEY_EVENTS));
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
        let ep = test_endpoint();
        let marker = RunMarker::new(&["bench".to_string()]);
        let start = marker.start_event(TEST_ANCHOR + TEST_SECOND, None);

        let user =
            r#"{"events":[{"timestamp":1700000000000000000,"description":"deploy","id":"d1"}]}"#;
        config.metadata = vec![(KEY_EVENTS.to_string(), user.to_string())];
        let m = build_metadata(&config, &ep, std::slice::from_ref(&start));
        let payload: Events = serde_json::from_str(m.get(KEY_EVENTS).unwrap()).unwrap();
        assert_eq!(payload.events.len(), 2);
        assert_eq!(payload.events[0].description, "deploy");
        assert_eq!(payload.events[1], start);

        config.metadata = vec![(KEY_EVENTS.to_string(), "not json".to_string())];
        let m = build_metadata(&config, &ep, std::slice::from_ref(&start));
        assert_eq!(m.get(KEY_EVENTS).map(String::as_str), Some("not json"));
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

    /// An endpoint whose `/status` carried no epoch adopts the first one a
    /// handshake shows. Nothing restarted; there was simply nothing to
    /// compare.
    #[test]
    fn a_first_epoch_is_adopted_rather_than_reported_as_a_restart() {
        let mut ep = test_endpoint();
        assert!(ep.agent.producer_epoch.is_none());
        note_epoch(&mut ep, "epoch-a");
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("epoch-a"));
        assert!(ep.restarts.is_empty());
    }

    /// The same epoch twice is the ordinary case and must not move anything.
    #[test]
    fn an_unchanged_epoch_leaves_the_recorded_one_alone() {
        let mut ep = test_endpoint();
        ep.agent.producer_epoch = Some("epoch-a".to_string());
        note_epoch(&mut ep, "epoch-a");
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("epoch-a"));
        assert!(ep.restarts.is_empty());
    }

    /// A change is a restart: every cumulative counter reset between these two
    /// handshakes. The recorder tracks the new one so it is recorded once.
    #[test]
    fn a_changed_epoch_is_tracked_so_the_restart_is_recorded_once() {
        let mut ep = test_endpoint();
        ep.agent.producer_epoch = Some("epoch-a".to_string());
        note_epoch(&mut ep, "epoch-b");
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("epoch-b"));
        // A second look at the same epoch is not another restart.
        note_epoch(&mut ep, "epoch-b");
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("epoch-b"));
        assert_eq!(ep.restarts.len(), 1);
    }

    /// Same precedence as `source`: what the agent reported is the default,
    /// and an explicit `--metadata version=...` overrides it (relabeling a
    /// capture taken through a proxy, say).
    #[test]
    fn an_explicit_metadata_version_overrides_the_agent_reported_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = test_config(&dir.path().join("out.dendro"));
        config.metadata = vec![("version".to_string(), "custom".to_string())];
        let mut ep = test_endpoint();
        ep.identity
            .hook_metadata
            .insert("version".to_string(), "5.19.2".to_string());

        let m = build_metadata(&config, &ep, &[]);
        assert_eq!(
            m.get(parquet_metadata::KEY_VERSION).map(String::as_str),
            Some("custom")
        );
    }

    #[test]
    fn metadata_omits_the_version_when_the_agent_did_not_report_one() {
        // A Prometheus endpoint, or an agent that answers neither `/status`
        // nor `/`. Absent, not empty: an empty string would render as the
        // blank `Rezolus Version:` line this change exists to remove.
        let dir = tempfile::tempdir().unwrap();
        let config = test_config(&dir.path().join("out.dendro"));
        let ep = test_endpoint();

        let m = build_metadata(&config, &ep, &[]);
        assert!(!m.contains_key(parquet_metadata::KEY_VERSION));
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

    fn test_endpoint() -> EndpointState {
        EndpointState::new(endpoint::EndpointConfig {
            url: Url::parse("http://localhost:4241").unwrap(),
            source: Some("rezolus".to_string()),
            role: None,
            protocol: Some(Protocol::Stream),
        })
    }

    /// A second endpoint, distinguishable from [`rez_endpoint`] by `source`.
    fn test_endpoint_b() -> EndpointState {
        EndpointState::new(endpoint::EndpointConfig {
            url: Url::parse("http://localhost:4242").unwrap(),
            source: Some("valkey".to_string()),
            role: None,
            protocol: Some(Protocol::Stream),
        })
    }

    /// Labels an operator reads as different, that the seal stagger cannot
    /// tell apart.
    ///
    /// Two keys differing only in bit 5 of an even number of bytes draw the
    /// same bucket for EVERY sampler, and in printable ASCII bit 5 is the case
    /// bit. Their recordings then seal in permanent lockstep — the failure the
    /// stagger exists to prevent — reached by two names that look distinct.
    /// Warned rather than hashed around, because every hash that closed the
    /// aliasing spread a real sampler set at or worse than random where this
    /// one spreads it perfectly (see `seal_policy::stagger_bucket`).
    #[test]
    fn labels_differing_only_in_case_are_warned_about() {
        let url = |s: &str| Url::parse(s).unwrap();
        let mut seen = BTreeMap::new();
        seen.insert("arm=valkey".to_string(), "http://a:4241/".to_string());

        // Even number of case flips: shares every bucket.
        let w = super::indistinguishable_warning(&seen, "arm=VALKEY", &url("http://b:4241"))
            .expect("an even-flip case difference must warn");
        assert!(w.contains("lockstep"), "{w}");
        assert!(w.contains("capitalisation"), "{w}");

        // Odd: does not alias, so must NOT warn — a warning on every
        // case-insensitive match would cry wolf on pairs that stagger fine.
        assert!(
            super::indistinguishable_warning(&seen, "arm=Valkey", &url("http://b:4241")).is_none(),
            "an odd-flip case difference does not alias and must not warn"
        );

        // Genuinely distinct labels: silent.
        assert!(
            super::indistinguishable_warning(&seen, "arm=redis", &url("http://b:4241")).is_none()
        );
    }

    /// The identical-labels warning still fires, and says the other thing:
    /// nothing downstream can tell the recordings apart at all. The two
    /// warnings are different problems and must not be collapsed.
    #[test]
    fn identical_labels_warn_about_identity_not_lockstep() {
        let mut seen = BTreeMap::new();
        seen.insert("arm=valkey".to_string(), "http://a:4241/".to_string());
        let w = super::indistinguishable_warning(
            &seen,
            "arm=valkey",
            &Url::parse("http://b:4241").unwrap(),
        )
        .expect("identical labels must warn");
        assert!(w.contains("identical labels"), "{w}");
        assert!(!w.contains("lockstep"), "{w}");
    }

    /// One tick of one counter for `endpoint`.
    fn tick(rec: &mut Archive, endpoint: usize, i: u64) -> Result<(), String> {
        let ts = TEST_ANCHOR + i * TEST_SECOND;
        let c = rez::recorder_tests_support::counter(
            "fake_ops",
            "fake",
            i,
            Some(::rez::window::Window::new(ts - 500, ts)),
        );
        let snapshot = rez::recorder_tests_support::snap(ts, vec![c]);
        let url = Url::parse("http://localhost:4241").unwrap();
        // A whole tick, as the recorder does one: stage every endpoint, then
        // commit once. Testing `stage` alone would leave the rows uncommitted
        // and every assertion about the file vacuous.
        rec.stage(endpoint, &url, &snapshot, ts, 0)?;
        rec.commit_tick()
    }

    #[test]
    fn a_scrape_with_no_recording_is_an_error_not_a_silent_drop() {
        // Nothing else catches an unrouted snapshot. Returning Ok here would
        // decode a scrape and throw it away every tick for the whole run, and
        // the archive would finalize one recording short with exit 0. That
        // was the behaviour when an endpoint down at startup activated later.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.dendro");
        let config = test_config(&path);
        let mut rec = start_recorder(&config, &[(0, &test_endpoint())], TEST_ANCHOR).unwrap();

        assert!(tick(&mut rec, 0, 0).is_ok(), "endpoint 0 has a recording");
        let err = tick(&mut rec, 1, 0).expect_err("endpoint 1 has none");
        assert!(
            err.contains("discarded"),
            "the error must say the samples would be lost, got: {err}"
        );
        assert!(
            err.contains("http://"),
            "and must name the endpoint, not its index, got: {err}"
        );
        rec.discard();
    }

    /// A window and the row it is stored against move together, and a raw
    /// wall reading would not.
    ///
    /// The archive stores a window as an offset from its row's `ts`, so what
    /// lands on disk is the SUBTRACTION of the two. Stated with a divergence
    /// injected, because that is the only way to see the difference: with the
    /// wall clock and the anchor agreeing — a machine that has not stepped
    /// since the recording began — the two spellings produce identical bytes,
    /// which is why the end-to-end test cannot tell them apart.
    #[test]
    fn a_window_and_its_row_are_read_off_one_clock() {
        // A recorder 60 s into a run whose wall clock has since moved 30 s
        // relative to its anchor.
        const ANCHOR: u64 = 1_700_000_000_000_000_000;
        const DIVERGENCE: i64 = 30_000_000_000;
        let elapsed = Duration::from_secs(60);
        let wall = ANCHOR + elapsed.as_nanos() as u64 + DIVERGENCE as u64;

        let (row_ts, wall_offset) = anchored_stamp(ANCHOR, elapsed, wall);
        assert_eq!(
            wall_offset, DIVERGENCE,
            "the divergence is recorded, not absorbed"
        );

        // The scrape's two readings, taken the way `scrape_one` takes them.
        let begin = anchored_at(ANCHOR, elapsed);
        let end = anchored_at(ANCHOR, elapsed + Duration::from_millis(2));
        assert_eq!(
            begin as i64 - row_ts as i64,
            0,
            "a request sent at the tick is at the tick"
        );
        assert_eq!(
            end as i64 - row_ts as i64,
            2_000_000,
            "and the round trip's width is the round trip"
        );

        // The same edge read off the wall clock instead: out by the whole
        // divergence, which is unbounded and grows for as long as the
        // recording runs. That was the defect.
        assert_eq!(wall as i64 - row_ts as i64, DIVERGENCE);
    }

    /// A rezolus recording is anchored on the AGENT's timeline, not the
    /// recorder's.
    ///
    /// Its rows carry the agent's timestamps, and `ts + wall_offset` is how a
    /// consumer turns one into a wall clock. Anchoring the recording on the
    /// recorder's clock while filling it with the agent's stamps would resolve
    /// to a wall time neither host observed — and the error is the skew
    /// between two machines, not a rounding.
    #[test]
    fn a_recording_is_anchored_on_the_source_that_fills_it() {
        const AGENT_ANCHOR: i64 = 1_600_000_000_000_000_000;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("anchored.dendro");
        let config = test_config(&path);

        let mut agent = test_endpoint();
        agent.agent.clock_anchor_wall_ns = Some(AGENT_ANCHOR);
        // A Prometheus target has no clock of its own to offer, so it keeps
        // the recorder's — one archive, two recordings, each coherent.
        let prom = test_endpoint_b();

        let mut rec = start_recorder(&config, &[(0, &agent), (1, &prom)], TEST_ANCHOR).unwrap();
        tick(&mut rec, 0, 0).expect("ingest a");
        tick(&mut rec, 1, 0).expect("ingest b");
        rec.finalize((TEST_ANCHOR, 0)).unwrap();

        use metriken_storage::Catalog;
        let sources = metriken_storage::DendroCatalog::open(&path)
            .expect("the archive opens")
            .sources()
            .expect("the catalog reads");
        let anchor_of = |source: &str| -> u64 {
            sources
                .iter()
                .find(|s| s.labels.get("source").map(String::as_str) == Some(source))
                .unwrap_or_else(|| panic!("a recording for {source}"))
                .clock_anchor_wall_ns
        };
        assert_eq!(
            anchor_of("rezolus"),
            AGENT_ANCHOR as u64,
            "the agent's recording takes the agent's anchor"
        );
        assert_eq!(
            anchor_of("valkey"),
            TEST_ANCHOR,
            "a source with no anchor of its own keeps the recorder's"
        );
    }

    #[test]
    fn an_endpoint_that_activates_late_still_gets_a_recording() {
        // Startup only exits when NO endpoint is reachable, so a run with one
        // agent up and one still starting commits to the archive with a single
        // recording and must be able to add the second one to the live
        // archive. Otherwise the second endpoint's scrapes go nowhere.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.dendro");
        let config = test_config(&path);
        let mut rec = start_recorder(&config, &[(0, &test_endpoint())], TEST_ANCHOR).unwrap();

        tick(&mut rec, 0, 0).expect("the endpoint up at startup records");
        // ... endpoint 1 comes up mid-run.
        rec.add_endpoint(1, &config, &test_endpoint_b(), TEST_ANCHOR, &[])
            .expect("a recording can join an open archive");
        for i in 1..3 {
            tick(&mut rec, 0, i).expect("ingest a");
            tick(&mut rec, 1, i).expect("ingest b");
        }
        rec.finalize((TEST_ANCHOR + 3 * TEST_SECOND, 0)).unwrap();

        // Both recordings come back out of the reader, tellable apart, and
        // both carry rows — a manifest entry with no data would pass a
        // count-only assertion while still having dropped the scrapes.
        let readers = crate::rez_reader::RezReader::open_recordings(
            &path,
            metriken_storage::BufferPool::new(64 * 1024 * 1024),
        )
        .expect("the archive opens");
        assert_eq!(
            readers.len(),
            2,
            "the late endpoint must be its own recording"
        );
        let mut sources: Vec<&str> = readers
            .iter()
            .filter_map(|(labels, _)| labels.get("source").map(String::as_str))
            .collect();
        sources.sort_unstable();
        assert_eq!(
            sources,
            vec!["rezolus", "valkey"],
            "each recording keeps its own source label"
        );
        for (labels, reader) in &readers {
            use metriken_query::MetricsSource;
            assert_eq!(
                reader.counter_names(),
                vec!["fake_ops".to_string()],
                "recording {labels:?} must hold the rows ingested for it"
            );
        }
    }

    /// Drive a recorder the way the loop does — ingest, `maybe_seal` every
    /// tick, then finalize — over `ticks` one-second samples of one counter.
    fn record_ticks(rec: &mut Archive, ticks: u64) {
        for i in 0..ticks {
            let ts = TEST_ANCHOR + i * TEST_SECOND;
            let c = rez::recorder_tests_support::counter(
                "fake_ops",
                "fake",
                i,
                Some(::rez::window::Window::new(ts - 500, ts)),
            );
            let snapshot = rez::recorder_tests_support::snap(ts, vec![c]);
            rec.stage(0, &test_endpoint().config.url, &snapshot, ts, 0)
                .expect("stage");
            rec.commit_tick().expect("commit");
            rec.maybe_seal().expect("seal");
        }
    }

    /// A streamed agent in `-o out.dendro`: a slotted group's rows become a long table
    /// whose occupants come from the streamed schemas' labels, as a scrape's
    /// do. Slot 0 changes hands at tick 4 (a new `__uid__` and `comm`, the
    /// counter restarting), slot 1 keeps one thread; the schema travels only
    /// when it changes, and there are no index frames, as a 6.0 agent sends
    /// none.
    #[test]
    fn a_streamed_dendro_recording_takes_occupants_from_the_schemas() {
        use crate::recorder::stream::StreamSubscriber;
        use dendro::replicate::Frame;
        use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc};

        const STREAM: &str = "fake/tasks";
        const PRODUCER_ANCHOR: i64 = 1_700_000_000_000_000_000;
        let labels = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        // (slot, comm, uid) holding each slot at tick `i`.
        let occupants = |i: u64| {
            let first = if i < 4 {
                ("nginx", "u-a")
            } else {
                ("redis", "u-b")
            };
            vec![(0u32, first.0, first.1), (1u32, "sshd", "u-c")]
        };
        let schema_at = |i: u64| GroupSchema {
            counters: occupants(i)
                .into_iter()
                .map(|(slot, comm, uid)| MetricDesc {
                    name: format!("0x{slot}"),
                    metadata: labels(&[
                        ("metric", "task_ops"),
                        ("id", &slot.to_string()),
                        ("comm", comm),
                        ("__uid__", uid),
                    ]),
                })
                .collect(),
            gauges: Vec::new(),
            histograms: Vec::new(),
        };
        let handshake = Frame::Handshake {
            source: 0,
            uuid: Some("epoch-1".to_string()),
            labels: labels(&[("source", "rezolus")]),
            metadata: BTreeMap::new(),
            clock_anchor_wall_ns: PRODUCER_ANCHOR,
            complete: false,
        };
        let mut sub = StreamSubscriber::new();
        sub.apply(vec![handshake]).unwrap();
        let source = sub.source().cloned().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("streamed.dendro");
        let config = test_config(&path);
        let mut ep = test_endpoint();
        adopt_source(&mut ep, &source);
        let mut rec = start_recorder(&config, &[(0, &ep)], TEST_ANCHOR).unwrap();

        let anchor = u64::try_from(source.clock_anchor_wall_ns).unwrap();
        let ticks = 8u64;
        let mut last_hash = None;
        for i in 0..ticks {
            let ts = anchor + i * TEST_SECOND;
            let schema = schema_at(i);
            let hash = schema.hash();
            let changed = last_hash != Some(hash);
            last_hash = Some(hash);
            let restarted = if i < 4 { i } else { i - 4 };
            let group = GroupSnapshot {
                name: STREAM.to_string(),
                schema_hash: hash,
                schema: Some(std::sync::Arc::new(schema.clone())),
                window: Some(metriken::Window::new(ts - 500, ts)),
                counters: vec![Some(restarted * 10), Some(i * 20)],
                gauges: Vec::new(),
                histograms: Vec::new(),
            };
            let payload = wal::encode_wal_group_row(&wal::wal_group_row(
                &group,
                changed.then(|| (&schema).into()),
            ))
            .unwrap();
            let frames = vec![Frame::Rows {
                source: 0,
                seq: i,
                index_state: dendro::replicate::NO_INDEX_STATE,
                rows: vec![dendro::archive::WalRow {
                    stream: STREAM.to_string(),
                    ts: ts as i64,
                    wall_offset: 3,
                    row: payload,
                }],
            }];
            let applied = sub.apply(frames).unwrap();
            rec.stage_stream(0, &ep.config.url, applied).unwrap();
            rec.commit_tick().unwrap();
            rec.maybe_seal().unwrap();
        }
        rec.finalize((anchor + (ticks - 1) * TEST_SECOND, 3))
            .unwrap();

        use metriken_storage::Catalog;
        let catalog = metriken_storage::DendroCatalog::open(&path).unwrap();
        let id = catalog.sources().unwrap()[0].id;
        let streams = catalog.tables(id).unwrap();
        assert!(
            streams.contains(&format!("{STREAM}/occupants")),
            "the slotted group is long: {streams:?}"
        );
        assert!(
            catalog.caller_row_streams(id).unwrap().is_empty(),
            "no identity index is written to a dendro archive"
        );
        drop(catalog);

        use metriken_query::MetricsSource;
        let reader = crate::rez_reader::RezReader::open_recordings(
            &path,
            metriken_storage::BufferPool::new(64 * 1024 * 1024),
        )
        .unwrap()
        .remove(0)
        .1;
        let (start, end) = reader.time_range().unwrap();
        let metriken_query::QueryResult::Matrix { result } = reader
            .query_range(
                "sum by (comm, __uid__) (rate(task_ops[2s]))",
                start,
                end + 1.0,
                1.0,
            )
            .unwrap()
        else {
            panic!("a matrix");
        };
        let mut seen: Vec<(String, String, f64)> = result
            .iter()
            .map(|s| {
                let v = s.values.last().map(|v| v.1).unwrap_or(f64::NAN);
                (
                    s.metric.get("comm").cloned().unwrap_or_default(),
                    s.metric.get("__uid__").cloned().unwrap_or_default(),
                    v,
                )
            })
            .collect();
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            seen.iter()
                .map(|(c, u, _)| (c.as_str(), u.as_str()))
                .collect::<Vec<_>>(),
            vec![("nginx", "u-a"), ("redis", "u-b"), ("sshd", "u-c")],
            "each thread is its own occupant, labelled from the schema: {seen:?}"
        );
        for (comm, _, rate) in &seen {
            let want = if comm == "sshd" { 20.0 } else { 10.0 };
            assert!((rate - want).abs() < 1e-6, "{comm}: {rate}");
        }
    }

    /// An interval with no recording to land in is an error, as a scrape
    /// with none is: nothing else would catch the rows.
    #[test]
    fn a_streamed_interval_with_no_recording_is_an_error_not_a_silent_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.dendro");
        let config = test_config(&path);
        let mut rec = start_recorder(&config, &[(0, &test_endpoint())], TEST_ANCHOR).unwrap();
        let url = Url::parse("http://localhost:4242").unwrap();
        let err = rec
            .stage_stream(1, &url, stream::Applied::default())
            .expect_err("endpoint 1 has no recording");
        assert!(
            err.contains("discarded") && err.contains("http://"),
            "{err}"
        );
        rec.discard();
    }

    /// The handshake is the authority on the timeline the rows arrive on, so
    /// it overrides what `/status` said a moment earlier — and on a reconnect
    /// a new uuid is a restarted agent, tracked as a restart.
    #[test]
    fn a_handshake_sets_the_anchor_and_epoch_and_a_new_uuid_is_a_restart() {
        let source = |uuid: Option<&str>, anchor: i64| stream::Source {
            uuid: uuid.map(String::from),
            labels: BTreeMap::new(),
            metadata: BTreeMap::new(),
            clock_anchor_wall_ns: anchor,
        };
        let mut ep = test_endpoint();
        ep.agent.clock_anchor_wall_ns = Some(1);
        ep.agent.producer_epoch = Some("from-status".to_string());

        adopt_source(&mut ep, &source(Some("from-handshake"), 42));
        assert_eq!(ep.agent.clock_anchor_wall_ns, Some(42));
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("from-handshake"));

        // A restart: the epoch moves with it.
        adopt_source(&mut ep, &source(Some("after-restart"), 43));
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("after-restart"));
        assert_eq!(ep.agent.clock_anchor_wall_ns, Some(43));

        // A handshake with nothing to say leaves what is known alone: zero is
        // not an anchor and an absent uuid is not a restart.
        adopt_source(&mut ep, &source(None, 0));
        assert_eq!(ep.agent.clock_anchor_wall_ns, Some(43));
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("after-restart"));
    }

    #[test]
    fn a_dendro_recording_round_trips_through_rezreader() {
        // `-o out.dendro` writes through metriken-storage's writer, and the
        // recording must come back out through the reader every consumer
        // uses, with each endpoint its own recording: one opened at startup,
        // one that activated mid-run.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.dendro");
        let config = test_config(&path);
        let mut rec = start_recorder(&config, &[(0, &test_endpoint())], TEST_ANCHOR).unwrap();
        assert!(path.exists(), "the archive is created at the output path");

        record_ticks(&mut rec, 2);
        rec.add_endpoint(1, &config, &test_endpoint_b(), TEST_ANCHOR, &[])
            .unwrap();
        for i in 2..5 {
            tick(&mut rec, 0, i).unwrap();
            tick(&mut rec, 1, i).unwrap();
            rec.maybe_seal().unwrap();
        }
        rec.finalize((TEST_ANCHOR + 5 * TEST_SECOND, 0)).unwrap();

        assert!(
            metriken_storage::DendroCatalog::is_archive(&path).unwrap(),
            "a .dendro output is a dendro archive"
        );
        let readers = crate::rez_reader::RezReader::open_recordings(
            &path,
            metriken_storage::BufferPool::new(64 * 1024 * 1024),
        )
        .unwrap();
        let mut sources: Vec<&str> = readers
            .iter()
            .filter_map(|(labels, _)| labels.get("source").map(String::as_str))
            .collect();
        sources.sort_unstable();
        assert_eq!(sources, vec!["rezolus", "valkey"]);
        for (labels, reader) in &readers {
            use metriken_query::MetricsSource;
            assert_eq!(labels.get("arm").map(String::as_str), Some("redis"));
            assert!(reader.complete(), "{labels:?} was finalized");
            let (start, end) = reader.time_range().unwrap();
            let r = reader.query_range("rate(fake_ops[5s])", start, end + 1.0, 1.0);
            let metriken_query::QueryResult::Matrix { result } = r.expect("the query must resolve")
            else {
                panic!("a range query over a counter is a matrix");
            };
            let points: Vec<f64> = result
                .iter()
                .flat_map(|s| s.values.iter().map(|(_, v)| *v))
                .collect();
            assert!(
                !points.is_empty(),
                "{labels:?}: the rows must come back out"
            );
            assert!(
                points.iter().all(|v| (*v - 1.0).abs() < 1e-6),
                "{labels:?}: a counter rising 1/s must read back as 1/s: {points:?}"
            );
        }
    }

    #[test]
    fn discarding_a_dendro_recording_leaves_no_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.dendro");
        let config = test_config(&path);
        let rec = start_recorder(&config, &[(0, &test_endpoint())], TEST_ANCHOR).unwrap();
        rec.discard();
        for suffix in ["", "-wal", "-shm"] {
            let p = dir.path().join(format!("out.dendro{suffix}"));
            assert!(!p.exists(), "discard left {} behind", p.display());
        }
        start_recorder(&config, &[(0, &test_endpoint())], TEST_ANCHOR)
            .unwrap_or_else(|e| panic!("the output path is still claimed: {e}"))
            .discard();
    }
}
