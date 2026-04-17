use super::*;

mod config;
mod endpoint;
mod prometheus;

use crate::parquet_metadata;
pub use config::RecordingConfig;
use endpoint::{EndpointState, EndpointStatus, Protocol};
use std::path::Path;

pub fn command() -> Command {
    Command::new("record")
        .about("On-demand recording to a file")
        .arg(
            clap::Arg::new("URL")
                .help("Metrics endpoint (Rezolus agent or Prometheus)")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(Url))
                .index(1),
        )
        .arg(
            clap::Arg::new("OUTPUT")
                .help("Path to the output file")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(PathBuf))
                .index(2),
        )
        .arg(
            clap::Arg::new("CONFIG_FILE")
                .long("config")
                .help("TOML config file for multi-endpoint recording")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(PathBuf))
                .conflicts_with_all(["URL"]),
        )
        .arg(
            clap::Arg::new("ENDPOINT")
                .long("endpoint")
                .help("Endpoint: url[,source=name][,role=role][,protocol=msgpack|prometheus]")
                .action(clap::ArgAction::Append)
                .conflicts_with_all(["URL", "CONFIG_FILE"]),
        )
        .arg(
            clap::Arg::new("SEPARATE")
                .long("separate")
                .help("Write one parquet file per endpoint instead of combining")
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
                .help("Sets the collection interval")
                .action(clap::ArgAction::Set)
                .default_value("1s")
                .value_parser(value_parser!(humantime::Duration)),
        )
        .arg(
            clap::Arg::new("DURATION")
                .long("duration")
                .short('d')
                .help("Sets the collection duration")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(humantime::Duration)),
        )
        .arg(
            clap::Arg::new("FORMAT")
                .long("format")
                .short('f')
                .help("Sets the collection format")
                .action(clap::ArgAction::Set)
                .default_value("parquet")
                .value_parser(value_parser!(Format)),
        )
        .arg(
            clap::Arg::new("METADATA")
                .long("metadata")
                .short('m')
                .help("Add file-level parquet metadata (key=value)")
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
}

// ---------------------------------------------------------------------------
// Probe and metadata helpers
// ---------------------------------------------------------------------------

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

/// Fetch systeminfo and descriptions from a Rezolus agent endpoint.
async fn fetch_agent_metadata(client: &Client, base_url: &Url) -> (Option<String>, Option<String>) {
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

    (systeminfo, descriptions)
}

// ---------------------------------------------------------------------------
// Scrape helper
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Metadata injection for msgpack snapshots
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Output path helpers
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Parquet converter builder
// ---------------------------------------------------------------------------

fn build_parquet_converter(
    config: &RecordingConfig,
    ep: &EndpointState,
    prom_converter: &Option<prometheus::PrometheusConverter>,
) -> MsgpackToParquet {
    let mut converter = MsgpackToParquet::with_options(ParquetOptions::new()).metadata(
        "sampling_interval_ms".to_string(),
        config.interval.as_millis().to_string(),
    );

    // Source metadata
    converter = converter.metadata("source".to_string(), ep.config.source.clone());

    // User-supplied metadata
    for (key, value) in &config.metadata {
        converter = converter.metadata(key.clone(), value.clone());
    }

    // Agent systeminfo
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

    // per_source_metadata with first/last sample timestamps
    let mut psm = serde_json::Map::new();
    let mut source_meta = serde_json::Map::new();
    if let Some(ns) = ep.first_success_ns {
        source_meta.insert(
            parquet_metadata::NESTED_FIRST_SAMPLE_NS.to_string(),
            serde_json::json!(ns),
        );
    }
    if let Some(ns) = ep.last_success_ns {
        source_meta.insert(
            parquet_metadata::NESTED_LAST_SAMPLE_NS.to_string(),
            serde_json::json!(ns),
        );
    }
    if let Some(ref role) = ep.config.role {
        source_meta.insert(
            parquet_metadata::NESTED_ROLE.to_string(),
            serde_json::json!(role),
        );
    }
    if !source_meta.is_empty() {
        psm.insert(
            ep.config.source.clone(),
            serde_json::Value::Object(source_meta),
        );
        if let Ok(json) = serde_json::to_string(&psm) {
            converter = converter.metadata("per_source_metadata".to_string(), json);
        }
    }

    converter
}

// ---------------------------------------------------------------------------
// Per-endpoint writer state
// ---------------------------------------------------------------------------

struct EndpointWriter {
    writer: std::fs::File,
    converter: Option<prometheus::PrometheusConverter>,
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

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

    // Initialize endpoint states
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
                    info!(
                        "endpoint {} ({}): detected {:?}",
                        ep.config.source, ep.config.url, protocol
                    );
                    if protocol == Protocol::Msgpack {
                        let (si, desc) = fetch_agent_metadata(&client, &ep.config.url).await;
                        ep.systeminfo = si;
                        ep.descriptions = desc;
                    }
                    ep.scrape_url = Some(url);
                    ep.detected_protocol = Some(protocol);
                    ep.status = EndpointStatus::Active;
                }
                None => {
                    warn!(
                        "endpoint {} ({}) not reachable, will retry each tick",
                        ep.config.source, ep.config.url
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

    // Create per-endpoint writers
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
                        ep.config.source.clone(),
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

    if config.duration.is_some() {
        info!("recording metrics... ctrl-c to terminate early");
    } else {
        info!("recording metrics... ctrl-c to end the recording");
    }

    rt.block_on(async {
        let interval_dur: Duration = config.interval.into();
        let start = Instant::now() + interval_dur;
        let mut interval = crate::common::aligned_interval(interval_dur);

        while STATE.load(Ordering::Relaxed) == RUNNING {
            if let Some(duration) = config.duration.map(Into::<Duration>::into) {
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
                                let snapshot = conv.convert(&text);
                                match rmp_serde::encode::to_vec(&snapshot) {
                                    Ok(b) => b,
                                    Err(e) => {
                                        error!(
                                            "serialize error for {}: {e}",
                                            endpoints[idx].config.source
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
                                            &endpoints[idx].config.source,
                                            endpoints[idx].config.url.as_str(),
                                        );
                                        match rmp_serde::encode::to_vec(&snapshot) {
                                            Ok(b) => b,
                                            Err(e) => {
                                                error!(
                                                    "serialize error for {}: {e}",
                                                    endpoints[idx].config.source
                                                );
                                                continue;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!(
                                            "msgpack decode error for {}: {e}",
                                            endpoints[idx].config.source
                                        );
                                        continue;
                                    }
                                }
                            };

                            if let Err(e) = ew.writer.write_all(&bytes) {
                                error!("write error for {}: {e}", endpoints[idx].config.source);
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "scrape failed for {} ({}): {e}",
                            endpoints[idx].config.source, endpoints[idx].config.url
                        );
                    }
                }
            }

            // Retry pending endpoints
            let pending_indices: Vec<usize> = endpoints
                .iter()
                .enumerate()
                .filter(|(_, ep)| ep.status == EndpointStatus::Pending)
                .map(|(i, _)| i)
                .collect();

            for idx in pending_indices {
                if let Some((protocol, url)) = probe_endpoint(&client, &endpoints[idx].config).await
                {
                    info!(
                        "endpoint {} ({}) now available, starting capture",
                        endpoints[idx].config.source, endpoints[idx].config.url
                    );
                    if protocol == Protocol::Msgpack {
                        let (si, desc) =
                            fetch_agent_metadata(&client, &endpoints[idx].config.url).await;
                        endpoints[idx].systeminfo = si;
                        endpoints[idx].descriptions = desc;
                    }
                    endpoints[idx].scrape_url = Some(url);
                    endpoints[idx].detected_protocol = Some(protocol.clone());
                    endpoints[idx].status = EndpointStatus::Active;

                    let converter = if protocol == Protocol::Prometheus {
                        Some(prometheus::PrometheusConverter::with_provenance(
                            endpoints[idx].config.source.clone(),
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

        // ── Finalization ──────────────────────────────────────────────────

        // Flush all writers
        for ew in writers.iter_mut().flatten() {
            let _ = ew.writer.flush();
        }

        let active_count = endpoints
            .iter()
            .filter(|ep| ep.first_success_ns.is_some())
            .count();

        if active_count == 0 {
            eprintln!("error: no data was recorded from any endpoint");
            std::process::exit(1);
        }

        match config.format {
            Format::Raw => {
                // For raw format, copy temp files to destinations
                for (idx, ew) in writers.iter_mut().enumerate() {
                    if let Some(ref mut ew) = ew {
                        if endpoints[idx].first_success_ns.is_none() {
                            continue;
                        }
                        let dest_path = if config.separate || active_count > 1 {
                            separate_output_path(&config.output, &endpoints[idx].config.source)
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
                // Separate mode: convert each endpoint to its own parquet
                info!("converting recordings to parquet (separate files)...");
                for (idx, ew) in writers.iter_mut().enumerate() {
                    if let Some(ref mut ew) = ew {
                        if endpoints[idx].first_success_ns.is_none() {
                            continue;
                        }
                        let dest_path =
                            separate_output_path(&config.output, &endpoints[idx].config.source);
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
                                        endpoints[idx].config.source
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
                                            endpoints[idx].config.source
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
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
}
