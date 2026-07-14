use super::*;

mod child;
mod config;
mod endpoint;
mod prometheus;
mod rez;

use crate::parquet_metadata;
pub use config::RecordingConfig;
use endpoint::{infer_source_name, EndpointState, EndpointStatus, Protocol};
use std::path::Path;

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
             rezolus record --separate --endpoint http://localhost:4241 --endpoint http://svc:9090/metrics,source=svc -o combined.parquet",
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
                .help("Output format: parquet (columnar, queryable) or raw (concatenated msgpack snapshots)")
                .action(clap::ArgAction::Set)
                .default_value("parquet")
                .value_parser(value_parser!(Format)),
        )
        .arg(
            clap::Arg::new("METADATA")
                .long("metadata")
                .short('m')
                .help("Add a file-level metadata tag as key=value (e.g. source=llm-perf); repeat for multiple tags")
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
    let mut converter = MsgpackToParquet::with_options(ParquetOptions::new()).metadata(
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

    ctrlc::set_handler(move || {
        let state = STATE.load(Ordering::SeqCst);
        println!();
        if state == RUNNING {
            info!("finalizing recording... ctrl+c to terminate early");
            STATE.store(TERMINATING, Ordering::SeqCst);
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

    let mut writers: Vec<Option<EndpointWriter>> = endpoints
        .iter()
        .map(|ep| {
            if ep.status == EndpointStatus::Active {
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

    // `.rez` per-sampler archive mode: selected by a `.rez` extension or
    // `--format rez`. Multi-source/A-B `.rez` is deferred; require one endpoint.
    let rez_mode = rez::wants_rez(&config.output, config.format);
    if rez_mode && endpoints.len() > 1 {
        eprintln!("error: .rez output currently supports a single endpoint");
        return;
    }
    let mut rez_recorder: Option<rez::RezRecorder> = None;

    let outcome: Option<child::Outcome> = rt.block_on(async {
        // Spawn the wrapped command only after probing/writers succeeded, so a
        // failed setup never starts an expensive workload.
        let mut child = if let Some(ref cmd) = config.command {
            match child::spawn(cmd) {
                Ok(c) => Some(c),
                Err(e) => {
                    eprintln!("error: failed to start command: {e}");
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

            interval.tick().await;
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;

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
                    async move { (idx, scrape_one(&client, &url).await) }
                })
                .collect();

            let results = futures::future::join_all(scrape_futures).await;

            for (idx, result) in results {
                match result {
                    Ok(body) => {
                        endpoints[idx].record_success(now_ns);
                        if let Some(ref mut ew) = writers[idx] {
                            let bytes = if let Some(ref mut conv) = ew.converter {
                                // Prometheus: parse text → snapshot → msgpack
                                let text = String::from_utf8_lossy(&body);
                                let snapshot = conv.convert(&text, now_ns);
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
                                        if rez_mode {
                                            let rec = rez_recorder.get_or_insert_with(|| {
                                                rez::RezRecorder::new(build_rez_metadata(
                                                    &config,
                                                    &endpoints[idx],
                                                ))
                                            });
                                            rec.ingest(&snapshot, now_ns);
                                        }
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
                if let Some((protocol, url)) = probe_endpoint(&client, &endpoints[idx].config).await
                {
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

                    let converter = if protocol == Protocol::Prometheus {
                        Some(prometheus::PrometheusConverter::with_provenance(
                            endpoints[idx].config.source_label().to_string(),
                            endpoints[idx].config.url.to_string(),
                        ))
                    } else {
                        None
                    };
                    writers[idx] = Some(EndpointWriter {
                        writer: tempfile_in(out_dir.clone()).expect("failed to create temp file"),
                        converter,
                    });
                }
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
            if wrapped {
                warn!("command exited before any metrics were recorded");
                return outcome;
            }
            eprintln!("error: no data was recorded from any endpoint");
            std::process::exit(1);
        }

        // `.rez` mode finalizes a per-sampler tar archive instead of parquet/raw.
        if rez_mode {
            match rez_recorder.take() {
                Some(rec) => {
                    if let Err(e) = rec.finalize(&config.output) {
                        eprintln!("error saving .rez archive: {e}");
                    } else {
                        info!("wrote .rez archive to {}", config.output.display());
                    }
                }
                None => eprintln!("error: no snapshots captured for .rez archive"),
            }
            return outcome;
        }

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
                                    eprintln!("error writing {}: {e}", dest_path.display());
                                }
                            }
                            Err(e) => eprintln!("error creating {}: {e}", dest_path.display()),
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
                                    eprintln!(
                                        "error saving parquet for {}: {e}",
                                        endpoints[idx].config.source_label()
                                    );
                                } else {
                                    info!("wrote {}", dest_path.display());
                                }
                            }
                            Err(e) => {
                                eprintln!("error creating {}: {e}", dest_path.display());
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
                                    eprintln!("error saving parquet file: {e}");
                                }
                            }
                            Err(e) => {
                                eprintln!("error creating output file: {e}");
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
                                    eprintln!("failed to create temp parquet file: {e}");
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
                                        eprintln!(
                                            "error converting {} to parquet: {e}",
                                            endpoints[idx].config.source_label()
                                        );
                                        continue;
                                    }
                                    temp_parquets.push(temp);
                                }
                                Err(e) => {
                                    eprintln!("error creating temp parquet: {e}");
                                }
                            }
                        }
                    }

                    if temp_parquets.len() < 2 {
                        // Only one file survived — just move it
                        if let Some(temp) = temp_parquets.into_iter().next() {
                            if let Err(e) = std::fs::copy(temp.path(), &config.output) {
                                eprintln!("error writing output: {e}");
                            }
                        } else {
                            eprintln!("error: no data was recorded");
                        }
                    } else {
                        let paths: Vec<PathBuf> = temp_parquets
                            .iter()
                            .map(|t| t.path().to_path_buf())
                            .collect();

                        if let Err(e) =
                            crate::parquet_tools::combine::combine_files(&paths, &config.output)
                        {
                            eprintln!("error combining parquet files: {e}");
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

    if let Some(o) = outcome {
        // Flush buffered logs before exiting the process: std::process::exit
        // skips destructors, so drop the log drain explicitly first.
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
