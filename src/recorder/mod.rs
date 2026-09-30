use super::*;

mod child;
mod config;
mod endpoint;
mod prometheus;
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
    parquet_ingest, rez, rez_sqlite, rez_v3_rewrite, rez_v3_writer, schema, seal_policy, wal, wire,
};

/// True when the recording should be written as an archive of recordings: a
/// `.rez`, or a dendro archive (`.dendro`).
///
/// Lives here rather than in the `rez` crate because `Format` is this binary's
/// CLI vocabulary — the archive format has no opinion about how a run chose it.
fn wants_rez(format: crate::Format) -> bool {
    config::is_archive(format)
}

use crate::parquet_metadata;
use crate::parquet_metadata::KEY_EVENTS;
use crate::viewer::{Event, Events};
pub use config::RecordingConfig;
use endpoint::{infer_source_name, AgentMetadata, EndpointState, EndpointStatus, Protocol};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub fn command() -> Command {
    Command::new("record")
        .about("On-demand recording of metrics to a file")
        .long_about(
            "Record one or more metrics endpoints at a fixed interval and write the samples to\n\
             a file.\n\n\
             The source is auto-detected: a Rezolus agent (msgpack) or a Prometheus-compatible\n\
             endpoint. A .dendro output (the default) records each Rezolus agent from its\n\
             replication stream and scrapes each Prometheus endpoint; every other format\n\
             scrapes both (see STREAMING AND SCRAPING).\n\n\
             WHAT TO RECORD (choose one): --url for a single endpoint (default\n\
             http://localhost:4241), --endpoint (repeatable) for several at once, or --config\n\
             for a TOML file. Exactly one: passing two of them is a parse error, not a\n\
             merge — and the deprecated positional URL counts as --url for that rule, as\n\
             positional OUTPUT counts as -o. Prefer the flags.\n\n\
             --url is shorthand for a single endpoint with no modifiers; reach for a single\n\
             --endpoint when you need source=, role= or protocol= on it. Several rezolus\n\
             endpoints are eligible for .rez too, and become one multi-recording archive\n\
             (see WHAT IT WRITES). --config conflicts only with --url/--endpoint: every other\n\
             flag still applies alongside it, and -o, --interval and --format override the\n\
             file\'s [recording] table. There is no duration key —\n\
             a bounded config-driven run passes --duration on the command line.\n\n\
             HOW LONG TO RECORD (choose one): --duration for a fixed window, nothing to run\n\
             until Ctrl-C, or `-- <command>` to record for exactly the lifetime of a wrapped\n\
             command (perf-record style) — it stops when the command exits.\n\n\
             A wrapped command keeps this terminal: its stdin/stdout/stderr pass straight\n\
             through, and rezolus exits with the command\'s own exit status, so\n\
             `rezolus record -o bench.rez -- ./bench.sh && analyze bench.rez` gates on the\n\
             benchmark exactly as it would without the wrapper. The one substitution is the\n\
             --duration cap: if it fires and the command is killed, rezolus exits 124.\n\n\
             A wrapped run is also marked in the recording: a `run_start` event when the\n\
             command spawns and a `run_end` event when it exits, so the viewer can draw the\n\
             run's edges and align two recordings on them. Only the program name is stored\n\
             unless --record-command-line is given. .rez, .dendro and parquet output carry\n\
             the events; raw output has no metadata to carry them in.\n\n\
             WHAT IT WRITES: the output path is -o/--output, and its extension picks the\n\
             format, so --format is rarely needed. With no -o at all the recording goes to\n\
             rezolus.<ext> for the format in play — by default rezolus.dendro. (A \"sampler\" below\n\
             is one metric collector — cpu_usage, scheduler, blockio — and each reads on its\n\
             own schedule rather than on one global clock.)\n\n    \
             .dendro   (default) A per-sampler archive: one table per acquisition group,\n    \
             \x20         each at its own cadence, carrying the window each read covered so\n    \
             \x20         PromQL rate() queries in `rezolus view` and `rezolus mcp` can\n    \
             \x20         report uncertainty bounds instead of a bare number. Groups whose\n    \
             \x20         members come and go (threads, cgroups, CPUs) are stored one row\n    \
             \x20         per member. Rezolus agents are streamed (5.21.0 or later) and\n    \
             \x20         Prometheus endpoints scraped; several of them become one archive\n    \
             \x20         holding a recording each, which is what `rezolus view` reads as an\n    \
             \x20         A/B or multi-host comparison. Prefer it.\n    \
             .rez      The same recordings in the archive format before 6.0, several\n    \
             \x20         times larger. Every tool still reads and writes it; choose it\n    \
             \x20         only for a consumer that has not moved to .dendro, or to record an\n    \
             \x20         agent older than 5.21.0 (which a .dendro refuses).\n    \
             .parquet  One columnar table on a single uniform clock. Use it for a uniform\n    \
             \x20         tabular export or other parquet tooling.\n    \
             \x20         (Multiple endpoints, including Prometheus, do NOT need\n    \
             \x20         parquet — .dendro holds them as separate recordings.)\n    \
             .raw      The msgpack snapshots as scraped, concatenated (a Prometheus source\n    \
             \x20         is converted to snapshots on the way in, so either source works).\n    \
             \x20         Cheapest thing the recorder can do: it appends and never rewrites.\n    \
             \x20         Turn it into parquet later with `rezolus recording convert` — which\n    \
             \x20         you can redo, re-stamping metadata, since the raw input is kept.\n\n\
             Any other extension (-o capture.dat, or no extension at all) is not an error and\n\
             not .dendro: it means parquet, unless --format says otherwise. A --format that\n\
             contradicts the extension (say --format parquet with -o out.rez) IS an error,\n\
             rather than a silent choice between them.\n\n\
             A Prometheus endpoint records into a .rez or .dendro like any other. One\n\
             scrape is one request and one response, so it becomes one acquisition group\n\
             per target, windowed by the real HTTP round trip. Neither the source nor the\n\
             endpoint count demotes the format any more; only --separate does, since one\n\
             archive cannot be one file per endpoint.\n\n\
             OVERWRITING: a .rez or .dendro output path must NOT already exist — the\n\
             recorder refuses rather than truncate, because the archive is committed as it\n\
             goes and has no staging file. A parquet or raw output IS overwritten. There is\n\
             no --force; remove the old file or pick a new path.\n\n\
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
             # Record a Prometheus endpoint to parquet, tagging the source in the metadata\n    \
             rezolus record --url http://host:9090/metrics -o out.parquet --metadata source=llm-perf\n\n    \
             # Record two agents into ONE archive holding a recording each (multi-host / A/B)\n    \
             rezolus record --endpoint http://web-01:4241 --endpoint http://web-02:4241 -o fleet.dendro\n\n    \
             # Same host, two agents: give each a source= so the recordings are tellable apart\n    \
             rezolus record --endpoint http://localhost:4241,source=redis --endpoint http://localhost:4242,source=valkey -o ab.dendro\n\n    \
             # Record several endpoints into ONE combined parquet file (uniform tabular export)\n    \
             rezolus record --endpoint http://localhost:4241 --endpoint http://svc:9090/metrics,source=svc -o run.parquet\n\n    \
             # ...or one file per endpoint: writes run_rezolus.parquet and run_svc.parquet\n    \
             rezolus record --separate --endpoint http://localhost:4241 --endpoint http://svc:9090/metrics,source=svc -o run.parquet\n\n    \
             # Capture raw msgpack now, convert later\n    \
             rezolus record -o run.raw --duration 1m && rezolus recording convert run.raw\n\n    \
             # Write the pre-6.0 .rez format\n    \
             rezolus record --url http://host:4241 --format rez --duration 5m\n\n    \
             # Take the endpoints and the output from a file\n    \
             rezolus record --config rec.toml\n    \
             #   [recording]\n    \
             #   output = \"run.parquet\"   # required; its extension picks the format\n    \
             #   interval = \"1s\"          # optional\n    \
             #   separate = false         # optional\n    \
             #   [[endpoints]]\n    \
             #   url = \"http://localhost:4241\"\n    \
             #   [[endpoints]]\n    \
             #   url = \"http://svc:9090/metrics\"\n    \
             #   source = \"svc\"           # optional, as are role = and protocol =\n\n\
             TAGGING: -m/--metadata k=v writes file-level metadata and applies to EVERY\n\
             format. -l/--label k=v applies to .rez and .dendro (it is dropped for parquet and raw):\n\
             it tags the recordings inside the archive, source and host are auto-populated,\n\
             and a two-recording archive drives the viewer\'s A/B comparison, which aliases the\n\
             arms off each recording\'s arm/host labels.\n\n\
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
             ABOUT .dendro AND .rez:\n\n\
             Archive recordings are written to disk as they run, so stopping costs the same\n\
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
             STREAMING AND SCRAPING:\n\n\
             A .dendro records each Rezolus agent from its replication stream\n\
             (/metrics/stream): the agent pushes one frame per --interval, carrying only\n\
             the acquisition groups it re-read since the last one, stamped when the agent\n\
             sampled rather than when the recorder asked. Prometheus endpoints cannot\n\
             stream, so they are scraped each tick, and one run can hold both kinds. There\n\
             is no scrape path for a Rezolus agent into a .dendro: an agent that cannot\n\
             serve the stream (older than 5.21.0, a V2 agent, or a handshake that does not\n\
             decode) is refused with its version. At startup that refuses the run before\n\
             anything is written; record such an agent with -o out.rez or -o out.parquet,\n\
             which scrape. An agent that comes up later and is refused, or is refused on a\n\
             reconnect, is left out of the recording from then on (a reconnect keeps the\n\
             rows it had), the other endpoints keep recording, and the run exits 1 once\n\
             the archive is finalized. An agent that is not reachable yet, or whose stream\n\
             fails with an error that can change (a 5xx, a handshake timeout), is retried\n\
             each tick. A stream that drops mid-run is reconnected after one interval (at\n\
             least a second), like a scrape that fails is retried, and one that goes\n\
             silent for the scrape timeout counts as dropped. A wrapped command that exits\n\
             on its own waits for each agent's frame covering the exit, at most one\n\
             interval plus the scrape timeout.",
        )
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
                .help("Record endpoints defined in a TOML file: a [recording] table (output, interval, format, separate) plus [[endpoints]] entries mirroring the --endpoint fields (url, source, role, protocol)")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(PathBuf))
                .conflicts_with_all(["URL"]),
        )
        .arg(
            clap::Arg::new("ENDPOINT")
                .long("endpoint")
                .help("Add an endpoint as url[,source=name][,role=label][,protocol=msgpack|prometheus]; role is a free-form tag (conventionally service or loadgen); repeat for several (e.g. http://host:9090/metrics,source=svc,role=service,protocol=prometheus)")
                .action(clap::ArgAction::Append)
                .conflicts_with_all(["URL", "CONFIG_FILE"]),
        )
        .arg(
            clap::Arg::new("SEPARATE")
                .long("separate")
                .help("Write one file per endpoint instead of combining; each is named <OUTPUT-stem>_<source>.<ext> alongside the output path. Without an explicit source=, a rezolus agent endpoint is named \"rezolus\" and a Prometheus one falls back to its host-port plus any distinguishing path (e.g. svc-9090, or svc-9090-federate — the conventional /metrics is left off). For parquet or raw output only: a .rez or .dendro archive already keeps each endpoint as its own recording, so --separate does not apply to it")
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
            clap::Arg::new("FORMAT")
                .long("format")
                .short('f')
                .help("Output format: dendro (per-sampler archive, the default), rez (the archive format before 6.0), parquet (one columnar table), or raw (concatenated msgpack snapshots). Usually unnecessary — the -o extension picks the format, and giving both a --format and a conflicting extension is an error. An archive takes any number of endpoints, rezolus or Prometheus, each as its own recording")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(Format)),
        )
        .arg(
            clap::Arg::new("METADATA")
                .long("metadata")
                .short('m')
                .help("Add a file-level metadata tag as key=value (e.g. source=llm-perf); repeat for multiple tags. Applies to every output format")
                .action(clap::ArgAction::Append),
        )
        .arg(
            clap::Arg::new("LABEL")
                .long("label")
                .short('l')
                .help("Tag the recording with a label as key=value (e.g. arm=redis, role=server); repeat for multiple. A value without `=` is ignored. `source` and `host` are auto-populated. Applies to EVERY recording in the run, so it cannot tell two endpoints apart — use --endpoint url,source=name for that. .rez and .dendro output only — dropped for parquet and raw, where --metadata is the equivalent")
                .action(clap::ArgAction::Append),
        )
        .arg(
            clap::Arg::new("URL_FLAG")
                .long("url")
                .help("Single metrics endpoint to record; auto-detects Rezolus agent vs Prometheus (default http://localhost:4241)")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(Url))
                .conflicts_with_all(["CONFIG_FILE", "ENDPOINT", "URL"]),
        )
        .arg(
            clap::Arg::new("OUTPUT_FLAG")
                .long("output")
                .short('o')
                .help("Path to the output file; its extension picks the format (.rez, .dendro, .parquet, .raw). Defaults to rezolus.<format>, i.e. rezolus.dendro")
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
                .help("Wrap a command: record only while it runs, then stop when it exits. Give it after `--`, e.g. rezolus record -o out.parquet -- ./bench.sh --iters 100. The run is marked in the recording by two events: run_start (kind run_start, description = the program name, e.g. bench.sh) when the command spawns, and run_end (kind run_end, description = the program name plus `exited <code>`, `capped` or `interrupted`) when its exit is observed. Only the program name is recorded unless --record-command-line is given. .rez, .dendro and parquet output carry the events; raw output has no metadata and cannot, and a run that captured no samples writes no file at all")
                .action(clap::ArgAction::Set)
                .index(3)
                .num_args(1..)
                .last(true)
                .allow_hyphen_values(true)
                .value_parser(value_parser!(String)),
        )
}

/// Probe a single endpoint to detect its protocol and resolve the scrape URL.
async fn probe_endpoint(
    client: &Client,
    config: &endpoint::EndpointConfig,
) -> Option<(Protocol, Url)> {
    // If protocol is explicitly set, validate connectivity on the expected path
    if let Some(ref proto) = config.protocol {
        let url = match proto {
            Protocol::Msgpack => {
                let mut u = config.url.clone();
                if u.path() == "/" {
                    u.set_path("/metrics/binary");
                }
                u
            }
            Protocol::Prometheus => {
                let mut u = config.url.clone();
                if u.path() == "/" {
                    u.set_path("/metrics");
                }
                u
            }
        };
        if let Ok(resp) = client.get(url.clone()).send().await {
            if resp.status().is_success() {
                return Some((proto.clone(), url));
            }
        }
        return None;
    }

    // Auto-detect: try Rezolus binary first, then Prometheus
    let candidates: Vec<(Url, bool)> = if config.url.path() == "/" {
        let mut rezolus_url = config.url.clone();
        rezolus_url.set_path("/metrics/binary");
        let mut prom_url = config.url.clone();
        prom_url.set_path("/metrics");
        vec![(rezolus_url, false), (prom_url, true)]
    } else {
        vec![(config.url.clone(), true)]
    };

    for (candidate_url, is_prom) in &candidates {
        if let Ok(response) = client.get(candidate_url.clone()).send().await {
            if !response.status().is_success() {
                continue;
            }
            if let Ok(body) = response.bytes().await {
                if *is_prom {
                    return Some((Protocol::Prometheus, candidate_url.clone()));
                }
                // `from_msgpack`, not a bare `from_slice`: a depth-capped,
                // trailing-byte-checked decode (see its doc) even for this
                // throwaway probe — an unauthenticated endpoint offering
                // hostile bytes at discovery time is exactly where an
                // unbounded decode is cheapest to abuse.
                if metriken_exposition::Snapshot::from_msgpack(&body).is_ok() {
                    return Some((Protocol::Msgpack, candidate_url.clone()));
                }
            }
        }
    }
    None
}

/// Fetch systeminfo, descriptions, sampler status and version from a Rezolus
/// agent.
async fn fetch_agent_metadata(client: &Client, base_url: &Url) -> AgentMetadata {
    let mut info_url = base_url.clone();
    info_url.set_path("/systeminfo");
    let systeminfo = match client.get(info_url).send().await {
        Ok(response) if response.status().is_success() => response.text().await.ok(),
        _ => None,
    };

    let mut desc_url = base_url.clone();
    desc_url.set_path("/metrics/descriptions");
    let descriptions = match client.get(desc_url).send().await {
        Ok(response) if response.status().is_success() => response.text().await.ok(),
        _ => None,
    };

    let mut samplers_url = base_url.clone();
    samplers_url.set_path("/samplers");
    let sampler_status = match client.get(samplers_url).send().await {
        Ok(response) if response.status().is_success() => response.text().await.ok(),
        _ => None,
    };

    let (version, producer_epoch, clock_anchor_wall_ns) =
        fetch_agent_identity(client, base_url).await;

    AgentMetadata {
        systeminfo,
        descriptions,
        sampler_status,
        version,
        producer_epoch,
        clock_anchor_wall_ns,
    }
}

/// The version of the agent being recorded — **not** this binary's.
///
/// The recorder and the agent are separate processes and routinely different
/// builds (one host upgrades, the fleet does not), so `CARGO_PKG_VERSION` here
/// would name the wrong thing precisely when the question is being asked.
///
/// `/status` is the structured answer and is tried first. It is also recent
/// (5.16.0), and the whole point of recording a version is to make old
/// captures attributable, so an agent that predates it falls back to the root
/// page — `"Rezolus <version> Agent"`, served by every agent this project has
/// ever shipped. A source that answers neither (a Prometheus exporter, an
/// agent behind a proxy that rewrites `/`) simply records no version, which is
/// how every recording before this change reads.
async fn fetch_agent_identity(
    client: &Client,
    base_url: &Url,
) -> (Option<String>, Option<String>, Option<i64>) {
    let mut status_url = base_url.clone();
    status_url.set_path("/status");
    if let Ok(response) = client.get(status_url).send().await {
        if response.status().is_success() {
            if let Ok(body) = response.text().await {
                if let Ok(status) =
                    serde_json::from_str::<crate::agent::sampler_status::AgentStatus>(&body)
                {
                    let epoch =
                        (!status.producer_epoch.is_empty()).then_some(status.producer_epoch);
                    // Zero is what an agent older than the anchor
                    // deserializes to, and no real anchor is zero: that is
                    // 1970, and the agent would have to have started then.
                    let anchor =
                        (status.clock_anchor_wall_ns != 0).then_some(status.clock_anchor_wall_ns);
                    if !status.version.is_empty() {
                        return (Some(status.version), epoch, anchor);
                    }
                    if epoch.is_some() {
                        return (None, epoch, anchor);
                    }
                }
            }
        }
    }

    let mut root_url = base_url.clone();
    root_url.set_path("/");
    let body = match client.get(root_url).send().await {
        Ok(response) if response.status().is_success() => match response.text().await {
            Ok(body) => body,
            Err(_) => return (None, None, None),
        },
        _ => return (None, None, None),
    };
    // No epoch from this path by construction: an agent old enough to lack
    // `/status` predates the epoch entirely, and inventing one here would
    // claim a restart boundary nobody observed.
    (parse_root_version(&body), None, None)
}

/// Pull the version out of the agent's root page, whose first line is
/// `Rezolus <version> Agent`. Returns `None` for anything else, so a proxy's
/// error page or another service on the port does not get recorded as a
/// version.
pub(crate) fn parse_root_version(body: &str) -> Option<String> {
    let first = body.lines().next()?;
    let rest = first.strip_prefix("Rezolus ")?;
    let version = rest.strip_suffix(" Agent")?;
    if version.is_empty() || version.contains(char::is_whitespace) {
        return None;
    }
    Some(version.to_string())
}

/// The producer epoch carried by a snapshot's metadata, if it has one.
///
/// Read from the snapshot rather than only from `/status` because this is the
/// channel that can catch a restart BETWEEN two scrapes: at that moment every
/// counter in the payload restarted from zero together, and no comparison of
/// the values can say so — a counter that reset and one that wrapped both just
/// went down.
fn snapshot_producer_epoch(snapshot: &metriken_exposition::Snapshot) -> Option<&str> {
    use metriken_exposition::Snapshot;
    let metadata = match snapshot {
        Snapshot::V1(s) => &s.metadata,
        Snapshot::V2(s) => &s.metadata,
        Snapshot::V3(s) => &s.metadata,
    };
    metadata
        .get(parquet_metadata::KEY_PRODUCER_EPOCH)
        .map(String::as_str)
        .filter(|e| !e.is_empty())
}

/// The producer's own stamp for this pass, if it sent one.
///
/// Returns `(ts, wall_offset)` on the AGENT's timeline: when it read the
/// values, and the wall clock's disagreement with that timeline at the read.
///
/// The recorder's alternative is its own tick, which names when it ASKED. The
/// two differ by the network round trip and by however long the agent had been
/// serving this pass from its TTL cache — a scrape inside that window gets
/// values read up to a TTL earlier, and one HTTP response looks the same
/// either way. Only the agent can tell the difference, so when it does, that
/// is what the recording keeps.
///
/// Absent for a Prometheus source, which has no such clock to offer, and for
/// an agent older than these keys. Both fall back to the recorder's stamp,
/// which is what every recording before this held.
fn snapshot_producer_stamp(snapshot: &metriken_exposition::Snapshot) -> Option<(u64, i64)> {
    use metriken_exposition::Snapshot;
    let metadata = match snapshot {
        Snapshot::V1(s) => &s.metadata,
        Snapshot::V2(s) => &s.metadata,
        Snapshot::V3(s) => &s.metadata,
    };
    let ts: i64 = metadata.get("ts")?.parse().ok()?;
    let wall_offset: i64 = metadata.get("wall_offset")?.parse().ok()?;
    // A negative stamp is not a timeline this recorder can write: `wal.ts` is
    // unsigned, and a producer that sent one is not one to guess for.
    Some((u64::try_from(ts).ok()?, wall_offset))
}

/// Warn once when the agent's epoch changes mid-recording.
///
/// The recording's metadata records the epoch observed when it opened, and
/// there is no plumbing yet to amend it in place — the streaming writer owns
/// the connection on its own thread. So the rows after a restart are stamped
/// with the epoch of the run before it, which is wrong in a way nothing
/// downstream can detect.
///
/// Saying so in the log is not a fix and is not pretending to be one. It is
/// the difference between an operator having a chance to notice and having
/// none. Persisting the history as dendro's `producer_epochs` is the fix.
fn note_epoch_change(ep: &mut EndpointState, snapshot: &metriken_exposition::Snapshot) {
    if let Some(seen) = snapshot_producer_epoch(snapshot) {
        note_epoch(ep, seen);
    }
}

/// The epoch an endpoint's source now carries, against the one on record.
///
/// The shared half of [`note_epoch_change`]: the scrape path reads the epoch
/// off a snapshot, the stream path off a handshake, and both must react the
/// same way to the same change.
fn note_epoch(ep: &mut EndpointState, seen: &str) {
    match ep.agent.producer_epoch.as_deref() {
        // First epoch observed on an endpoint whose `/status` did not carry
        // one: adopt it rather than warn. Nothing restarted.
        None => ep.agent.producer_epoch = Some(seen.to_string()),
        Some(known) if known == seen => {}
        Some(known) => {
            warn!(
                "{}: the agent restarted mid-recording (producer epoch {known} -> {seen}); \
                 every cumulative counter reset at this point, and rows from here on are \
                 stamped with the earlier epoch",
                ep.config.source_label()
            );
            ep.agent.producer_epoch = Some(seen.to_string());
        }
    }
}

/// Open a replication stream to `ep`, an endpoint already probed as a Rezolus
/// agent, and mark it active and streaming.
///
/// The stream path's `fetch_agent_metadata`. Two things differ from the
/// scrape path, and both follow from the handshake being the authority on the
/// rows that will arrive:
///
/// - The recording's anchor and epoch are the handshake's, overriding what
///   `/status` said a moment earlier. The rows are stamped on the handshake's
///   timeline; an agent that restarted between the two fetches would
///   otherwise have its rows anchored on a clock it no longer keeps.
/// - The agent is asked for the stream and judged on the answer — see
///   [`stream::ConnectError`] for the split between "not there" and "cannot".
///   The metadata is kept on `ep` whatever the answer, so a refusal can name
///   the agent's version.
///
/// `/status` is read BEFORE the stream is opened, so the handshake is the
/// later of the two observations. The other order installed the handshake's
/// epoch over a newer `/status` reading, and an agent that restarted between
/// the two got a restart warning pointing backwards.
///
/// `timeout` bounds the connect and the handshake together, for the reason
/// the scrape path bounds a probe: this runs on the tick loop. The metadata
/// fetch is not bounded, exactly as the scrape path's is not.
async fn open_stream(
    client: &Client,
    ep: &mut EndpointState,
    interval: Duration,
    timeout: Duration,
) -> Result<stream::Subscription, stream::ConnectError> {
    ep.agent = fetch_agent_metadata(client, &ep.config.url).await;
    let sub = match tokio::time::timeout(
        timeout,
        stream::Subscription::connect(client, &ep.config.url, interval),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Err(stream::ConnectError::Unreachable(format!(
                "{} did not complete a handshake within {}",
                ep.config.url,
                humantime::format_duration(timeout)
            )))
        }
    };
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

    if ep.config.source.is_none() {
        ep.config.source = Some("rezolus".to_string());
    }
    adopt_source(ep, &source);
    ep.scrape_url = Some(ep.config.url.clone());
    ep.detected_protocol = Some(Protocol::Msgpack);
    ep.status = EndpointStatus::Active;
    ep.streaming = true;
    Ok(sub)
}

/// Where an agent was refused, which decides what the refusal advises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefusedAt {
    /// Before the archive exists: the whole run is refused.
    Startup,
    /// After the archive opened, on first activation: only this endpoint is
    /// left out.
    MidRun,
    /// On a reconnect, after the stream had been open: this endpoint's
    /// recording stops with the rows it had.
    Reconnect,
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

/// The refusal for a Rezolus agent that cannot serve its replication stream
/// into a `.dendro`.
///
/// Names the agent's version when `/status` or the `/` banner gave one. An
/// agent older than [`STREAM_SINCE`] is said to predate the stream; one at or
/// after it should serve the stream, so the refusal only quotes what the
/// route answered (a proxy that does not route `/metrics/stream` gives a 404
/// from a current agent). There is no scrape fallback: a `.dendro` records
/// agents by stream only, and a run that quietly scraped one agent would put
/// two endpoints of one A/B on different transports.
fn unstreamable_agent(ep: &EndpointState, reason: &str, at: RefusedAt) -> String {
    let url = &ep.config.url;
    let why = match ep.agent.version.as_deref() {
        Some(version) => match predates_stream(version) {
            Some(true) => format!(
                "{url} is Rezolus {version}, which predates the replication stream \
                 (agents serve /metrics/stream from {STREAM_SINCE}): {reason}"
            ),
            _ => format!(
                "{url} is Rezolus {version}, which should serve a replication stream, \
                 but it did not open: {reason}"
            ),
        },
        None => format!(
            "{url} is a Rezolus agent of unknown version, and its replication stream \
             did not open: {reason}. Agents serve /metrics/stream from {STREAM_SINCE}"
        ),
    };
    let next = match at {
        RefusedAt::Startup => {
            "A .dendro records Rezolus agents from their replication stream only; \
             record this agent to a .rez (-o out.rez) or to parquet (-o out.parquet) \
             instead, both of which scrape"
        }
        RefusedAt::MidRun => {
            "This endpoint is excluded from this recording and not retried; the other \
             endpoints keep recording. A .dendro records Rezolus agents from their \
             replication stream only; a separate `rezolus record` to a .rez or to \
             parquet, both of which scrape, can record this agent"
        }
        RefusedAt::Reconnect => {
            "This endpoint's recording stops here, keeping the rows it had, and is not \
             retried; the other endpoints keep recording. A .dendro records Rezolus \
             agents from their replication stream only; a separate `rezolus record` to a \
             .rez or to parquet, both of which scrape, can record this agent"
        }
    };
    format!("{why}. {next}")
}

/// The first Rezolus release whose agent serves `/metrics/stream`.
const STREAM_SINCE: &str = "5.21.0";

/// Whether `version` (as `/status` or the `/` banner reports it) is older
/// than [`STREAM_SINCE`]. `None` when it does not parse as
/// `major.minor.patch`; a pre-release or build suffix is ignored, so
/// `5.21.0-alpha.1` counts as 5.21.0.
fn predates_stream(version: &str) -> Option<bool> {
    let parse = |v: &str| -> Option<(u64, u64, u64)> {
        let core = v.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        let triple = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(triple)
    };
    Some(parse(version)? < parse(STREAM_SINCE)?)
}

/// How an endpoint came up, from [`activate_endpoint`].
enum Activation {
    /// Not up yet; probed again next tick. Carries the line that says why,
    /// for the caller to log at the level it wants.
    Pending(String),
    /// Active and scraped each tick: a Prometheus endpoint, or any endpoint of
    /// a run that does not write a `.dendro`.
    Scrape,
    /// A Rezolus agent in a `.dendro` run, subscribed to its stream.
    Stream(Box<stream::Subscription>),
}

/// Probe `ep` and bring it up on the transport its kind takes.
///
/// Shared by startup and by the late activation of an endpoint that was not
/// reachable at startup, so both apply one rule: with `stream_agents` (a
/// `.dendro` run) a Rezolus agent is streamed and a Prometheus endpoint is
/// scraped; without it everything is scraped. The probe classifies the
/// endpoint as it always has (`/metrics/binary` first, then `/metrics`, or
/// the declared `protocol=`).
///
/// `Err` is an agent that answered and cannot serve the stream: refused, never
/// scraped instead (see [`unstreamable_agent`]), with `at` choosing the
/// advice. The probe and the stream connect are each bounded by `timeout`,
/// because this runs on the tick loop.
async fn activate_endpoint(
    client: &Client,
    ep: &mut EndpointState,
    stream_agents: bool,
    interval: Duration,
    timeout: Duration,
    at: RefusedAt,
) -> Result<Activation, String> {
    let probed = match tokio::time::timeout(timeout, probe_endpoint(client, &ep.config)).await {
        Ok(probed) => probed,
        Err(_) => {
            warn!(
                "probe of {} timed out after {}",
                ep.config.url,
                humantime::format_duration(timeout)
            );
            None
        }
    };
    let Some((protocol, url)) = probed else {
        return Ok(Activation::Pending(format!(
            "endpoint {} not reachable, will retry each tick",
            ep.config.url
        )));
    };

    if stream_agents && protocol == Protocol::Msgpack {
        return match open_stream(client, ep, interval, timeout).await {
            Ok(sub) => Ok(Activation::Stream(Box::new(sub))),
            Err(stream::ConnectError::Unsupported(e)) => Err(unstreamable_agent(ep, &e, at)),
            // It answered the probe, so it is there; the stream route failed
            // with an answer that may change (a 5xx from a proxy, a handshake
            // that timed out). Retried like any unreachable endpoint rather
            // than scraped.
            Err(stream::ConnectError::Unreachable(e)) => Ok(Activation::Pending(format!(
                "endpoint {} answered, but its replication stream did not open ({e}); \
                 will retry each tick",
                ep.config.url
            ))),
        };
    }

    if ep.config.source.is_none() {
        if protocol == Protocol::Msgpack {
            ep.config.source = Some("rezolus".to_string());
        } else {
            let inferred = infer_source_name(&ep.config.url);
            eprintln!(
                "warn: no source name specified for {}, using \"{inferred}\" \
                 (pass --metadata source=NAME to override)",
                ep.config.url,
            );
            ep.config.source = Some(inferred);
        }
    }
    if protocol == Protocol::Msgpack {
        ep.agent = fetch_agent_metadata(client, &ep.config.url).await;
    }
    ep.scrape_url = Some(url);
    ep.detected_protocol = Some(protocol);
    ep.status = EndpointStatus::Active;
    Ok(Activation::Scrape)
}

/// Take the handshake's word on the endpoint's timeline and epoch.
///
/// Also the reconnect path: a new connection's handshake names the source
/// again, and a different uuid is a restarted agent, which `note_epoch`
/// warns about exactly as the scrape path does on a changed snapshot epoch.
fn adopt_source(ep: &mut EndpointState, source: &stream::Source) {
    // Zero is what no agent anchors at (1970), so it is not an anchor.
    if source.clock_anchor_wall_ns != 0 {
        ep.agent.clock_anchor_wall_ns = Some(source.clock_anchor_wall_ns);
    }
    if let Some(uuid) = source.uuid.as_deref().filter(|u| !u.is_empty()) {
        note_epoch(ep, uuid);
    }
}

/// Stage everything the stream pumps have delivered so far.
///
/// Returns the first failure that ends the recording: an interval that could
/// not be staged, or an agent that came back unable to serve the stream.
/// Never waits — the pumps deliver for as long as they run, and this is
/// called from the tick and once more on the way out.
fn drain_stream_events(
    rx: &mut tokio::sync::mpsc::Receiver<(usize, stream::StreamEvent)>,
    endpoints: &mut [EndpointState],
    mut rez_recorder: Option<&mut RezStream>,
    wall_ns: u64,
) -> Option<String> {
    let mut failed: Option<String> = None;
    while let Ok((idx, event)) = rx.try_recv() {
        if let Some(e) =
            handle_stream_event(idx, event, endpoints, rez_recorder.as_deref_mut(), wall_ns)
        {
            failed.get_or_insert(e);
        }
    }
    failed
}

/// Act on one event from a pump. `Some` is a failure that ends the recording:
/// an interval that could not be staged, or an agent that came back unable to
/// serve the stream.
fn handle_stream_event(
    idx: usize,
    event: stream::StreamEvent,
    endpoints: &mut [EndpointState],
    rez_recorder: Option<&mut RezStream>,
    wall_ns: u64,
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
            if let Some(rec) = rez_recorder {
                if let Err(e) = rec.stage_stream(idx, &endpoints[idx].config.url, applied) {
                    failed.get_or_insert(e);
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
        stream::StreamEvent::Refused(e) => {
            // The agent came back unable to serve the stream. Refused as a
            // mid-run first activation is: this endpoint only, never
            // scraped instead, never retried (its pump has stopped). Its
            // recording keeps what it had and is finalized with the rest.
            let refusal = unstreamable_agent(&endpoints[idx], &e, RefusedAt::Reconnect);
            refuse_mid_run(&mut endpoints[idx], &refusal);
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

fn separate_output_path(base: &Path, source: &str) -> PathBuf {
    let stem = base.file_stem().unwrap_or_default().to_string_lossy();
    let ext = base.extension().unwrap_or_default().to_string_lossy();
    let filename = if ext.is_empty() {
        format!("{stem}_{source}")
    } else {
        format!("{stem}_{source}.{ext}")
    };
    base.with_file_name(filename)
}

fn output_dir(output: &Path) -> PathBuf {
    match output.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn build_parquet_converter(
    config: &RecordingConfig,
    ep: &EndpointState,
    prom_converter: &Option<prometheus::PrometheusConverter>,
    run_events: &[Event],
) -> MsgpackToParquet {
    let mut converter = MsgpackToParquet::with_options(
        ParquetOptions::new().max_batch_size(parquet_metadata::MAX_ROW_GROUP_SIZE),
    )
    .metadata(
        "sampling_interval_ms".to_string(),
        config.interval.as_millis().to_string(),
    );

    converter = converter.metadata("source".to_string(), ep.config.source_label().to_string());
    // Where the recording was scraped from. It used to ride in every metric's
    // metadata, where nothing read it; a flat parquet has no manifest, so file
    // level is the only place provenance can live for this format.
    converter = converter.metadata("endpoint".to_string(), ep.config.url.to_string());

    // Before the user's `--metadata`, the same as `source` above: what the
    // agent reported is the default, and an explicit `--metadata version=...`
    // overrides it.
    if let Some(ref version) = ep.agent.version {
        converter = converter.metadata(parquet_metadata::KEY_VERSION.to_string(), version.clone());
    }

    if let Some(ref epoch) = ep.agent.producer_epoch {
        converter = converter.metadata(
            parquet_metadata::KEY_PRODUCER_EPOCH.to_string(),
            epoch.clone(),
        );
    }

    for (key, value) in &config.metadata {
        // A user `--metadata events=...` is merged with the run events below
        // rather than written here, where the run events would replace it.
        if key == KEY_EVENTS {
            continue;
        }
        converter = converter.metadata(key.clone(), value.clone());
    }

    if let Some(ref json) = ep.agent.systeminfo {
        converter = converter.metadata("systeminfo".to_string(), json.clone());
    }

    // Descriptions: prefer agent-fetched, fall back to Prometheus HELP
    let prom_desc = prom_converter
        .as_ref()
        .filter(|c| !c.descriptions().is_empty())
        .and_then(|c| serde_json::to_string(c.descriptions()).ok());
    let desc = ep.agent.descriptions.clone().or(prom_desc);
    if let Some(ref json) = desc {
        converter = converter.metadata("descriptions".to_string(), json.clone());
    }

    // A user-supplied --metadata source=... takes precedence over the
    // endpoint's source. If the value parses as a JSON array the stream
    // represents multiple logical sources and we emit one
    // per_source_metadata entry per name.
    let effective_source = config
        .metadata
        .iter()
        .find(|(k, _)| k == "source")
        .map(|(_, v)| v.as_str())
        .unwrap_or(ep.config.source_label());

    if let Some(json) = build_per_source_metadata(
        effective_source,
        ep.first_success_ns,
        ep.last_success_ns,
        ep.config.role.as_deref(),
        ep.agent.sampler_status.as_deref(),
    ) {
        converter = converter.metadata("per_source_metadata".to_string(), json);
    }

    // The wrapped run's `run_start`/`run_end` events, under the key `annotate`
    // uses, merged with any `--metadata events=...` the user gave, the same
    // rule `build_rez_metadata` applies. Every endpoint's file gets the same
    // two: they are global (no source/node/instance), and a multi-endpoint
    // parquet run goes through `combine_files`, which concatenates the key
    // and dedups by event id.
    let user_events = config
        .metadata
        .iter()
        .rev()
        .find(|(k, _)| k == KEY_EVENTS)
        .map(|(_, v)| v.as_str());
    if let Some(json) = events_payload(user_events, run_events) {
        converter = converter.metadata(KEY_EVENTS.to_string(), json);
    }

    converter
}

/// The `KEY_EVENTS` payload for a parquet footer: `run_events` merged into
/// the user's own `--metadata events=...` value, or `None` when there is
/// neither. A user value that does not parse is kept verbatim, with a
/// warning, and the run events are the ones dropped; an empty result drops
/// the key rather than storing `{"events":[]}`, as `annotate` does.
fn events_payload(user_events: Option<&str>, run_events: &[Event]) -> Option<String> {
    let mut m = BTreeMap::new();
    if let Some(raw) = user_events {
        m.insert(KEY_EVENTS.to_string(), raw.to_string());
    }
    if let Err(e) = merge_events_into(&mut m, run_events) {
        warn!("not adding the run events to the parquet footer: {e}");
    }
    m.remove(KEY_EVENTS)
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

/// File-level metadata for a `.rez` archive manifest, mirroring the keys
/// `build_parquet_converter` writes (`sampling_interval_ms`, `source`,
/// `version`, user `--metadata`, `systeminfo`, `descriptions`).
///
/// `run_events` are the wrapped run's events known so far, so a recording
/// opened after the command spawned (a late-joining endpoint) carries
/// `run_start` from its seed rather than waiting for an update that already
/// happened.
fn build_rez_metadata(
    config: &RecordingConfig,
    ep: &EndpointState,
    run_events: &[Event],
) -> std::collections::BTreeMap<String, String> {
    let mut m = std::collections::BTreeMap::new();
    m.insert(
        "sampling_interval_ms".to_string(),
        config.interval.as_millis().to_string(),
    );
    m.insert("source".to_string(), ep.config.source_label().to_string());
    // Written whether or not the agent answered anything else: a recording
    // with no systeminfo and no descriptions is still worth attributing to a
    // build. Before the user's `--metadata` for the same reason `source` is —
    // an explicit `--metadata version=...` overrides what the agent reported.
    if let Some(ref version) = ep.agent.version {
        m.insert(parquet_metadata::KEY_VERSION.to_string(), version.clone());
    }
    if let Some(ref epoch) = ep.agent.producer_epoch {
        m.insert(
            parquet_metadata::KEY_PRODUCER_EPOCH.to_string(),
            epoch.clone(),
        );
    }
    for (k, v) in &config.metadata {
        m.insert(k.clone(), v.clone());
    }
    if let Some(ref json) = ep.agent.systeminfo {
        m.insert("systeminfo".to_string(), json.clone());
    }
    if let Some(ref json) = ep.agent.descriptions {
        m.insert("descriptions".to_string(), json.clone());
    }
    // Cannot fail on a map with no `events` key yet, and a user `--metadata
    // events=...` that does not parse is the user's to fix: their value is
    // kept and the run events are the ones dropped, with a warning.
    if let Err(e) = merge_events_into(&mut m, run_events) {
        warn!("not adding the run events to the recording's metadata: {e}");
    }
    m
}

/// The recording's label set for a `.rez` manifest: `source`, `host` (from the
/// agent's systeminfo hostname), plus any user `--label k=v` (last-wins). Thin
/// adapter over `rez::build_labels`; the merge logic + tests live in `rez`.
fn build_rez_labels(
    config: &RecordingConfig,
    ep: &EndpointState,
) -> std::collections::BTreeMap<String, String> {
    rez::build_labels(
        ep.config.source_label(),
        ep.agent.systeminfo.as_deref(),
        &config.labels,
    )
}

/// The recorder the loop feeds in archive mode (`.rez` or `.dendro`).
///
/// A newtype rather than a bare writer because the recording loop needs two
/// things the writers have no opinion about: what to tell the user after a
/// mid-recording failure, and how to leave nothing behind when a run captured
/// no samples at all.
struct RezStream {
    /// The container being written, and its per-recording state.
    sink: Sink,
    /// Each recording's metadata map as last written, keyed by endpoint
    /// index like the recorders.
    ///
    /// Kept because the `.rez` writer's `update_metadata` replaces the whole
    /// map: adding a run event means sending the seed back with the event
    /// merged in, and the writer does not hand the seed back.
    metadata: BTreeMap<usize, BTreeMap<String, String>>,
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
    /// Held here rather than locally in `start_rez_recorder` because an
    /// endpoint that was down at startup opens its recording later, and it
    /// has to be checked against the recordings already open.
    seen_labels: BTreeMap<String, String>,
}

/// The two archive writers. Each keeps one recorder per recording, keyed by
/// the endpoint index it serves: sparse rather than a `Vec` parallel to
/// `endpoints`, because only the endpoints being archived get one.
///
/// In both, the writer is **declared after the recorders deliberately.**
/// Fields drop in declaration order, and dropping the writer joins its
/// thread. Dropping it first would not block (`join` sends a shutdown before
/// releasing its own sender, and the writer honours it whoever still holds a
/// clone), but it would stop the writer while the recordings could still
/// queue their final seals, silently losing them.
enum Sink {
    /// The `.rez` v3 container.
    Rez {
        recs: BTreeMap<usize, rez_v3_writer::StreamRecorderV3>,
        /// This tick's rows, per recording, waiting for one commit.
        ///
        /// Cleared by `commit_tick`. Rows sitting here have already advanced
        /// each recorder's dedup and seal accounting — the same as the moment
        /// between `StreamRecorderV3::ingest` building its rows and its send
        /// returning, so a failure to commit loses the tick exactly as a
        /// failed send always did.
        staged: Vec<rez_sqlite::TickBatch>,
        archive: rez_v3_writer::RezArchive,
    },
    /// A dendro archive through metriken-archive's writer: groups with slots
    /// are written long, with their occupants in a stream beside each.
    Dendro {
        recs: BTreeMap<usize, metriken_archive::SourceRecorder>,
        /// This tick's rows, as for `Rez`.
        staged: Vec<metriken_archive::writer::Staged>,
        writer: metriken_archive::ArchiveWriter,
        /// The archive's path; the writer does not report it.
        path: std::path::PathBuf,
        /// Streamed endpoints only: per endpoint, the schema each stream's rows
        /// align with, to rebuild the snapshots the writer ingests.
        schemas: BTreeMap<usize, stream::StreamSchemas>,
    },
}

/// A metriken-archive error as the recorder reports errors.
fn archive_err(e: Box<dyn std::error::Error + Send + Sync>) -> String {
    e.to_string()
}

impl RezStream {
    /// Append one scraped snapshot.
    ///
    /// Fallible because the tick is written to the WAL here, rather than
    /// only appended to an in-memory builder that could not fail until a seal.
    /// Route one endpoint's snapshot to that endpoint's recording.
    ///
    /// A snapshot with no recording to land in is an ERROR, not a skip. In
    /// `.rez` mode there is no parquet writer to catch it — `writers` is all
    /// `None` — so returning `Ok` here would decode a scrape, inject its
    /// provenance and then drop it, every tick, for the whole run, and the
    /// archive would finalize successfully one recording short. Every endpoint
    /// that scrapes in this mode opens its recording first, at startup or on
    /// activation, so reaching this arm means that invariant broke.
    /// Stage one endpoint's tick. Nothing reaches the archive until
    /// [`commit_tick`](Self::commit_tick).
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
        self.last_stamp
            .insert(endpoint, (anchored_ts, wall_offset_ns));
        let missing = || {
            format!(
                "{url} was scraped with no recording open for it; its samples would be \
                 discarded"
            )
        };
        match &mut self.sink {
            Sink::Rez { recs, staged, .. } => {
                let rec = recs.get_mut(&endpoint).ok_or_else(missing)?;
                let rows = rec.stage(snapshot, anchored_ts, wall_offset_ns)?;
                // Keyed by recording id, which is what the writer commits
                // against.
                staged.push(rez_sqlite::TickBatch {
                    recording_id: rec.recording_id(),
                    rows,
                });
            }
            Sink::Dendro { recs, staged, .. } => {
                let rec = recs.get_mut(&endpoint).ok_or_else(missing)?;
                staged.push(
                    rec.stage(snapshot, anchored_ts, wall_offset_ns)
                        .map_err(archive_err)?,
                );
            }
        }
        Ok(())
    }

    /// Stage one interval off an endpoint's replication stream.
    ///
    /// [`stage`](Self::stage) for the stream path, which writes `.dendro`
    /// only: each pass is rebuilt into the V3 snapshot the writer ingests
    /// (see `stream::StreamSchemas`).
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
        let Sink::Dendro {
            recs,
            staged,
            schemas,
            ..
        } = &mut self.sink
        else {
            // Only a `.dendro` run streams; see `RecordingConfig::streams_agents`.
            return Err(format!(
                "{url} streamed an interval into a .rez archive, which records agents by \
                 scraping them"
            ));
        };
        let Some(rec) = recs.get_mut(&endpoint) else {
            return Err(format!(
                "{url} streamed an interval with no recording open for it; its rows \
                 would be discarded"
            ));
        };
        let cache = schemas.entry(endpoint).or_default();
        let before = cache.unresolved;
        for pass in &passes {
            let ts = u64::try_from(pass.ts)
                .map_err(|_| format!("{url} stamped a pass at {} ns, before the epoch", pass.ts))?;
            self.last_stamp.insert(endpoint, (ts, pass.wall_offset));
            let snapshot = cache.snapshot(pass)?;
            staged.push(
                rec.stage(&snapshot, ts, pass.wall_offset)
                    .map_err(archive_err)?,
            );
        }
        if cache.unresolved > before {
            warn!(
                "{url}: {} streamed rows named a schema this connection had not sent; \
                 skipped",
                cache.unresolved - before
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
        match &mut self.sink {
            Sink::Rez {
                staged, archive, ..
            } => archive.wal_tick(std::mem::take(staged)),
            Sink::Dendro { staged, writer, .. } => {
                writer.commit(std::mem::take(staged)).map_err(archive_err)
            }
        }
    }

    /// Open a recording for an endpoint that became reachable after the
    /// archive was created.
    ///
    /// The writer thread is still running and the archive still holds its
    /// sender, so a recording can join an open archive at any point. The
    /// endpoint's `systeminfo` must already be fetched — that is what supplies
    /// the `host` label — which is why the caller does this after the metadata
    /// fetch rather than at probe time.
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
        let labels = build_rez_labels(config, ep);
        warn_if_indistinguishable(&mut self.seen_labels, &labels, &ep.config.url);
        let metadata = build_rez_metadata(config, ep, run_events);
        let seed = rez_v3_writer::ManifestSeed {
            labels,
            metadata: metadata.clone(),
            // The SOURCE's anchor where there is one. This recording's rows
            // carry the agent's timestamps, so anchoring it on the recorder's
            // clock instead would make `ts + wall_offset` resolve against a
            // reading neither party took — and the two clocks are on different
            // hosts, so the error is the skew between them, not a rounding.
            //
            // A Prometheus source, or an agent too old to report an anchor,
            // keeps the recorder's: that recording's rows are the recorder's
            // stamps too, so the pair stays coherent either way. Coherence is
            // per recording, which is what lets one archive hold both.
            clock_anchor_wall_ns: ep
                .agent
                .clock_anchor_wall_ns
                .and_then(|a| u64::try_from(a).ok())
                .unwrap_or(clock_anchor_wall_ns),
        };
        match &mut self.sink {
            Sink::Rez { recs, archive, .. } => {
                let writer = archive.add_recording(seed)?;
                recs.insert(idx, rez_v3_writer::StreamRecorderV3::new(writer));
            }
            Sink::Dendro { recs, writer, .. } => {
                let rec = writer
                    .add_source(seed.labels, seed.metadata, seed.clock_anchor_wall_ns)
                    .map_err(archive_err)?;
                recs.insert(idx, rec);
            }
        }
        self.metadata.insert(idx, metadata);
        Ok(())
    }

    /// Add `events` to every open recording's `KEY_EVENTS` payload.
    ///
    /// Each recording's stored map is cloned, the events merged in
    /// (`merge_events_into`, which parses what is there and dedups by id),
    /// and the result sent through the writer: to a `.rez` as a whole-map
    /// replacement, and to a dendro archive as a patch of the one key, since
    /// dendro merges a patch key by key. The stored copy is updated only once
    /// the writer accepts it, so a failed update can be retried with the same
    /// input. Every recording is attempted even if an earlier one failed; the
    /// first error is reported.
    fn merge_events(&mut self, events: &[Event]) -> Result<(), String> {
        let mut first_err = None;
        let indices: Vec<usize> = match &self.sink {
            Sink::Rez { recs, .. } => recs.keys().copied().collect(),
            Sink::Dendro { recs, .. } => recs.keys().copied().collect(),
        };
        for idx in indices {
            let Some(current) = self.metadata.get(&idx) else {
                first_err.get_or_insert(format!(
                    "the recording for endpoint {idx} has no metadata to add events to"
                ));
                continue;
            };
            let mut merged = current.clone();
            let result =
                merge_events_into(&mut merged, events).and_then(|()| match &mut self.sink {
                    Sink::Rez { recs, .. } => recs[&idx].update_metadata(merged.clone()),
                    Sink::Dendro { recs, .. } => {
                        let patch = merged
                            .get(crate::parquet_metadata::KEY_EVENTS)
                            .map(|v| (crate::parquet_metadata::KEY_EVENTS.to_string(), v.clone()))
                            .into_iter()
                            .collect();
                        recs.get_mut(&idx)
                            .expect("the index came from this map")
                            .update_metadata(patch)
                            .map_err(archive_err)
                    }
                });
            match result {
                Ok(()) => {
                    self.metadata.insert(idx, merged);
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Run every recording's seal check.
    ///
    /// All of them every tick, not just the ones that scraped: a seal decision
    /// that were ingest-driven would leave an unreachable endpoint's pre-outage
    /// rows unsealed forever, and the age bound would stop bounding the
    /// kill-loss window. Reports the first failure but still checks the rest,
    /// so a failure is attributed to the recording that caused it rather than
    /// to whichever happened to be checked first — note this is about
    /// reporting, not survival: the recordings share one writer thread, and a
    /// failure in any of its arms tears that writer down for all of them.
    fn maybe_seal(&mut self) -> Result<(), String> {
        let mut first_err = None;
        match &mut self.sink {
            Sink::Rez { recs, .. } => {
                for rec in recs.values_mut() {
                    if let Err(e) = rec.maybe_seal() {
                        first_err.get_or_insert(e);
                    }
                }
            }
            Sink::Dendro { recs, .. } => {
                for rec in recs.values_mut() {
                    if let Err(e) = rec.maybe_seal() {
                        first_err.get_or_insert(archive_err(e));
                    }
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    /// Mark the recording complete and stop the writer, reporting either
    /// failure.
    ///
    /// Both halves matter. `RecordingWriter::finalize` only *queues* the
    /// completion — the writer owns the thread now, so the handle cannot join
    /// it — and the final seal it triggers runs after that hand-off returns. So
    /// the archive is joined here, unconditionally, and its result is folded
    /// in: without it a failure while sealing the last segments would surface
    /// only as a `Drop` warning and the recording would report success.
    fn finalize(self, clock_offset: (u64, i64)) -> Result<(), String> {
        let RezStream {
            sink,
            seen_labels: _,
            metadata: _,
            last_stamp,
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
        let joined = match sink {
            Sink::Rez {
                recs, mut archive, ..
            } => {
                for (idx, rec) in recs {
                    if let Err(e) = rec.finalize(close(idx)) {
                        first_err.get_or_insert(e);
                    }
                }
                archive.join()
            }
            Sink::Dendro {
                recs, mut writer, ..
            } => {
                for (idx, rec) in recs {
                    let (ts, wall_offset) = close(idx);
                    if let Err(e) = rec.finalize((ts, wall_offset)) {
                        first_err.get_or_insert(archive_err(e));
                    }
                }
                writer.join().map_err(archive_err)
            }
        };
        first_err.map_or(joined, Err)
    }

    /// What to tell the user after a mid-recording failure: where the data
    /// captured so far can still be read from — which for v3 is the output
    /// path itself, since the file is a valid `.rez` from the moment it is
    /// created.
    fn recovery_note(&self) -> String {
        format!(
            "note: the recording so far is readable at {}",
            self.sink.path().display()
        )
    }

    /// Stop the writer and leave nothing behind. Only for the paths where the
    /// recording captured no samples at all — a stub is not a recovery
    /// artifact, and the writer refuses to overwrite, so leaving one behind
    /// would also block the retry.
    fn discard(self) {
        let path = self.sink.path().to_path_buf();
        // Drop first: joining the writer thread is what guarantees nothing is
        // still appending to the file we are about to unlink. There is no
        // abort — a dropped writer leaves a valid recording, which is exactly
        // why this path has to remove it explicitly rather than rely on a
        // staging convention. The recorders drop before the writer (see
        // `Sink`).
        drop(self.sink);
        // Safe to remove: both writers claimed this path with O_EXCL during
        // THIS run, so it cannot be a file that was already there. Both
        // containers are SQLite, with the same sidecars.
        //
        // Sidecars included. A `.rez` is three files while it is open, and
        // SQLite only cleans `-wal`/`-shm` up on a CLEAN close — so removing
        // the main file alone can leave a stray `-wal` beside a path the next
        // run believes is free. That is worse than a stale main file: `O_EXCL`
        // catches the main file and says so, while a stray sidecar is adopted
        // silently by the newly created database.
        rez_sqlite::RezDb::remove_archive(&path);
    }
}

impl Sink {
    fn path(&self) -> &Path {
        match self {
            Sink::Rez { archive, .. } => archive.path(),
            Sink::Dendro { path, .. } => path,
        }
    }
}

/// Open the streaming `.rez` writer for a just-activated endpoint and spawn
/// its writer thread, creating the output file.
///
/// Both the file creation and the thread spawn can fail, which is why this
/// happens at activation rather than lazily on the first snapshot.
/// Open the archive and one recording per endpoint in `eps`.
///
/// `eps` carries each endpoint's index alongside it so the returned stream can
/// route a scrape back to the recording that owns it.
///
/// Every recording lands in ONE archive: that is what a multi-recording `.rez`
/// is, and it is why the arms of an A/B captured this way share a clock anchor
/// and a load environment rather than differing in both, as two sequential
/// single-endpoint runs would.
fn start_rez_recorder(
    config: &RecordingConfig,
    eps: &[(usize, &EndpointState)],
    clock_anchor_wall_ns: u64,
) -> Result<RezStream, String> {
    let sink = if config.format == Format::Dendro {
        Sink::Dendro {
            recs: BTreeMap::new(),
            staged: Vec::new(),
            writer: metriken_archive::ArchiveWriter::create(
                &config.output,
                metriken_archive::WriterConfig::default(),
            )
            .map_err(|e| format!("failed to create {}: {e}", config.output.display()))?,
            path: config.output.clone(),
            schemas: BTreeMap::new(),
        }
    } else {
        Sink::Rez {
            recs: BTreeMap::new(),
            staged: Vec::new(),
            archive: rez_v3_writer::RezArchive::create(&config.output)?,
        }
    };
    let mut stream = RezStream {
        sink,
        metadata: BTreeMap::new(),
        last_stamp: BTreeMap::new(),
        seen_labels: BTreeMap::new(),
    };

    for (idx, ep) in eps {
        // No run events yet: the archive is opened before the wrapped command
        // is spawned, so `run_start` reaches these recordings by update.
        if let Err(e) = stream.add_endpoint(*idx, config, ep, clock_anchor_wall_ns, &[]) {
            // `RezArchive::create` claimed the path with O_EXCL moments ago, so
            // the half-built archive is unambiguously ours to remove. Leaving
            // it would both look like a recording and block the retry, since
            // the writer refuses to overwrite an existing path.
            stream.discard();
            return Err(e);
        }
    }

    Ok(stream)
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

/// Build the `per_source_metadata` JSON written by the recorder.
///
/// When `source` is a JSON array, each name in the array becomes an entry
/// with the same per-source fields duplicated — a single endpoint
/// represents all the listed sources, so the timing and role apply
/// identically to every name.
///
/// Returns `None` when no per-source fields are available.
fn build_per_source_metadata(
    source: &str,
    first_sample_ns: Option<u64>,
    last_sample_ns: Option<u64>,
    role: Option<&str>,
    sampler_status: Option<&str>,
) -> Option<String> {
    let mut source_meta = serde_json::Map::new();
    if let Some(ns) = first_sample_ns {
        source_meta.insert(
            parquet_metadata::NESTED_FIRST_SAMPLE_NS.to_string(),
            serde_json::json!(ns),
        );
    }
    if let Some(ns) = last_sample_ns {
        source_meta.insert(
            parquet_metadata::NESTED_LAST_SAMPLE_NS.to_string(),
            serde_json::json!(ns),
        );
    }
    if let Some(role) = role {
        source_meta.insert(
            parquet_metadata::NESTED_ROLE.to_string(),
            serde_json::json!(role),
        );
    }
    if let Some(ss) = sampler_status {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(ss) {
            source_meta.insert(parquet_metadata::NESTED_SAMPLER_STATUS.to_string(), value);
        }
    }

    if source_meta.is_empty() {
        return None;
    }

    let source_names: Vec<String> =
        serde_json::from_str::<Vec<String>>(source).unwrap_or_else(|_| vec![source.to_string()]);

    let mut psm = serde_json::Map::new();
    for name in &source_names {
        psm.insert(name.clone(), serde_json::Value::Object(source_meta.clone()));
    }
    serde_json::to_string(&psm).ok()
}

struct EndpointWriter {
    writer: std::fs::File,
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
/// tick for TCP-timeout scales, and the tick is what drives `.rez` age seals
/// and the ctrl-c check (`STATE` is only re-checked at the loop top) — so one
/// hung endpoint would stall durability and shutdown, not just this sample.
///
/// Deliberately generous rather than exactly one interval: this must catch a
/// *hung* endpoint, not a merely slow one. A local agent already takes ~75 ms
/// to answer, so at `--interval 5ms` a one-interval bound would time out every
/// single scrape and record nothing at all, where the honest outcome is
/// sampling at the endpoint's pace. Floored so short intervals stay
/// recordable, capped so a long interval still hands back a bounded tick.
fn tick_timeout(interval: Duration) -> Duration {
    (interval * 2).clamp(Duration::from_secs(2), MAX_SCRAPE_TIMEOUT)
}

/// Intervals a stream pump may have queued for the tick loop before its send
/// blocks, per endpoint.
///
/// The loop drains the queue every tick, so this only fills when the loop
/// falls behind the agent's frame rate — a slow commit, or an `--interval`
/// shorter than the writer can keep up with. Bounded so that case pushes back
/// on the socket rather than growing without limit.
const STREAM_QUEUE_PER_ENDPOINT: usize = 16;

/// Handle a run that asked for (or defaulted to) archive output that this
/// endpoint set cannot produce: either rewrite `config` to record parquet and
/// carry on, or exit non-zero.
///
/// The distinction is who chose the archive. `--format dendro` or an
/// `-o out.dendro` is a request the recorder must not quietly substitute — a
/// pipeline that goes on to read `out.dendro` would find a parquet file, or
/// nothing. But an archive is also what a bare `rezolus record` picks with
/// nothing to go on, and demanding a flag before it will record a run it
/// recorded before archives became the default is a regression for no gain:
/// nothing downstream has been promised a filename yet, so the recorder picks
/// the format that fits.
///
/// The refusal exits 1 rather than returning, because a `record && analyze`
/// pipeline sees only the exit code: this used to print the error and exit 0,
/// leaving the next command to read whatever `out.rez` a previous run left
/// behind.
fn demote_from_rez(config: &mut RecordingConfig, reason: &str) {
    if !config.format_defaulted {
        eprintln!("error: {reason}");
        std::process::exit(1);
    }
    let name = config::format_name(config.format);
    config.format = Format::Parquet;
    config.output = PathBuf::from("rezolus.parquet");
    // `--separate` finalizes through `separate_output_path`, so the run writes
    // `rezolus_<source>.parquet` per endpoint and never `config.output` itself.
    // Naming the file it will not write is worse than saying nothing, and the
    // `--separate` demotion makes this the message that user actually sees.
    let written = if config.separate {
        format!(
            "{}_<source>.parquet per endpoint",
            config
                .output
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
        )
    } else {
        config.output.display().to_string()
    };
    eprintln!(
        "note: {reason}; recording parquet to {written} instead (pass --format {name} to require a .{name} archive)"
    );
}

/// Runs the Rezolus `recorder` which pulls metrics from one or more endpoints
/// and writes them to parquet file(s). Supports Rezolus msgpack and Prometheus
/// text format endpoints, with auto-detection.
pub fn run(mut config: RecordingConfig) {
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
        println!();
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

    // `.rez` per-sampler archive mode. By this point the output extension has
    // already been folded into the format, so the format is the whole answer.
    let mut rez_mode = wants_rez(config.format);

    if rez_mode {
        // Neither a Prometheus endpoint nor several endpoints is a blocker any
        // more. Each endpoint becomes its own label-tagged recording in one
        // archive, and a Prometheus scrape converts to a V3 acquisition group
        // on the way in — one request and one response is exactly one
        // acquisition, which is what the group models.
        //
        // `--separate` is what remains: it writes a file per endpoint, which
        // one archive cannot do. An explicit archive was already rejected at
        // parse time; reaching here means the format was merely defaulted, so
        // demote to the parquet-per-endpoint run the flag asked for rather
        // than erroring on a format nobody chose.
        let blocker = None.or_else(|| {
            // Only with several endpoints: with one there is nothing to
            // separate, and `main`'s multi-endpoint blocker never fired
            // on a single-endpoint run either.
            (config.separate && config.endpoints.len() > 1).then(|| {
                format!(
                    "--separate writes one file per endpoint, which a .{} cannot do (every \
                     endpoint is a recording inside the one archive)",
                    config::format_name(config.format)
                )
            })
        });

        if let Some(reason) = blocker {
            demote_from_rez(&mut config, &reason);
            rez_mode = false;
        }
    }

    let out_dir = output_dir(&config.output);

    // A `.dendro` records every Rezolus agent from its replication stream and
    // scrapes every Prometheus endpoint; every other format scrapes both. Read
    // after the demotion above, which can turn a defaulted `.dendro` into
    // parquet.
    let stream_agents = rez_mode && config.streams_agents();

    let interval_dur: Duration = config.interval.into();
    let connect_timeout = tick_timeout(interval_dur);

    // Subscriptions opened at startup, one slot per endpoint, handed to their
    // pumps once the archive they feed exists. Only a `.dendro` run's agents
    // fill any.
    let mut opened: Vec<Option<stream::Subscription>> = endpoints.iter().map(|_| None).collect();

    // Probe all endpoints (best-effort startup). An agent that cannot serve
    // its stream into a `.dendro` is fatal here, before the archive exists,
    // so nothing is left behind; one that is not reachable yet is the
    // ordinary retry-each-tick case.
    rt.block_on(async {
        for (idx, ep) in endpoints.iter_mut().enumerate() {
            match activate_endpoint(
                &client,
                ep,
                stream_agents,
                interval_dur,
                connect_timeout,
                RefusedAt::Startup,
            )
            .await
            {
                Ok(Activation::Stream(sub)) => {
                    info!(
                        "endpoint {} ({}): subscribed to its replication stream",
                        ep.config.source_label(),
                        ep.config.url
                    );
                    opened[idx] = Some(*sub);
                }
                Ok(Activation::Scrape) => {
                    info!(
                        "endpoint {} ({}): detected {:?}",
                        ep.config.source_label(),
                        ep.config.url,
                        ep.protocol()
                    );
                }
                Ok(Activation::Pending(why)) => warn!("{why}"),
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
    });

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

    // The streaming `.rez` writer is opened here — when the endpoints become
    // active, not lazily on the first snapshot — because creating the
    // output (or, in v2, the `<output>.partial`) and spawning the writer thread
    // are both fallible.
    let mut rez_recorder: Option<RezStream> = None;
    if rez_mode {
        {
            // Every active endpoint becomes a recording in ONE archive. Only
            // the endpoints active at this point: a `.rez` recording is opened
            // when its writer is, and an endpoint that activates later through
            // the Pending path has no recording to append to.
            let active: Vec<(usize, &EndpointState)> = endpoints
                .iter()
                .enumerate()
                .filter(|(_, ep)| ep.status == EndpointStatus::Active)
                .collect();

            if !active.is_empty() {
                match start_rez_recorder(&config, &active, clock_anchor_wall_ns) {
                    Ok(rec) => rez_recorder = Some(rec),
                    Err(e) => {
                        eprintln!(
                            "error: failed to start the .{} recording: {e}",
                            config::format_name(config.format)
                        );
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    // Per-endpoint Prometheus converters, kept OUTSIDE `EndpointWriter`
    // because `.rez` mode has no writer to hang them on: it has no msgpack
    // spool at all, snapshots go straight into the streaming writer. Both
    // modes need the same converter — one per endpoint, because it holds the
    // (name, labels) -> id map that keeps a column's identity stable across
    // scrapes, and two endpoints' id spaces must not mix.
    let mut prom_converters: Vec<Option<prometheus::PrometheusConverter>> = endpoints
        .iter()
        .map(|ep| {
            (ep.status == EndpointStatus::Active && ep.protocol() == Some(&Protocol::Prometheus))
                .then(prometheus::PrometheusConverter::new)
        })
        .collect();

    let mut writers: Vec<Option<EndpointWriter>> = endpoints
        .iter()
        .map(|ep| {
            // In `.rez` mode there is no msgpack spool at all: snapshots go
            // straight into the streaming writer, so the temp file and the
            // re-serialization that fed it are both gone.
            if ep.status == EndpointStatus::Active && !rez_mode {
                let writer = match tempfile_in(out_dir.clone()) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("failed to create temp file: {e}");
                        std::process::exit(1);
                    }
                };
                Some(EndpointWriter { writer })
            } else {
                None
            }
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
                    if let Some(rec) = rez_recorder.take() {
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
        // the child's own start time; it goes to the open `.rez` recordings
        // at once and rides in the seed of any recording opened later. The
        // parquet footer takes the whole list after the loop.
        let run_marker = config.command.as_deref().map(RunMarker::new);
        let mut run_events: Vec<Event> = Vec::new();
        if let (Some(marker), Some(cmd)) = (&run_marker, config.command.as_deref()) {
            let command_line = config.record_command_line.then(|| cmd.join(" "));
            let spawned_at = anchored_at(clock_anchor_wall_ns, clock_anchor_mono.elapsed());
            run_events.push(marker.start_event(spawned_at, command_line));
            if let Some(rec) = rez_recorder.as_mut() {
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
                scrape_timeout,
                stream_tx.clone(),
            ));
        };
        for (idx, sub) in opened.iter_mut().enumerate() {
            if let Some(sub) = sub.take() {
                spawn_pump(idx, sub, endpoints[idx].config.url.clone());
            }
        }
        // The last tick's clock observation, handed to `.rez` finalization so
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
            // loop notices, losing every still-unsealed `.rez` segment — which
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
            // Both clocks, every tick. `wall_ns` is the raw reading every
            // non-`.rez` consumer has always used (endpoint success stamps, the
            // msgpack spool); `.rez` rows are stamped on the anchored monotonic
            // timeline and carry the difference as a per-row observation
            // instead. The prometheus converter used to be in the first group
            // and is now in the second: its scrape window is stored relative
            // to an anchored row `ts`, so it has to be anchored too.
            let wall_ns = wall_now_ns();
            let (anchored_ns, wall_offset_ns) =
                anchored_stamp(clock_anchor_wall_ns, clock_anchor_mono.elapsed(), wall_ns);
            last_clock = (anchored_ns, wall_offset_ns);

            // A `.rez` ingest failure for this tick. Held rather than acted on
            // where it happens: that is inside the per-endpoint result loop,
            // whose `break` would only leave that loop. Surfaced below,
            // alongside `maybe_seal`'s — the same class of failure at the same
            // cadence, and the earlier and more specific of the two wins.
            let mut ingest_failed: Option<String> = None;
            // A `.rez` endpoint that activated mid-run and could not be given a
            // recording. Surfaced on the same path as an ingest failure, below,
            // so the partial archive is named and the run exits non-zero rather
            // than finalizing one recording short.
            let mut late_endpoint_failure: Option<String> = None;

            // Scrape all active endpoints concurrently. A streamed agent is
            // not scraped: its pump delivers, and the loop stages what
            // arrived alongside the scrapes, below.
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

                        // `.rez`: decode once and hand the snapshot straight to
                        // the streaming writer. No spool, no re-serialization.
                        //
                        // `Snapshot::from_msgpack`, not a bare `from_slice`:
                        // this is the exact wire the recorder controls end to
                        // end for `.rez` mode (single endpoint, msgpack-only,
                        // never Prometheus), so the depth-capped,
                        // trailing-byte-checked decode applies uniformly to
                        // whichever version the agent sends — including the
                        // V3 groups this build natively ingests. Decoding a
                        // concrete `SnapshotV3` directly (skipping the
                        // untagged enum probe entirely) would need to know the
                        // endpoint's `snapshot_format` ahead of time, which
                        // the recorder does not: it is agent-side config, not
                        // negotiated over HTTP, and `.rez` mode serves V1/V2/V3
                        // endpoints alike from this one call site.
                        //
                        // Correction to this decision's original reasoning
                        // (recorded here rather than by editing the already-
                        // committed history): a "try SnapshotV3 first, fall
                        // back to the untagged decode" scheme was rejected
                        // partly on the claim that a V2 payload with an empty
                        // `counters` vec could decode as a spurious
                        // empty-`groups` SnapshotV3 by reading only 4 of the
                        // 6 top-level array elements and silently ignoring
                        // the rest. That claim is wrong:
                        // metriken-exposition's own
                        // `trailing_extra_field_errors_not_ignored` test
                        // shows a too-long positional payload FAILS a
                        // struct's decode rather than having the extra
                        // elements silently ignored, so that specific
                        // landmine does not exist even without a hand-rolled
                        // trailing-byte check. The rest of the decision
                        // stands on its own: the recorder still cannot know
                        // an endpoint's `snapshot_format` ahead of time, so
                        // there is no reliable way to pick "try V3 first" over
                        // "try the untagged decode" without guessing wrong on
                        // most of a V1/V2-heavy fleet — `from_msgpack` is
                        // still the right-sized, version-agnostic fix here.
                        if rez_mode {
                            // A Prometheus endpoint converts its text to a
                            // snapshot here; a rezolus one decodes msgpack.
                            // Both reach the same `ingest` — the archive has no
                            // opinion about which wire a recording came off.
                            //
                            // The converted snapshot is NOT run through
                            // `inject_provenance`: the converter already writes
                            // `source` and `endpoint` into every metric's
                            // metadata, so injecting again would be a second
                            // spelling of the same fact, free to disagree.
                            let snapshot = match prom_converters[idx].as_mut() {
                                Some(conv) => {
                                    let text = String::from_utf8_lossy(&body);
                                    Some(conv.convert(&text, request_ns, response_ns))
                                }
                                None => match metriken_exposition::Snapshot::from_msgpack(&body) {
                                    Ok(snapshot) => {
                                        note_epoch_change(&mut endpoints[idx], &snapshot);
                                        Some(snapshot)
                                    }
                                    Err(e) => {
                                        warn!(
                                            "msgpack decode error for {}: {e}",
                                            endpoints[idx].config.source_label()
                                        );
                                        None
                                    }
                                },
                            };
                            if let (Some(snapshot), Some(rec)) = (snapshot, rez_recorder.as_mut()) {
                                // The producer's stamp when it sent one, this
                                // tick's otherwise. See
                                // `snapshot_producer_stamp`.
                                let (ts, wall_offset) = snapshot_producer_stamp(&snapshot)
                                    .unwrap_or((anchored_ns, wall_offset_ns));
                                if let Err(e) = rec.stage(
                                    idx,
                                    &endpoints[idx].config.url,
                                    &snapshot,
                                    ts,
                                    wall_offset,
                                ) {
                                    ingest_failed.get_or_insert(e);
                                }
                            }
                            continue;
                        }

                        if let Some(ref mut ew) = writers[idx] {
                            let bytes = if let Some(ref mut conv) = prom_converters[idx] {
                                // Prometheus: parse text → snapshot → msgpack
                                let text = String::from_utf8_lossy(&body);
                                // The real round trip, not the tick's clock:
                                // every value in this body was read somewhere
                                // inside it. See `Scraped`.
                                let snapshot = conv.convert(&text, request_ns, response_ns);
                                match rmp_serde::encode::to_vec(&snapshot) {
                                    Ok(b) => b,
                                    Err(e) => {
                                        error!(
                                            "serialize error for {}: {e}",
                                            endpoints[idx].config.source_label()
                                        );
                                        continue;
                                    }
                                }
                            } else {
                                // Msgpack: deserialize, inject provenance,
                                // re-serialize. `from_msgpack`, not a bare
                                // `from_slice` — same depth-cap/trailing-byte
                                // hardening as the `.rez`-mode call site above,
                                // and just as contained here: this branch is
                                // unconditionally msgpack (the Prometheus
                                // sibling branch above never reaches it).
                                match metriken_exposition::Snapshot::from_msgpack(&body) {
                                    Ok(snapshot) => {
                                        note_epoch_change(&mut endpoints[idx], &snapshot);
                                        // The body goes through verbatim. It
                                        // used to be decoded, relabelled and
                                        // re-encoded here; provenance is the
                                        // recording's now, so there is nothing
                                        // to rewrite and no reason to pay a
                                        // round trip.
                                        body.clone()
                                    }
                                    Err(e) => {
                                        warn!(
                                            "msgpack decode error for {}: {e}",
                                            endpoints[idx].config.source_label()
                                        );
                                        continue;
                                    }
                                }
                            };

                            if let Err(e) = ew.writer.write_all(&bytes) {
                                error!(
                                    "write error for {}: {e}",
                                    endpoints[idx].config.source_label()
                                );
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
                rez_recorder.as_mut(),
                wall_ns,
            ) {
                ingest_failed.get_or_insert(e);
            }

            let pending_indices: Vec<usize> = endpoints
                .iter()
                .enumerate()
                .filter(|(_, ep)| ep.status == EndpointStatus::Pending)
                .map(|(i, _)| i)
                .collect();

            for idx in pending_indices {
                // The same activation as startup, with one difference: an
                // agent that cannot serve its stream is refused on its own.
                // The archive already holds the other endpoints' recordings,
                // so they keep recording and finalize as usual; the refused
                // endpoint is never retried, and the run exits 1 at the end.
                let activated = activate_endpoint(
                    &client,
                    &mut endpoints[idx],
                    stream_agents,
                    interval_dur,
                    scrape_timeout,
                    RefusedAt::MidRun,
                )
                .await;
                let sub = match activated {
                    Ok(Activation::Pending(why)) => {
                        debug!("{why}");
                        continue;
                    }
                    Err(e) => {
                        refuse_mid_run(&mut endpoints[idx], &e);
                        continue;
                    }
                    Ok(Activation::Stream(sub)) => {
                        info!(
                            "endpoint {} ({}) now reachable, subscribed to its \
                             replication stream",
                            endpoints[idx].config.source_label(),
                            endpoints[idx].config.url
                        );
                        Some(*sub)
                    }
                    Ok(Activation::Scrape) => {
                        // Deliberately says "now reachable", not "starting
                        // capture": the block below can still fail to open a
                        // recording for it, and claiming capture had started
                        // one line before that error is how the silent-drop
                        // bug read in the logs.
                        info!(
                            "endpoint {} ({}) now reachable",
                            endpoints[idx].config.source_label(),
                            endpoints[idx].config.url
                        );
                        // A late endpoint that probes as prometheus gets a
                        // converter now, the same as one present at startup.
                        if endpoints[idx].protocol() == Some(&Protocol::Prometheus)
                            && prom_converters[idx].is_none()
                        {
                            prom_converters[idx] = Some(prometheus::PrometheusConverter::new());
                        }
                        None
                    }
                };

                // An archive DOES reach here: startup only exits when no
                // endpoint at all was reachable, so a run with one endpoint up
                // and one still starting commits to the archive with a
                // recording for the first and reaches this path for the
                // second. It has no spool to create — it needs a recording
                // opened on the live archive instead, which is what keeps the
                // "will retry each tick" warning honest.
                if rez_mode {
                    // `None` means the recording already failed and was
                    // reported; the loop is about to exit.
                    if let Some(rec) = rez_recorder.as_mut() {
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
                                    "failed to open a .{} recording for {}: {e}",
                                    config::format_name(config.format),
                                    endpoints[idx].config.url
                                ));
                            }
                        }
                    }
                } else {
                    writers[idx] = Some(EndpointWriter {
                        writer: tempfile_in(out_dir.clone()).expect("failed to create temp file"),
                    });
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
            let committed = match rez_recorder.as_mut() {
                Some(rec) => rec.commit_tick(),
                None => Ok(()),
            };

            // Seal checks run every tick, scrape or not: if they were
            // ingest-driven an unreachable endpoint would leave its pre-outage
            // rows unsealed forever and the age bound would stop bounding the
            // kill-loss window.
            let sealed = match rez_recorder.as_mut() {
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
                if let Some(rec) = rez_recorder.take() {
                    // Dropped, not discarded: what is on disk holds everything
                    // written before the failure and is the recovery artifact —
                    // in v2 every sealed segment, in v3 every committed tick.
                    eprintln!("{}", rec.recovery_note());
                }
                break;
            }
            scraped_last_pass = true;
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
        // The parquet path reads `run_events` after the loop; the `.rez`
        // path merges it into each recording right before finalizing.
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
        // exit, bounded by one interval plus the tick timeout, and cut short
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
        if endpoints.iter().any(|ep| ep.streaming) && rez_recorder.is_some() {
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
                                rez_recorder.as_mut(),
                                wall_now_ns(),
                            ) {
                                failed = Some(e);
                                break;
                            }
                        }
                        _ = tokio::time::sleep_until(deadline.into()) => {
                            for idx in waiting {
                                warn!(
                                    "{} ({}): no stream frame from after the command exited \
                                     arrived within {}; the recording ends before it",
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
            let failed = failed
                .or_else(|| {
                    drain_stream_events(
                        &mut stream_rx,
                        &mut endpoints,
                        rez_recorder.as_mut(),
                        wall_now_ns(),
                    )
                })
                .or_else(|| {
                    rez_recorder
                        .as_mut()
                        .and_then(|rec| rec.commit_tick().err())
                });
            if let Some(e) = failed {
                eprintln!("error: recording failed: {e}");
                recording_failed.store(true, Ordering::SeqCst);
                if let Some(rec) = rez_recorder.take() {
                    eprintln!("{}", rec.recovery_note());
                }
            }
        }

        for ew in writers.iter_mut().flatten() {
            let _ = ew.writer.flush();
        }

        let active_count = endpoints
            .iter()
            .filter(|ep| ep.first_success_ns.is_some())
            .count();

        if active_count == 0 {
            // Nothing was captured, so the recording holds no recoverable data:
            // stop the writer and remove it rather than leaving a stub behind.
            // Explicit because the `exit` below skips destructors.
            if let Some(rec) = rez_recorder.take() {
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

        // `.rez` mode finalizes a per-sampler archive instead of parquet/raw:
        // the segments are already on disk, so this only seals the (small) open
        // ones and marks the recording complete — in v2 by writing the final
        // manifest and renaming the `.partial` into place, in v3 by one commit.
        if rez_mode {
            // `None` means the recording already failed mid-run and reported it
            // (what was on disk was left in place there, and its path printed);
            // nothing to add here.
            if let Some(mut rec) = rez_recorder.take() {
                // `run_end` before finalize: `update_metadata` needs a live
                // writer, and finalize is what stops it. Not fatal for the
                // same reason the `run_start` merge is not.
                if let Some(event) = run_events
                    .iter()
                    .find(|e| e.kind.as_deref() == Some("run_end"))
                {
                    if let Err(e) = rec.merge_events(std::slice::from_ref(event)) {
                        warn!("failed to add the run_end event to the recording: {e}");
                    }
                }
                if let Err(e) = rec.finalize(last_clock) {
                    // Must flip `recording_failed`: without it a failed tail
                    // seal / manifest write / rename (ENOSPC at the end of a
                    // long capture, a rename EACCES) exits 0, and neither
                    // container overwrites a pre-existing output — v2 because
                    // it stages at `.partial`, v3 because `create` refuses an
                    // existing file — so the PREVIOUS run's `out.rez` is still
                    // sitting there for the next command in the pipeline to
                    // analyze.
                    eprintln!(
                        "error saving .{} archive: {e}",
                        config::format_name(config.format)
                    );
                    recording_failed.store(true, Ordering::SeqCst);
                } else {
                    info!(
                        "wrote .{} archive to {}",
                        config::format_name(config.format),
                        config.output.display()
                    );
                }
            }
            // Endpoints refused after the archive opened, on first
            // activation or on a reconnect (a refusal at startup exits
            // before the archive exists). The recording went on without
            // them; the exit status says so.
            let refused_mid_run = endpoints
                .iter()
                .filter(|ep| ep.status == EndpointStatus::Refused)
                .count();
            if refused_mid_run > 0 {
                eprintln!(
                    "error: {refused_mid_run} endpoint(s) were refused mid-run and are not in \
                     {} past the refusal; see the refusal above",
                    config.output.display()
                );
                recording_failed.store(true, Ordering::SeqCst);
            }
            return outcome;
        }

        // Every finalization error below is the same class as the `.rez`
        // finalize failure above: the samples are gone (or partial) and the
        // only thing a supervisor, CI job or `record && analyze` pipeline can
        // see is the exit code, so none of these may report success.
        let fail = |msg: String| {
            eprintln!("{msg}");
            recording_failed.store(true, Ordering::SeqCst);
        };

        match config.format {
            Format::Raw => {
                for (idx, ew) in writers.iter_mut().enumerate() {
                    if let Some(ref mut ew) = ew {
                        if endpoints[idx].first_success_ns.is_none() {
                            continue;
                        }
                        let dest_path = if config.separate || active_count > 1 {
                            separate_output_path(
                                &config.output,
                                endpoints[idx].config.source_label(),
                            )
                        } else {
                            config.output.clone()
                        };
                        let _ = ew.writer.rewind();
                        match std::fs::File::create(&dest_path) {
                            Ok(mut dest) => {
                                if let Err(e) = std::io::copy(&mut ew.writer, &mut dest) {
                                    fail(format!("error writing {}: {e}", dest_path.display()));
                                }
                            }
                            Err(e) => fail(format!("error creating {}: {e}", dest_path.display())),
                        }
                    }
                }
                debug!("finished (raw)");
            }
            Format::Parquet if config.separate => {
                info!("converting recordings to parquet (separate files)...");
                for (idx, ew) in writers.iter_mut().enumerate() {
                    if let Some(ref mut ew) = ew {
                        if endpoints[idx].first_success_ns.is_none() {
                            continue;
                        }
                        let dest_path = separate_output_path(
                            &config.output,
                            endpoints[idx].config.source_label(),
                        );
                        match std::fs::File::create(&dest_path) {
                            Ok(dest) => {
                                let _ = ew.writer.rewind();
                                let converter = build_parquet_converter(
                                    &config,
                                    &endpoints[idx],
                                    &prom_converters[idx],
                                    &run_events,
                                );
                                if let Err(e) = converter
                                    .convert_file_handle(ew.writer.try_clone().unwrap(), dest)
                                {
                                    fail(format!(
                                        "error saving parquet for {}: {e}",
                                        endpoints[idx].config.source_label()
                                    ));
                                } else {
                                    info!("wrote {}", dest_path.display());
                                }
                            }
                            Err(e) => {
                                fail(format!("error creating {}: {e}", dest_path.display()));
                            }
                        }
                    }
                }
            }
            Format::Parquet => {
                if active_count == 1 {
                    // Single endpoint — direct conversion, no combine needed
                    info!("converting the recording to parquet... please wait");
                    let idx = endpoints
                        .iter()
                        .position(|ep| ep.first_success_ns.is_some())
                        .unwrap();
                    if let Some(ref mut ew) = writers[idx] {
                        let _ = ew.writer.rewind();
                        match std::fs::File::create(&config.output) {
                            Ok(dest) => {
                                let converter = build_parquet_converter(
                                    &config,
                                    &endpoints[idx],
                                    &prom_converters[idx],
                                    &run_events,
                                );
                                if let Err(e) = converter
                                    .convert_file_handle(ew.writer.try_clone().unwrap(), dest)
                                {
                                    fail(format!("error saving parquet file: {e}"));
                                }
                            }
                            Err(e) => {
                                fail(format!("error creating output file: {e}"));
                            }
                        }
                    }
                } else {
                    // Multiple endpoints — convert each to temp parquet, then combine
                    info!("converting and combining recordings to parquet... please wait");
                    let mut temp_parquets: Vec<tempfile::NamedTempFile> = Vec::new();

                    for (idx, ew) in writers.iter_mut().enumerate() {
                        if let Some(ref mut ew) = ew {
                            if endpoints[idx].first_success_ns.is_none() {
                                continue;
                            }
                            let _ = ew.writer.rewind();

                            let temp = match tempfile::NamedTempFile::new_in(&out_dir) {
                                Ok(t) => t,
                                Err(e) => {
                                    fail(format!("failed to create temp parquet file: {e}"));
                                    continue;
                                }
                            };

                            match std::fs::File::create(temp.path()) {
                                Ok(dest) => {
                                    let converter = build_parquet_converter(
                                        &config,
                                        &endpoints[idx],
                                        &prom_converters[idx],
                                        &run_events,
                                    );
                                    if let Err(e) = converter
                                        .convert_file_handle(ew.writer.try_clone().unwrap(), dest)
                                    {
                                        fail(format!(
                                            "error converting {} to parquet: {e}",
                                            endpoints[idx].config.source_label()
                                        ));
                                        continue;
                                    }
                                    temp_parquets.push(temp);
                                }
                                Err(e) => {
                                    fail(format!("error creating temp parquet: {e}"));
                                }
                            }
                        }
                    }

                    if temp_parquets.len() < 2 {
                        // Only one file survived — just move it
                        if let Some(temp) = temp_parquets.into_iter().next() {
                            if let Err(e) = std::fs::copy(temp.path(), &config.output) {
                                fail(format!("error writing output: {e}"));
                            }
                        } else {
                            fail("error: no data was recorded".to_string());
                        }
                    } else {
                        let paths: Vec<PathBuf> = temp_parquets
                            .iter()
                            .map(|t| t.path().to_path_buf())
                            .collect();

                        if let Err(e) =
                            crate::parquet_tools::combine::combine_files(&paths, &config.output)
                        {
                            fail(format!("error combining parquet files: {e}"));
                        } else {
                            info!("wrote combined recording to {}", config.output.display());
                        }
                    }
                    // temp files cleaned up on drop
                }
            }
            Format::Rez | Format::Dendro => {
                // `.rez` output is finalized above via the `rez_mode` short-circuit,
                // so this arm is never reached (Format::Rez always sets rez_mode).
                unreachable!("rez output is finalized before the format match");
            }
        }

        outcome
    });

    // Every path out of the block above already finalized or discarded the
    // `.rez` writer; this is the backstop for the ones below that skip
    // destructors (`std::process::exit`). Dropping joins the writer thread and
    // leaves what is on disk alone — v2's `.partial`, v3's output file — so a
    // missed path costs a recoverable archive, never a detached thread
    // appending to it after we exit.
    drop(rez_recorder);

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
        assert_eq!(config.format, Format::Dendro);
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

    // ── `--rez-version` wiring ───────────────────────────────────────────────
    //
    // These drive `start_rez_recorder` and the `RezStream` it returns rather
    // than `run()`, which owns `std::process::exit` and a tokio runtime. What
    // they cover is the wiring: which container a version selects, that the
    // result is readable end to end, and that a discarded recording leaves
    // nothing behind. What they do NOT cover is the loop around it — the tick
    // scheduling, the scrape timeouts and the exit paths are exercised by
    // `tests/record_lifecycle.rs` against the real binary.

    // ── agent version capture (issue #1195) ─────────────────────────────────

    #[test]
    fn root_version_parses_the_agent_banner() {
        assert_eq!(
            parse_root_version("Rezolus 5.20.0 Agent\nFor information, see: https://rezolus.com\n")
                .as_deref(),
            Some("5.20.0")
        );
        // A prerelease is the interesting case: most of the sampler history
        // worth bisecting lives in `-alpha.N` builds.
        assert_eq!(
            parse_root_version("Rezolus 5.20.1-alpha.0 Agent\n").as_deref(),
            Some("5.20.1-alpha.0")
        );
    }

    #[test]
    fn root_version_refuses_anything_that_is_not_the_banner() {
        // A proxy error page, another service on the port, and a banner with
        // no version in it. None of these may be recorded as a version.
        assert_eq!(parse_root_version("<html>404</html>"), None);
        assert_eq!(parse_root_version("Prometheus 2.5\n"), None);
        assert_eq!(parse_root_version("Rezolus  Agent\n"), None);
        assert_eq!(parse_root_version("Rezolus 5.20.0 Exporter\n"), None);
        assert_eq!(parse_root_version(""), None);
    }

    #[test]
    fn rez_metadata_carries_the_agent_version() {
        let dir = tempfile::tempdir().unwrap();
        let config = rez_config(&dir.path().join("out.rez"));
        let mut ep = rez_endpoint();
        ep.agent.version = Some("5.19.2".to_string());

        let m = build_rez_metadata(&config, &ep, &[]);
        assert_eq!(
            m.get(parquet_metadata::KEY_VERSION).map(String::as_str),
            Some("5.19.2")
        );
    }

    // ── producer epoch (#1224) ──────────────────────────────────────────────

    fn snap_with_epoch(epoch: Option<&str>) -> metriken_exposition::Snapshot {
        use std::collections::HashMap;
        let mut metadata = HashMap::new();
        metadata.insert("source".to_string(), "rezolus".to_string());
        if let Some(e) = epoch {
            metadata.insert("producer_epoch".to_string(), e.to_string());
        }
        metriken_exposition::Snapshot::V2(metriken_exposition::SnapshotV2 {
            systemtime: std::time::SystemTime::UNIX_EPOCH,
            duration: Duration::ZERO,
            metadata,
            counters: vec![],
            gauges: vec![],
            histograms: vec![],
        })
    }

    #[test]
    fn rez_metadata_carries_the_producer_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let config = rez_config(&dir.path().join("out.rez"));
        let mut ep = rez_endpoint();
        ep.agent.producer_epoch = Some("11111111-2222-4333-8444-555555555555".to_string());

        let m = build_rez_metadata(&config, &ep, &[]);
        assert_eq!(
            m.get(parquet_metadata::KEY_PRODUCER_EPOCH)
                .map(String::as_str),
            Some("11111111-2222-4333-8444-555555555555")
        );
    }

    /// Absent, not empty. An empty epoch would read as "this recording knows
    /// its counters did not restart", which is the opposite of not knowing.
    #[test]
    fn rez_metadata_omits_the_epoch_when_the_agent_reported_none() {
        let dir = tempfile::tempdir().unwrap();
        let config = rez_config(&dir.path().join("out.rez"));
        let ep = rez_endpoint();
        let m = build_rez_metadata(&config, &ep, &[]);
        assert!(!m.contains_key(parquet_metadata::KEY_PRODUCER_EPOCH));
    }

    // ── run events ──────────────────────────────────────────────────────────

    /// The seed carries the run events it is given under `KEY_EVENTS`, so a
    /// recording opened after the wrapped command spawned starts with
    /// `run_start` rather than waiting for an update that already happened.
    #[test]
    fn rez_metadata_carries_the_run_events_when_there_are_any() {
        let dir = tempfile::tempdir().unwrap();
        let config = rez_config(&dir.path().join("out.rez"));
        let ep = rez_endpoint();
        let marker = RunMarker::new(&["/usr/local/bin/bench.sh".to_string()]);
        let start = marker.start_event(TEST_ANCHOR, None);

        let m = build_rez_metadata(&config, &ep, std::slice::from_ref(&start));
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
    fn rez_metadata_omits_the_events_key_without_run_events() {
        let dir = tempfile::tempdir().unwrap();
        let config = rez_config(&dir.path().join("out.rez"));
        let ep = rez_endpoint();
        let m = build_rez_metadata(&config, &ep, &[]);
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

    /// The parquet footer merges the run events into a user `--metadata
    /// events=...` value, as the `.rez` seed does, rather than replacing it.
    #[test]
    fn parquet_events_payload_merges_into_the_users_events() {
        let marker = RunMarker::new(&["bench".to_string()]);
        let start = marker.start_event(TEST_ANCHOR + TEST_SECOND, None);
        let user =
            r#"{"events":[{"timestamp":1700000000000000000,"description":"deploy","id":"d1"}]}"#;

        let json = events_payload(Some(user), std::slice::from_ref(&start)).unwrap();
        let payload: Events = serde_json::from_str(&json).unwrap();
        assert_eq!(payload.events.len(), 2);
        assert_eq!(payload.events[0].description, "deploy");
        assert_eq!(payload.events[1], start);

        // Without run events the user's value is carried as given.
        assert_eq!(events_payload(Some(user), &[]).as_deref(), Some(user));
        // Without either there is no key.
        assert!(events_payload(None, &[]).is_none());
    }

    /// A user value that does not parse is kept verbatim; the run events are
    /// the ones dropped.
    #[test]
    fn parquet_events_payload_keeps_an_unparseable_user_value() {
        let marker = RunMarker::new(&["bench".to_string()]);
        let start = marker.start_event(TEST_ANCHOR, None);
        let garbage = "not json";
        assert_eq!(
            events_payload(Some(garbage), std::slice::from_ref(&start)).as_deref(),
            Some(garbage)
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

    /// An endpoint whose `/status` carried no epoch adopts the first one a
    /// snapshot shows. Nothing restarted; there was simply nothing to compare.
    #[test]
    fn a_first_epoch_is_adopted_rather_than_reported_as_a_restart() {
        let mut ep = rez_endpoint();
        assert!(ep.agent.producer_epoch.is_none());
        note_epoch_change(&mut ep, &snap_with_epoch(Some("epoch-a")));
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("epoch-a"));
    }

    /// The same epoch twice is the ordinary case and must not move anything.
    #[test]
    fn an_unchanged_epoch_leaves_the_recorded_one_alone() {
        let mut ep = rez_endpoint();
        ep.agent.producer_epoch = Some("epoch-a".to_string());
        note_epoch_change(&mut ep, &snap_with_epoch(Some("epoch-a")));
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("epoch-a"));
    }

    /// A change is a restart: every cumulative counter reset between these two
    /// scrapes. The recorder tracks the new one so it warns once rather than
    /// once per tick for the rest of the recording.
    #[test]
    fn a_changed_epoch_is_tracked_so_the_warning_fires_once() {
        let mut ep = rez_endpoint();
        ep.agent.producer_epoch = Some("epoch-a".to_string());
        note_epoch_change(&mut ep, &snap_with_epoch(Some("epoch-b")));
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("epoch-b"));
        // A second look at the same epoch is not another restart.
        note_epoch_change(&mut ep, &snap_with_epoch(Some("epoch-b")));
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("epoch-b"));
    }

    /// A snapshot from an agent that predates the epoch must not clear one we
    /// already have — absence is not a restart.
    #[test]
    fn a_snapshot_without_an_epoch_does_not_clear_the_known_one() {
        let mut ep = rez_endpoint();
        ep.agent.producer_epoch = Some("epoch-a".to_string());
        note_epoch_change(&mut ep, &snap_with_epoch(None));
        assert_eq!(ep.agent.producer_epoch.as_deref(), Some("epoch-a"));
    }

    /// Same precedence as `source`: what the agent reported is the default,
    /// and an explicit `--metadata version=...` overrides it (relabeling a
    /// capture taken through a proxy, say).
    #[test]
    fn an_explicit_metadata_version_overrides_the_agent_reported_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = rez_config(&dir.path().join("out.rez"));
        config.metadata = vec![("version".to_string(), "custom".to_string())];
        let mut ep = rez_endpoint();
        ep.agent.version = Some("5.19.2".to_string());

        let m = build_rez_metadata(&config, &ep, &[]);
        assert_eq!(
            m.get(parquet_metadata::KEY_VERSION).map(String::as_str),
            Some("custom")
        );
    }

    #[test]
    fn rez_metadata_omits_the_version_when_the_agent_did_not_report_one() {
        // A Prometheus endpoint, or an agent that answers neither `/status`
        // nor `/`. Absent, not empty: an empty string would render as the
        // blank `Rezolus Version:` line this change exists to remove.
        let dir = tempfile::tempdir().unwrap();
        let config = rez_config(&dir.path().join("out.rez"));
        let ep = rez_endpoint();

        let m = build_rez_metadata(&config, &ep, &[]);
        assert!(!m.contains_key(parquet_metadata::KEY_VERSION));
    }

    const TEST_ANCHOR: u64 = 1_700_000_000_000_000_000;
    const TEST_SECOND: u64 = 1_000_000_000;

    fn rez_config(output: &Path) -> RecordingConfig {
        RecordingConfig {
            interval: humantime::Duration::from(Duration::from_secs(1)),
            duration: None,
            format: Format::Rez,
            verbose: 0,
            output: output.to_path_buf(),
            separate: false,
            metadata: Vec::new(),
            labels: vec![("arm".to_string(), "redis".to_string())],
            endpoints: Vec::new(),
            command: None,
            format_defaulted: false,
            record_command_line: false,
        }
    }

    fn rez_endpoint() -> EndpointState {
        EndpointState::new(endpoint::EndpointConfig {
            url: Url::parse("http://localhost:4241").unwrap(),
            source: Some("rezolus".to_string()),
            role: None,
            protocol: Some(Protocol::Msgpack),
        })
    }

    /// The refusal for an agent that cannot stream names the endpoint, the
    /// agent's version when there is one, and the outputs that record such an
    /// agent. Only an agent older than the stream is said to predate it: a
    /// current agent's 404 is quoted as the route's answer.
    #[test]
    fn an_unstreamable_agent_is_refused_by_version_with_the_alternatives() {
        let reason = "http://localhost:4241/metrics/stream?interval=1s returned HTTP 404";
        let mut ep = rez_endpoint();
        ep.agent.version = Some("5.20.0".to_string());
        let msg = unstreamable_agent(&ep, reason, RefusedAt::Startup);
        for needle in [
            "http://localhost:4241/ is Rezolus 5.20.0",
            "predates",
            reason,
            STREAM_SINCE,
            "-o out.rez",
            "-o out.parquet",
        ] {
            assert!(msg.contains(needle), "{needle:?} in {msg}");
        }

        ep.agent.version = Some("5.22.1".to_string());
        let msg = unstreamable_agent(&ep, reason, RefusedAt::Startup);
        assert!(!msg.contains("predates"), "{msg}");
        assert!(
            msg.contains("Rezolus 5.22.1") && msg.contains(reason),
            "{msg}"
        );

        ep.agent.version = None;
        let msg = unstreamable_agent(&ep, reason, RefusedAt::Startup);
        assert!(msg.contains("unknown version"), "{msg}");
        assert!(msg.contains(STREAM_SINCE), "{msg}");

        // Mid-run, only the endpoint is left out: switching the run's format
        // is not the advice.
        ep.agent.version = Some("5.20.0".to_string());
        let msg = unstreamable_agent(&ep, reason, RefusedAt::MidRun);
        assert!(msg.contains("excluded from this recording"), "{msg}");
        assert!(msg.contains("separate"), "{msg}");
        assert!(!msg.contains("-o out.rez"), "{msg}");
    }

    #[test]
    fn predates_stream_compares_the_release() {
        assert_eq!(predates_stream("5.20.0"), Some(true));
        assert_eq!(predates_stream("4.1.2"), Some(true));
        assert_eq!(predates_stream("5.21.0"), Some(false));
        assert_eq!(predates_stream("5.21.0-alpha.1"), Some(false));
        assert_eq!(predates_stream("6.0.0-alpha.14"), Some(false));
        assert_eq!(predates_stream("5.21"), None);
        assert_eq!(predates_stream("dev"), None);
    }

    /// A second endpoint, distinguishable from [`rez_endpoint`] by `source`.
    fn rez_endpoint_b() -> EndpointState {
        EndpointState::new(endpoint::EndpointConfig {
            url: Url::parse("http://localhost:4242").unwrap(),
            source: Some("valkey".to_string()),
            role: None,
            protocol: Some(Protocol::Msgpack),
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
    fn tick(rec: &mut RezStream, endpoint: usize, i: u64) -> Result<(), String> {
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
        // In `.rez` mode there is no parquet writer to catch an unrouted
        // snapshot: `writers` is all `None`. So returning Ok here would decode
        // a scrape and throw it away every tick for the whole run, and the
        // archive would finalize one recording short with exit 0. That was the
        // behaviour when an endpoint down at startup activated later.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let config = rez_config(&path);
        let mut rec = start_rez_recorder(&config, &[(0, &rez_endpoint())], TEST_ANCHOR).unwrap();

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

    /// The recorder keeps the producer's stamp when the agent sends one, and
    /// falls back to its own tick when it does not.
    ///
    /// The fallback is not a nicety: a Prometheus target has no such clock,
    /// and neither does an agent older than these keys, and a recorder that
    /// insisted on one would have nothing to stamp their rows with.
    #[test]
    fn a_producers_stamp_is_used_where_there_is_one_and_not_invented_where_there_is_not() {
        use metriken_exposition::{Snapshot, SnapshotV2};

        let with = |pairs: &[(&str, &str)]| {
            Snapshot::V2(SnapshotV2 {
                systemtime: std::time::SystemTime::UNIX_EPOCH,
                duration: Duration::from_secs(1),
                metadata: pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                counters: Vec::new(),
                gauges: Vec::new(),
                histograms: Vec::new(),
            })
        };

        assert_eq!(
            snapshot_producer_stamp(&with(&[
                ("ts", "1700000000000000000"),
                ("wall_offset", "-42")
            ])),
            Some((1_700_000_000_000_000_000, -42)),
            "both halves come from the producer or neither does"
        );
        assert_eq!(
            snapshot_producer_stamp(&with(&[("ts", "1700000000000000000")])),
            None,
            "a ts with no offset cannot be turned into a wall clock, so it is \
             not half-used"
        );
        assert_eq!(
            snapshot_producer_stamp(&with(&[])),
            None,
            "a source that sends no stamp gets the recorder's"
        );
        assert_eq!(
            snapshot_producer_stamp(&with(&[("ts", "-1"), ("wall_offset", "0")])),
            None,
            "a negative stamp is not a timeline this archive can hold"
        );
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
        let path = dir.path().join("anchored.rez");
        let config = rez_config(&path);

        let mut agent = rez_endpoint();
        agent.agent.clock_anchor_wall_ns = Some(AGENT_ANCHOR);
        // A Prometheus target has no clock of its own to offer, so it keeps
        // the recorder's — one archive, two recordings, each coherent.
        let prom = rez_endpoint_b();

        let mut rec = start_rez_recorder(&config, &[(0, &agent), (1, &prom)], TEST_ANCHOR).unwrap();
        tick(&mut rec, 0, 0).expect("ingest a");
        tick(&mut rec, 1, 0).expect("ingest b");
        rec.finalize((TEST_ANCHOR, 0)).unwrap();

        let db = rez_sqlite::RezDb::open(&path).expect("the archive opens");
        let recordings = db.read_recordings().expect("the manifest reads");
        let anchor_of = |source: &str| -> u64 {
            recordings
                .iter()
                .find(|r| r.meta.labels.get("source").map(String::as_str) == Some(source))
                .unwrap_or_else(|| panic!("a recording for {source}"))
                .meta
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
        let path = dir.path().join("out.rez");
        let config = rez_config(&path);
        let mut rec = start_rez_recorder(&config, &[(0, &rez_endpoint())], TEST_ANCHOR).unwrap();

        tick(&mut rec, 0, 0).expect("the endpoint up at startup records");
        // ... endpoint 1 comes up mid-run.
        rec.add_endpoint(1, &config, &rez_endpoint_b(), TEST_ANCHOR, &[])
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
            metriken_query::BufferPool::new(64 * 1024 * 1024),
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
    fn record_ticks(rec: &mut RezStream, ticks: u64) {
        for i in 0..ticks {
            let ts = TEST_ANCHOR + i * TEST_SECOND;
            let c = rez::recorder_tests_support::counter(
                "fake_ops",
                "fake",
                i,
                Some(::rez::window::Window::new(ts - 500, ts)),
            );
            let snapshot = rez::recorder_tests_support::snap(ts, vec![c]);
            rec.stage(0, &rez_endpoint().config.url, &snapshot, ts, 0)
                .expect("stage");
            rec.commit_tick().expect("commit");
            rec.maybe_seal().expect("seal");
        }
    }

    #[test]
    fn recording_writes_a_sqlite_container_that_round_trips_through_rezreader() {
        // The only container this binary writes. Writing it is half the
        // wiring — the recording has to come back out through the same reader
        // every consumer uses, or the recorder is producing a file nothing can
        // query.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let config = rez_config(&path);
        let mut rec = start_rez_recorder(&config, &[(0, &rez_endpoint())], TEST_ANCHOR).unwrap();

        // Valid and openable before a single row is written — there is no
        // `.partial` standing in for it.
        assert!(path.exists(), "v3 records at the output path itself");
        assert!(!dir.path().join("out.rez.partial").exists());

        record_ticks(&mut rec, 3);
        rec.finalize((TEST_ANCHOR + 3 * TEST_SECOND, 0)).unwrap();

        assert_eq!(
            rez::detect_rez_format(&path).unwrap(),
            rez::RezFormat::V3Sqlite
        );
        use metriken_query::MetricsSource;
        let reader = crate::rez_reader::RezReader::open_with_pool(
            &path,
            metriken_query::BufferPool::new(64 * 1024 * 1024),
        )
        .unwrap();
        assert_eq!(reader.counter_names(), vec!["fake_ops".to_string()]);
        let (start, end) = reader.time_range().unwrap();
        // A bare counter is not an instant vector; `rate()` is how a counter is
        // read, and it is also what consumes the acquisition windows the rows
        // carry.
        let r = reader.query_range("rate(fake_ops[5s])", start, end + 1.0, 1.0);
        let metriken_query::QueryResult::Matrix { result } = r.expect("the query must resolve")
        else {
            panic!("a range query over a counter is a matrix");
        };
        // Values, not merely a successful parse: the counter rises by 1 every
        // second, so the rate is 1/s wherever it is defined.
        let points: Vec<f64> = result
            .iter()
            .flat_map(|s| s.values.iter().map(|(_, v)| *v))
            .collect();
        assert!(!points.is_empty(), "the recorded rows must come back out");
        assert!(
            points.iter().all(|v| (*v - 1.0).abs() < 1e-6),
            "a counter rising 1/s must read back as 1/s: {points:?}"
        );
        // The recording's identity survives: labels the run was tagged with,
        // and the metadata the manifest used to carry.
        assert_eq!(reader.source(), "rezolus");
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
        let config = dendro_config(&path);
        let mut ep = rez_endpoint();
        adopt_source(&mut ep, &source);
        let mut rec = start_rez_recorder(&config, &[(0, &ep)], TEST_ANCHOR).unwrap();

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

        use metriken_archive::Catalog;
        let catalog = metriken_archive::DendroCatalog::open(&path).unwrap();
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
            metriken_query::BufferPool::new(64 * 1024 * 1024),
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
        let config = dendro_config(&path);
        let mut rec = start_rez_recorder(&config, &[(0, &rez_endpoint())], TEST_ANCHOR).unwrap();
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
    /// a new uuid is a restarted agent, tracked the way a changed snapshot
    /// epoch is on the scrape path.
    #[test]
    fn a_handshake_sets_the_anchor_and_epoch_and_a_new_uuid_is_a_restart() {
        let source = |uuid: Option<&str>, anchor: i64| stream::Source {
            uuid: uuid.map(String::from),
            labels: BTreeMap::new(),
            metadata: BTreeMap::new(),
            clock_anchor_wall_ns: anchor,
        };
        let mut ep = rez_endpoint();
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

    fn dendro_config(output: &Path) -> RecordingConfig {
        RecordingConfig {
            format: Format::Dendro,
            ..rez_config(output)
        }
    }

    #[test]
    fn a_dendro_recording_round_trips_through_rezreader() {
        // `-o out.dendro` writes through metriken-archive's writer, and the
        // recording must come back out through the reader every consumer
        // uses, with each endpoint its own recording: one opened at startup,
        // one that activated mid-run.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.dendro");
        let config = dendro_config(&path);
        let mut rec = start_rez_recorder(&config, &[(0, &rez_endpoint())], TEST_ANCHOR).unwrap();
        assert!(path.exists(), "the archive is created at the output path");

        record_ticks(&mut rec, 2);
        rec.add_endpoint(1, &config, &rez_endpoint_b(), TEST_ANCHOR, &[])
            .unwrap();
        for i in 2..5 {
            tick(&mut rec, 0, i).unwrap();
            tick(&mut rec, 1, i).unwrap();
            rec.maybe_seal().unwrap();
        }
        rec.finalize((TEST_ANCHOR + 5 * TEST_SECOND, 0)).unwrap();

        assert!(
            metriken_archive::DendroCatalog::is_archive(&path).unwrap(),
            "a .dendro output is a dendro archive"
        );
        let readers = crate::rez_reader::RezReader::open_recordings(
            &path,
            metriken_query::BufferPool::new(64 * 1024 * 1024),
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
        let config = dendro_config(&path);
        let rec = start_rez_recorder(&config, &[(0, &rez_endpoint())], TEST_ANCHOR).unwrap();
        rec.discard();
        for suffix in ["", "-wal", "-shm"] {
            let p = dir.path().join(format!("out.dendro{suffix}"));
            assert!(!p.exists(), "discard left {} behind", p.display());
        }
        start_rez_recorder(&config, &[(0, &rez_endpoint())], TEST_ANCHOR)
            .unwrap_or_else(|e| panic!("the output path is still claimed: {e}"))
            .discard();
    }

    #[test]
    fn discarding_a_recording_that_captured_nothing_leaves_no_file_behind() {
        // The "no data was recorded" / "failed to start command" paths: there
        // is nothing to recover, and the writer refuses to overwrite an
        // existing output (`RezDb::create` claims the path with O_EXCL), so a
        // stub left behind would block the retry as well as lying about what
        // was captured.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let config = rez_config(&path);
        let rec = start_rez_recorder(&config, &[(0, &rez_endpoint())], TEST_ANCHOR).unwrap();
        rec.discard();
        assert!(
            !path.exists() && !dir.path().join("out.rez.partial").exists(),
            "discard must leave neither the output nor a staging file"
        );
        // And the path is free again, which is the property the retry needs.
        let config = rez_config(&path);
        start_rez_recorder(&config, &[(0, &rez_endpoint())], TEST_ANCHOR)
            .unwrap_or_else(|e| panic!("the output path is still claimed: {e}"))
            .discard();
    }

    #[test]
    fn test_separate_output_path() {
        let base = PathBuf::from("/tmp/recording.parquet");
        assert_eq!(
            separate_output_path(&base, "rezolus"),
            PathBuf::from("/tmp/recording_rezolus.parquet")
        );
    }

    #[test]
    fn test_separate_output_path_no_extension() {
        let base = PathBuf::from("/tmp/recording");
        assert_eq!(
            separate_output_path(&base, "vllm"),
            PathBuf::from("/tmp/recording_vllm")
        );
    }

    #[test]
    fn test_output_dir() {
        assert_eq!(
            output_dir(&PathBuf::from("/tmp/out.parquet")),
            PathBuf::from("/tmp")
        );
        assert_eq!(
            output_dir(&PathBuf::from("out.parquet")),
            PathBuf::from(".")
        );
    }

    #[test]
    fn test_build_per_source_metadata_single_source() {
        let json =
            build_per_source_metadata("rezolus", Some(100), Some(200), Some("service"), None)
                .unwrap();
        let psm: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            psm["rezolus"]["first_sample_ns"].as_u64(),
            Some(100),
            "got: {psm}"
        );
        assert_eq!(psm["rezolus"]["last_sample_ns"].as_u64(), Some(200));
        assert_eq!(psm["rezolus"]["role"].as_str(), Some("service"));
        // Only one source entry
        assert_eq!(psm.as_object().unwrap().len(), 1);
    }

    #[test]
    fn test_build_per_source_metadata_array_source_duplicates_fields() {
        // When the source is a JSON array, one entry per source name is
        // emitted with the same per-source fields duplicated.
        let json = build_per_source_metadata(
            "[\"rezolus\",\"llm-perf\"]",
            Some(100),
            Some(200),
            Some("service"),
            None,
        )
        .unwrap();
        let psm: serde_json::Value = serde_json::from_str(&json).unwrap();
        for name in ["rezolus", "llm-perf"] {
            assert_eq!(
                psm[name]["first_sample_ns"].as_u64(),
                Some(100),
                "missing first_sample_ns for {name}"
            );
            assert_eq!(psm[name]["last_sample_ns"].as_u64(), Some(200));
            assert_eq!(psm[name]["role"].as_str(), Some("service"));
        }
        assert_eq!(psm.as_object().unwrap().len(), 2);
    }

    #[test]
    fn test_build_per_source_metadata_returns_none_when_empty() {
        // No per-source fields at all → no per_source_metadata.
        assert!(build_per_source_metadata("rezolus", None, None, None, None).is_none());
    }

    #[test]
    fn test_build_per_source_metadata_array_with_partial_fields() {
        // Array source with only a subset of per-source fields populated.
        let json =
            build_per_source_metadata("[\"a\",\"b\",\"c\"]", Some(50), None, None, None).unwrap();
        let psm: serde_json::Value = serde_json::from_str(&json).unwrap();
        for name in ["a", "b", "c"] {
            assert_eq!(psm[name]["first_sample_ns"].as_u64(), Some(50));
            assert!(psm[name].get("last_sample_ns").is_none());
            assert!(psm[name].get("role").is_none());
        }
    }

    #[test]
    fn test_build_per_source_metadata_includes_sampler_status() {
        let ss = r#"[{"name":"cpu_usage","state":"active"}]"#;
        let json = build_per_source_metadata("rezolus", None, None, None, Some(ss)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arr = &v["rezolus"]["sampler_status"];
        assert!(arr.is_array());
        assert_eq!(arr[0]["name"], "cpu_usage");
        assert_eq!(arr[0]["state"], "active");
    }
}
