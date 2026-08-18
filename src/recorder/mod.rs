use super::*;

mod child;
mod config;
mod endpoint;
mod prometheus;
pub(crate) mod rez;
pub(crate) mod rez_sqlite;
pub(crate) mod rez_stream;
pub(crate) mod rez_v3_writer;

use crate::parquet_metadata;
pub use config::RecordingConfig;
use endpoint::{infer_source_name, EndpointState, EndpointStatus, Protocol};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub fn command() -> Command {
    Command::new("record")
        .about("On-demand recording of metrics to a file")
        .long_about(
            "Scrape a metrics endpoint at a fixed interval and write the samples to a file.\n\n\
             The source is auto-detected: a Rezolus agent (msgpack) or a Prometheus-compatible\n\
             endpoint.\n\n\
             WHAT TO RECORD (choose one): --url for a single endpoint (default\n\
             http://localhost:4241), --endpoint (repeatable) for several at once, or --config\n\
             for a TOML file. Write the result with -o/--output (default rezolus.parquet).\n\
             (The positional URL and OUTPUT still work but are deprecated; prefer --url / -o.)\n\n\
             HOW LONG TO RECORD (choose one): --duration for a fixed window, nothing to run\n\
             until Ctrl-C, or `-- <command>` to record for exactly the lifetime of a wrapped\n\
             command (perf-record style) — it stops when the command exits.\n\n\
             EXAMPLES:\n    \
             # Record a local agent for 5 minutes\n    \
             rezolus record --url http://localhost:4241 -o out.parquet --duration 5m\n\n    \
             # Record a Prometheus endpoint, tagging the source in the file metadata\n    \
             rezolus record --url http://host:9090/metrics -o out.parquet --metadata source=llm-perf\n\n    \
             # Record only while a benchmark runs, then stop (defaults: localhost:4241 -> rezolus.parquet)\n    \
             rezolus record -- ./bench.sh --iters 100\n\n    \
             # Same, writing to a named file\n    \
             rezolus record -o bench.parquet -- ./bench.sh\n\n    \
             # High-resolution capture: sample every 100ms for 30 seconds\n    \
             rezolus record --url http://localhost:4241 -o out.parquet --interval 100ms --duration 30s\n\n    \
             # Record several endpoints into separate per-endpoint files\n    \
             rezolus record --separate --endpoint http://localhost:4241 --endpoint http://svc:9090/metrics,source=svc -o combined.parquet\n\n    \
             # Write a per-sampler .rez archive (each sampler at its own cadence, with\n    \
             # per-metric acquisition windows), tagging the recording with labels\n    \
             rezolus record --url http://localhost:4241 -o out.rez --label arm=redis --label host=node1\n\
             \n\
             The .rez format (chosen by a .rez output extension or --format rez) holds one\n\
             parquet table per sampler, each at its own cadence. It requires a\n\
             rezolus/msgpack endpoint (not Prometheus). --label k=v (repeatable) tags the\n\
             recording; source and host are auto-populated.\n\n\
             .rez recordings are written to disk as they run, so stopping costs the same\n\
             whether the recording ran for a minute or a day. Ctrl-c and SIGTERM (e.g. a\n\
             docker stop) are clean stops: the signal interrupts the wait between samples\n\
             straight away, so finalizing costs only the write of the still-open segments —\n\
             at any --interval, comfortably inside a container's stop grace, and never\n\
             proportional to the recording's length.\n\n\
             A .rez is a single SQLite file, valid at every instant. There is no .partial,\n\
             so the output path must not already exist, and every sample is committed as it\n\
             is taken: a SIGKILL or a power loss costs at most one sampling interval, for\n\
             every sampler. `rezolus parquet metadata -i out.rez` reports an interrupted\n\
             recording as \"not cleanly finalized\" and how many samples are still in its\n\
             write-ahead log. Pass --rez-version 2 to write the previous tar container\n\
             instead (staged at <output>.partial, recoverable only to its last checkpoint).",
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
                .help("Write one parquet file per endpoint instead of combining; each is named <OUTPUT-stem>_<source>.<ext> alongside the output path (source falls back to host-port, e.g. localhost-4241, when not set via source=)")
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
                .help("Output format: parquet (columnar, queryable), raw (concatenated msgpack snapshots), or rez (per-sampler .rez archive; also selected by a .rez output extension, requires a rezolus/msgpack endpoint)")
                .action(clap::ArgAction::Set)
                .default_value("parquet")
                .value_parser(value_parser!(Format)),
        )
        .arg(
            clap::Arg::new("REZ_VERSION")
                .long("rez-version")
                .help("Container version for .rez output: 3 (default, a single SQLite file) or 2 (the legacy tar archive, kept for a release or two)")
                .action(clap::ArgAction::Set)
                .default_value("3")
                .value_parser(value_parser!(u8).range(2..=3)),
        )
        .arg(
            clap::Arg::new("METADATA")
                .long("metadata")
                .short('m')
                .help("Add a file-level metadata tag as key=value (e.g. source=llm-perf); repeat for multiple tags")
                .action(clap::ArgAction::Append),
        )
        .arg(
            clap::Arg::new("LABEL")
                .long("label")
                .short('l')
                .help("Tag the recording with a label as key=value (e.g. arm=redis, role=server); repeat for multiple. A value without `=` is ignored. `source` and `host` are auto-populated. Used by .rez output.")
                .action(clap::ArgAction::Append),
        )
        .arg(
            clap::Arg::new("NODE")
                .long("node")
                .help("Node name for rezolus agent data (written to parquet metadata)")
                .action(clap::ArgAction::Set),
        )
        .arg(
            clap::Arg::new("INSTANCE")
                .long("instance")
                .help("Instance name for service data (written to parquet metadata)")
                .action(clap::ArgAction::Set),
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
                .help("Path to the output file (default rezolus.parquet)")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(PathBuf))
                .conflicts_with("OUTPUT"),
        )
        .arg(
            clap::Arg::new("COMMAND")
                .help("Wrap a command: record only while it runs, then stop when it exits. Give it after `--`, e.g. rezolus record -o out.parquet -- ./bench.sh --iters 100")
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
                if rmp_serde::from_slice::<metriken_exposition::Snapshot>(&body).is_ok() {
                    return Some((Protocol::Msgpack, candidate_url.clone()));
                }
            }
        }
    }
    None
}

/// Fetch systeminfo, descriptions, and sampler status from a Rezolus agent.
async fn fetch_agent_metadata(
    client: &Client,
    base_url: &Url,
) -> (Option<String>, Option<String>, Option<String>) {
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

    (systeminfo, descriptions, sampler_status)
}

/// `sleep_until(deadline)` when there is one, otherwise a future that never
/// resolves — the `tokio::select!` arm shape for an optional deadline.
async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d.into()).await,
        None => std::future::pending().await,
    }
}

async fn scrape_one(client: &Client, url: &Url) -> Result<Vec<u8>, String> {
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| format!("{e}"))?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("{e}"))
}

fn inject_provenance(
    mut snapshot: metriken_exposition::Snapshot,
    source: &str,
    endpoint_url: &str,
) -> metriken_exposition::Snapshot {
    fn inject_metrics(
        counters: &mut [metriken_exposition::Counter],
        gauges: &mut [metriken_exposition::Gauge],
        histograms: &mut [metriken_exposition::Histogram],
        source: &str,
        endpoint_url: &str,
    ) {
        for counter in counters.iter_mut() {
            counter
                .metadata
                .insert("source".to_string(), source.to_string());
            counter
                .metadata
                .insert("endpoint".to_string(), endpoint_url.to_string());
        }
        for gauge in gauges.iter_mut() {
            gauge
                .metadata
                .insert("source".to_string(), source.to_string());
            gauge
                .metadata
                .insert("endpoint".to_string(), endpoint_url.to_string());
        }
        for histogram in histograms.iter_mut() {
            histogram
                .metadata
                .insert("source".to_string(), source.to_string());
            histogram
                .metadata
                .insert("endpoint".to_string(), endpoint_url.to_string());
        }
    }

    match &mut snapshot {
        metriken_exposition::Snapshot::V2(ref mut v2) => {
            inject_metrics(
                &mut v2.counters,
                &mut v2.gauges,
                &mut v2.histograms,
                source,
                endpoint_url,
            );
        }
        metriken_exposition::Snapshot::V1(ref mut v1) => {
            inject_metrics(
                &mut v1.counters,
                &mut v1.gauges,
                &mut v1.histograms,
                source,
                endpoint_url,
            );
        }
        metriken_exposition::Snapshot::V3(_) => {
            // Native V3 ingest (schema-hash caching, per-group WAL) lands in a
            // later stage. A V3 payload here means the agent was flipped to
            // snapshot_format = "v3" ahead of this recorder build. V3's
            // grouped shape has no flat `counters`/`gauges`/`histograms` to
            // inject provenance into directly, so leave the snapshot
            // untouched: the `.rez` ingest path drops it in
            // `group_by_sampler` (warns there), and the raw/parquet
            // passthrough path re-serializes it as-is for the external
            // msgpack-to-parquet converter, which expands V3 via accessors.
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                warn!(
                    "received a SnapshotV3 payload; this build records v2 only — \
                     set [general] snapshot_format = \"v2\" on the agent"
                );
            }
        }
    }
    snapshot
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
) -> MsgpackToParquet {
    let mut converter = MsgpackToParquet::with_options(
        ParquetOptions::new().max_batch_size(parquet_metadata::MAX_ROW_GROUP_SIZE),
    )
    .metadata(
        "sampling_interval_ms".to_string(),
        config.interval.as_millis().to_string(),
    );

    converter = converter.metadata("source".to_string(), ep.config.source_label().to_string());

    for (key, value) in &config.metadata {
        converter = converter.metadata(key.clone(), value.clone());
    }

    if let Some(ref json) = ep.systeminfo {
        converter = converter.metadata("systeminfo".to_string(), json.clone());
    }

    // Descriptions: prefer agent-fetched, fall back to Prometheus HELP
    let prom_desc = prom_converter
        .as_ref()
        .filter(|c| !c.descriptions().is_empty())
        .and_then(|c| serde_json::to_string(c.descriptions()).ok());
    let desc = ep.descriptions.clone().or(prom_desc);
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
        ep.sampler_status.as_deref(),
    ) {
        converter = converter.metadata("per_source_metadata".to_string(), json);
    }

    converter
}

/// File-level metadata for a `.rez` archive manifest, mirroring the keys
/// `build_parquet_converter` writes (`sampling_interval_ms`, `source`,
/// user `--metadata`, `systeminfo`, `descriptions`).
fn build_rez_metadata(
    config: &RecordingConfig,
    ep: &EndpointState,
) -> std::collections::BTreeMap<String, String> {
    let mut m = std::collections::BTreeMap::new();
    m.insert(
        "sampling_interval_ms".to_string(),
        config.interval.as_millis().to_string(),
    );
    m.insert("source".to_string(), ep.config.source_label().to_string());
    for (k, v) in &config.metadata {
        m.insert(k.clone(), v.clone());
    }
    if let Some(ref json) = ep.systeminfo {
        m.insert("systeminfo".to_string(), json.clone());
    }
    if let Some(ref json) = ep.descriptions {
        m.insert("descriptions".to_string(), json.clone());
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
        ep.systeminfo.as_deref(),
        &config.labels,
    )
}

/// The streaming `.rez` recorder the loop feeds, in whichever container the run
/// selected (`--rez-version`).
///
/// The recording loop is identical for both and must stay that way: one clock
/// anchor, a row per tick stamped on the anchored monotonic timeline, a
/// `maybe_seal` every tick whether or not anything was scraped, and one
/// finalize. The container is the only thing that varies, so it varies here and
/// nowhere else.
enum RezStream {
    /// v2: a tar archive staged at `<output>.partial`, renamed into place at
    /// finalize, recoverable up to its last checkpoint.
    V2(rez_stream::StreamRecorder),
    /// v3: a single SQLite file, a valid `.rez` at `<output>` from the moment
    /// it is created. There is no staging path to unlink and no rename to fail,
    /// so a v3 recorder that is dropped without finalizing leaves a valid,
    /// recoverable recording whose `complete` flag is simply still 0.
    V3(rez_v3_writer::StreamRecorderV3),
}

impl RezStream {
    /// Append one scraped snapshot.
    ///
    /// Fallible in v3 and not in v2, and the difference is real: v3 writes the
    /// tick to the WAL here, where v2 only appended to in-memory builders and
    /// could not fail until a seal.
    fn ingest(
        &mut self,
        snapshot: &metriken_exposition::Snapshot,
        anchored_ts: u64,
        wall_offset_ns: i64,
    ) -> Result<(), String> {
        match self {
            RezStream::V2(rec) => {
                rec.ingest(snapshot, anchored_ts, wall_offset_ns);
                Ok(())
            }
            RezStream::V3(rec) => rec.ingest(snapshot, anchored_ts, wall_offset_ns),
        }
    }

    fn maybe_seal(&mut self) -> Result<(), String> {
        match self {
            RezStream::V2(rec) => rec.maybe_seal(),
            RezStream::V3(rec) => rec.maybe_seal(),
        }
    }

    fn finalize(self, clock_offset: (u64, i64)) -> Result<(), String> {
        match self {
            RezStream::V2(rec) => rec.finalize(clock_offset),
            RezStream::V3(rec) => rec.finalize(clock_offset),
        }
    }

    /// What to tell the user after a mid-recording failure: where the data
    /// captured so far can still be read from. The two containers keep it in
    /// different places, and v3's is the output path itself.
    fn recovery_note(&self) -> String {
        match self {
            RezStream::V2(rec) => format!(
                "note: the partial recording is readable at {}",
                rec.partial_path().display()
            ),
            RezStream::V3(rec) => format!(
                "note: the recording so far is readable at {}",
                rec.path().display()
            ),
        }
    }

    /// Stop the writer and leave nothing behind. Only for the paths where the
    /// recording captured no samples at all — a stub is not a recovery artifact,
    /// and both containers refuse to overwrite, so leaving one behind would also
    /// block the retry.
    fn discard(self) {
        match self {
            // Unlinks `<output>.partial`; the previous run's output, which v2
            // never touched, stays where it was.
            RezStream::V2(rec) => rec.abort(),
            RezStream::V3(rec) => {
                let path = rec.path().to_path_buf();
                // Drop first: joining the writer thread is what guarantees
                // nothing is still appending to the file we are about to
                // unlink. v3 has no abort — a dropped writer leaves a valid
                // recording, which is exactly why this path has to remove it
                // explicitly rather than rely on a staging convention.
                drop(rec);
                // Safe to remove: `RezDb::create` claimed this path with
                // O_EXCL during THIS run, so it cannot be a file that was
                // already there.
                if let Err(e) = std::fs::remove_file(&path) {
                    warn!(
                        "failed to remove the empty recording at {}: {e}",
                        path.display()
                    );
                }
            }
        }
    }
}

/// Open the streaming `.rez` writer for a just-activated endpoint and spawn its
/// writer thread. v3 creates the output file itself; v2 creates
/// `<output>.partial` and writes an initial manifest.
///
/// Both the file creation and the thread spawn can fail, which is why this
/// happens at activation rather than lazily on the first snapshot.
fn start_rez_recorder(
    config: &RecordingConfig,
    ep: &EndpointState,
    clock_anchor_wall_ns: u64,
) -> Result<RezStream, String> {
    let labels = build_rez_labels(config, ep);
    let metadata = build_rez_metadata(config, ep);
    // Clap constrains `--rez-version` to 2 or 3, so 2 is the only opt-out and
    // everything else is v3.
    if config.rez_version == 2 {
        let seed = rez_stream::ManifestSeed {
            dir: rez::recording_dir_slug(&labels),
            labels,
            metadata,
            clock_anchor_wall_ns,
        };
        return rez_stream::RezWriterHandle::create(&config.output, seed)
            .map(|h| RezStream::V2(rez_stream::StreamRecorder::new(h)));
    }
    let seed = rez_v3_writer::ManifestSeed {
        labels,
        metadata,
        clock_anchor_wall_ns,
    };
    rez_v3_writer::RezV3Writer::create(&config.output, seed)
        .map(|w| RezStream::V3(rez_v3_writer::StreamRecorderV3::new(w)))
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
    let anchored_ns = anchor_wall_ns.saturating_add(elapsed.as_nanos() as u64);
    (anchored_ns, wall_ns as i64 - anchored_ns as i64)
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
    converter: Option<prometheus::PrometheusConverter>,
}

/// Upper bound on a single scrape or endpoint probe, whatever the interval.
/// The bound exists to keep the tick responsive (age seals, ctrl-c), and that
/// is a human/container-teardown timescale — a long `--interval` must not buy a
/// correspondingly long stall.
const MAX_SCRAPE_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs the Rezolus `recorder` which pulls metrics from one or more endpoints
/// and writes them to parquet file(s). Supports Rezolus msgpack and Prometheus
/// text format endpoints, with auto-detection.
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

    let out_dir = output_dir(&config.output);

    let mut endpoints: Vec<EndpointState> = config
        .endpoints
        .iter()
        .map(|ep| EndpointState::new(ep.clone()))
        .collect();

    // `.rez` per-sampler archive mode: selected by a `.rez` output extension or
    // `--format rez`.
    let rez_mode = rez::wants_rez(&config.output, config.format);

    if rez_mode {
        // `.rez` ingest reads msgpack snapshots; an explicitly-prometheus
        // endpoint yields none, so reject it up front (before probing) rather
        // than recording an empty archive. Auto-detected prometheus is rejected
        // right after the probe, below.
        if let Some(ep) = endpoints
            .iter()
            .find(|e| matches!(e.config.protocol, Some(Protocol::Prometheus)))
        {
            eprintln!(
                "error: .rez output requires a rezolus (msgpack) endpoint; {} is configured protocol=prometheus",
                ep.config.url
            );
            return;
        }
        // Multi-source/A-B `.rez` is deferred; require one endpoint. Checked
        // before probing so a misconfiguration costs no network round-trips.
        if endpoints.len() > 1 {
            eprintln!("error: .rez output currently supports a single endpoint");
            return;
        }
    }

    // Probe all endpoints (best-effort startup)
    rt.block_on(async {
        for ep in &mut endpoints {
            match probe_endpoint(&client, &ep.config).await {
                Some((protocol, url)) => {
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
                    info!(
                        "endpoint {} ({}): detected {:?}",
                        ep.config.source_label(),
                        ep.config.url,
                        protocol
                    );
                    if protocol == Protocol::Msgpack {
                        let (si, desc, ss) = fetch_agent_metadata(&client, &ep.config.url).await;
                        ep.systeminfo = si;
                        ep.descriptions = desc;
                        ep.sampler_status = ss;
                    }
                    ep.scrape_url = Some(url);
                    ep.detected_protocol = Some(protocol);
                    ep.status = EndpointStatus::Active;
                }
                None => {
                    warn!(
                        "endpoint {} not reachable, will retry each tick",
                        ep.config.url
                    );
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

    // The streaming `.rez` writer is opened here — when the (single) endpoint
    // becomes active, not lazily on the first snapshot — because creating the
    // output (or, in v2, the `<output>.partial`) and spawning the writer thread
    // are both fallible.
    let mut rez_recorder: Option<RezStream> = None;
    if rez_mode {
        // `.rez` is single-endpoint (guarded above) and at least one endpoint is
        // active (checked above), so the active endpoint is *the* endpoint. It
        // can therefore never activate later via the Pending path.
        if let Some(ep) = endpoints
            .iter()
            .find(|ep| ep.status == EndpointStatus::Active)
        {
            if ep.protocol() != Some(&Protocol::Msgpack) {
                eprintln!(
                    "error: .rez output requires a rezolus (msgpack) endpoint; {} answered as prometheus",
                    ep.config.url
                );
                return;
            }
            match start_rez_recorder(&config, ep, clock_anchor_wall_ns) {
                Ok(rec) => rez_recorder = Some(rec),
                Err(e) => {
                    eprintln!("error: failed to start the .rez recording: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

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
                let converter = if ep.protocol() == Some(&Protocol::Prometheus) {
                    Some(prometheus::PrometheusConverter::with_provenance(
                        ep.config.source_label().to_string(),
                        ep.config.url.to_string(),
                    ))
                } else {
                    None
                };
                Some(EndpointWriter { writer, converter })
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

        let interval_dur: Duration = config.interval.into();
        let start = Instant::now() + interval_dur;
        // In wrapped mode the cap is intentionally measured from command spawn
        // (`Instant::now()`), which differs from the non-wrapped path's `start`
        // reference (now + interval). Do not unify these — they are distinct by
        // design: the cap bounds the child's lifetime, `start` bounds recording.
        let cap_deadline: Option<Instant> =
            config.duration.map(|d| Instant::now() + Duration::from(d));
        let mut interval = crate::common::aligned_interval(interval_dur);
        // How long a single scrape or probe may take before the tick gives up on
        // it. Without a bound a *hung* endpoint (stalled server, SYN blackhole)
        // parks `join_all` for TCP-timeout scales, and the tick is what drives
        // `.rez` age seals and the ctrl-c check (`STATE` is only re-checked at
        // the loop top) — so one hung endpoint would stall durability and
        // shutdown, not just this sample.
        //
        // Deliberately generous rather than exactly one interval: this must
        // catch a *hung* endpoint, not a merely slow one. A local agent already
        // takes ~75 ms to answer, so at `--interval 5ms` a one-interval bound
        // would time out every single scrape and record nothing at all, where
        // the honest outcome is sampling at the endpoint's pace. Floored so
        // short intervals stay recordable, capped so a long interval still
        // hands back a bounded tick.
        let scrape_timeout = (interval_dur * 2).clamp(Duration::from_secs(2), MAX_SCRAPE_TIMEOUT);
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
                            child = None;
                            break;
                        }
                        Ok(None) => {}
                        Err(e) => {
                            warn!("failed to poll command: {e}");
                        }
                    }
                    if let Some(deadline) = cap_deadline {
                        if Instant::now() >= deadline {
                            info!("--duration reached, stopping command");
                            if let Some(mut c) = child.take() {
                                child::terminate(&mut c, child::TERM_GRACE).await;
                            }
                            outcome = Some(child::Outcome::Capped);
                            break;
                        }
                    }
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
            tokio::select! {
                biased;
                _ = shutdown.notified() => continue,
                _ = interval.tick() => {}
                _ = sleep_until_opt(loop_deadline), if !deadline_fired => {
                    // Latched so a deadline already in the past cannot spin the
                    // loop if the top declines to stop for some reason.
                    deadline_fired = true;
                    continue;
                }
            }
            // Both clocks, every tick. `wall_ns` is the raw reading every
            // non-`.rez` consumer has always used (endpoint success stamps, the
            // prometheus converter, the msgpack spool); `.rez` rows are stamped
            // on the anchored monotonic timeline and carry the difference as a
            // per-row observation instead.
            let wall_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            let (anchored_ns, wall_offset_ns) =
                anchored_stamp(clock_anchor_wall_ns, clock_anchor_mono.elapsed(), wall_ns);
            last_clock = (anchored_ns, wall_offset_ns);

            // A `.rez` ingest failure for this tick. Held rather than acted on
            // where it happens: that is inside the per-endpoint result loop,
            // whose `break` would only leave that loop. Surfaced below,
            // alongside `maybe_seal`'s — the same class of failure at the same
            // cadence, and the earlier and more specific of the two wins.
            let mut ingest_failed: Option<String> = None;

            // Scrape all active endpoints concurrently
            let active_indices: Vec<usize> = endpoints
                .iter()
                .enumerate()
                .filter(|(_, ep)| ep.status == EndpointStatus::Active)
                .map(|(i, _)| i)
                .collect();

            let scrape_futures: Vec<_> = active_indices
                .iter()
                .map(|&idx| {
                    let client = client.clone();
                    let url = endpoints[idx].scrape_url.clone().unwrap();
                    async move {
                        let result =
                            match tokio::time::timeout(scrape_timeout, scrape_one(&client, &url))
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
                    Ok(body) => {
                        endpoints[idx].record_success(wall_ns);

                        // `.rez`: decode once and hand the snapshot straight to
                        // the streaming writer. No spool, no re-serialization.
                        if rez_mode {
                            match rmp_serde::from_slice::<metriken_exposition::Snapshot>(&body) {
                                Ok(snapshot) => {
                                    let snapshot = inject_provenance(
                                        snapshot,
                                        endpoints[idx].config.source_label(),
                                        endpoints[idx].config.url.as_str(),
                                    );
                                    if let Some(rec) = rez_recorder.as_mut() {
                                        if let Err(e) =
                                            rec.ingest(&snapshot, anchored_ns, wall_offset_ns)
                                        {
                                            ingest_failed.get_or_insert(e);
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "msgpack decode error for {}: {e}",
                                        endpoints[idx].config.source_label()
                                    );
                                }
                            }
                            continue;
                        }

                        if let Some(ref mut ew) = writers[idx] {
                            let bytes = if let Some(ref mut conv) = ew.converter {
                                // Prometheus: parse text → snapshot → msgpack
                                let text = String::from_utf8_lossy(&body);
                                let snapshot = conv.convert(&text, wall_ns);
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
                                // Msgpack: deserialize, inject provenance, re-serialize
                                match rmp_serde::from_slice::<metriken_exposition::Snapshot>(&body)
                                {
                                    Ok(snapshot) => {
                                        let snapshot = inject_provenance(
                                            snapshot,
                                            endpoints[idx].config.source_label(),
                                            endpoints[idx].config.url.as_str(),
                                        );
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

            let pending_indices: Vec<usize> = endpoints
                .iter()
                .enumerate()
                .filter(|(_, ep)| ep.status == EndpointStatus::Pending)
                .map(|(i, _)| i)
                .collect();

            for idx in pending_indices {
                // Bounded like the scrapes above, and for the same reason: the
                // probe runs on the loop, so a hung endpoint here would stall
                // the tick. A timeout is just a failed probe — retried next tick.
                let probed = match tokio::time::timeout(
                    scrape_timeout,
                    probe_endpoint(&client, &endpoints[idx].config),
                )
                .await
                {
                    Ok(probed) => probed,
                    Err(_) => {
                        warn!(
                            "probe of {} timed out after {}",
                            endpoints[idx].config.url,
                            humantime::format_duration(scrape_timeout)
                        );
                        None
                    }
                };
                if let Some((protocol, url)) = probed {
                    if endpoints[idx].config.source.is_none() {
                        if protocol == Protocol::Msgpack {
                            endpoints[idx].config.source = Some("rezolus".to_string());
                        } else {
                            let inferred = infer_source_name(&endpoints[idx].config.url);
                            eprintln!(
                                "warn: no source name specified for {}, using \"{inferred}\" \
                                 (pass --metadata source=NAME to override)",
                                endpoints[idx].config.url,
                            );
                            endpoints[idx].config.source = Some(inferred);
                        }
                    }
                    info!(
                        "endpoint {} ({}) now available, starting capture",
                        endpoints[idx].config.source_label(),
                        endpoints[idx].config.url
                    );
                    if protocol == Protocol::Msgpack {
                        let (si, desc, ss) =
                            fetch_agent_metadata(&client, &endpoints[idx].config.url).await;
                        endpoints[idx].systeminfo = si;
                        endpoints[idx].descriptions = desc;
                        endpoints[idx].sampler_status = ss;
                    }
                    endpoints[idx].scrape_url = Some(url);
                    endpoints[idx].detected_protocol = Some(protocol.clone());
                    endpoints[idx].status = EndpointStatus::Active;

                    // `.rez` never reaches here: it is single-endpoint and that
                    // endpoint must already be Active (startup exits otherwise),
                    // and an Active endpoint never returns to Pending. It also
                    // has no spool, so there is nothing to create.
                    if !rez_mode {
                        let converter = if protocol == Protocol::Prometheus {
                            Some(prometheus::PrometheusConverter::with_provenance(
                                endpoints[idx].config.source_label().to_string(),
                                endpoints[idx].config.url.to_string(),
                            ))
                        } else {
                            None
                        };
                        writers[idx] = Some(EndpointWriter {
                            writer: tempfile_in(out_dir.clone())
                                .expect("failed to create temp file"),
                            converter,
                        });
                    }
                }
            }

            // Seal checks run every tick, scrape or not: if they were
            // ingest-driven an unreachable endpoint would leave its pre-outage
            // rows unsealed forever and the age bound would stop bounding the
            // kill-loss window.
            let sealed = match rez_recorder.as_mut() {
                Some(rec) => rec.maybe_seal(),
                None => Ok(()),
            };
            if let Some(e) = ingest_failed.or_else(|| sealed.err()) {
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
        }

        // If the loop ended via ctrl-c (STATE flip) while the wrapped command
        // is still alive, terminate and reap it so we never orphan the child.
        if let Some(mut c) = child.take() {
            let status = child::terminate(&mut c, child::TERM_GRACE).await;
            if outcome.is_none() {
                outcome = Some(child::Outcome::Exited(child::map_exit_code(status)));
            }
        }

        // ── Finalization ──────────────────────────────────────────────────

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
            if let Some(rec) = rez_recorder.take() {
                if let Err(e) = rec.finalize(last_clock) {
                    // Must flip `recording_failed`: without it a failed tail
                    // seal / manifest write / rename (ENOSPC at the end of a
                    // long capture, a rename EACCES) exits 0, and neither
                    // container overwrites a pre-existing output — v2 because
                    // it stages at `.partial`, v3 because `create` refuses an
                    // existing file — so the PREVIOUS run's `out.rez` is still
                    // sitting there for the next command in the pipeline to
                    // analyze.
                    eprintln!("error saving .rez archive: {e}");
                    recording_failed.store(true, Ordering::SeqCst);
                } else {
                    info!("wrote .rez archive to {}", config.output.display());
                }
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
                                    &ew.converter,
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
                                    &ew.converter,
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
                                        &ew.converter,
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
            Format::Rez => {
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
        assert_eq!(config.output, PathBuf::from("rezolus.parquet"));
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

    const TEST_ANCHOR: u64 = 1_700_000_000_000_000_000;
    const TEST_SECOND: u64 = 1_000_000_000;

    fn rez_config(output: &Path, rez_version: u8) -> RecordingConfig {
        RecordingConfig {
            interval: humantime::Duration::from(Duration::from_secs(1)),
            duration: None,
            format: Format::Rez,
            verbose: 0,
            output: output.to_path_buf(),
            separate: false,
            metadata: Vec::new(),
            labels: vec![("arm".to_string(), "redis".to_string())],
            rez_version,
            endpoints: Vec::new(),
            command: None,
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

    /// Drive a recorder the way the loop does — ingest, `maybe_seal` every
    /// tick, then finalize — over `ticks` one-second samples of one counter.
    fn record_ticks(rec: &mut RezStream, ticks: u64) {
        for i in 0..ticks {
            let ts = TEST_ANCHOR + i * TEST_SECOND;
            let c = rez::recorder_tests_support::counter(
                "fake_ops",
                "fake",
                i,
                Some(metriken::Window::new(ts - 500, ts)),
            );
            let snapshot = rez::recorder_tests_support::snap(ts, vec![c]);
            rec.ingest(&snapshot, ts, 0).expect("ingest");
            rec.maybe_seal().expect("seal");
        }
    }

    #[test]
    fn rez_version_3_writes_a_sqlite_container_that_round_trips_through_rezreader() {
        // The default. Writing it is only half the wiring — the recording has
        // to come back out through the same reader every consumer uses, or the
        // recorder is producing a file nothing can query.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let config = rez_config(&path, 3);
        let mut rec = start_rez_recorder(&config, &rez_endpoint(), TEST_ANCHOR).unwrap();

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

    #[test]
    fn rez_version_2_still_writes_the_tar_container() {
        // The opt-out has to keep working for a release or two, `.partial`
        // staging and all — this is what makes the flag more than a no-op.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.rez");
        let config = rez_config(&path, 2);
        let mut rec = start_rez_recorder(&config, &rez_endpoint(), TEST_ANCHOR).unwrap();

        assert!(
            dir.path().join("out.rez.partial").exists(),
            "v2 stages at <output>.partial"
        );
        assert!(
            !path.exists(),
            "and does not touch the output until finalize"
        );

        record_ticks(&mut rec, 3);
        rec.finalize((TEST_ANCHOR + 3 * TEST_SECOND, 0)).unwrap();

        assert_eq!(
            rez::detect_rez_format(&path).unwrap(),
            rez::RezFormat::V2Tar
        );
    }

    #[test]
    fn discarding_a_recording_that_captured_nothing_leaves_no_file_behind() {
        // The "no data was recorded" / "failed to start command" paths: there
        // is nothing to recover, and both containers refuse to overwrite an
        // existing output — v2 by staging, v3 because `RezDb::create` claims
        // the path with O_EXCL — so a stub left behind would block the retry
        // as well as lying about what was captured.
        let dir = tempfile::tempdir().unwrap();
        for version in [2u8, 3] {
            let path = dir.path().join(format!("v{version}.rez"));
            let config = rez_config(&path, version);
            let rec = start_rez_recorder(&config, &rez_endpoint(), TEST_ANCHOR).unwrap();
            rec.discard();
            assert!(
                !path.exists() && !dir.path().join(format!("v{version}.rez.partial")).exists(),
                "v{version}: discard must leave neither the output nor a staging file"
            );
            // And the path is free again, which is the property the retry needs.
            let config = rez_config(&path, version);
            start_rez_recorder(&config, &rez_endpoint(), TEST_ANCHOR)
                .unwrap_or_else(|e| panic!("v{version}: the output path is still claimed: {e}"))
                .discard();
        }
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
