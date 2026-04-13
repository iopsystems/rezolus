use super::*;
use clap::ArgMatches;

mod prometheus;
mod valkey;

pub struct Config {
    interval: humantime::Duration,
    duration: Option<humantime::Duration>,
    format: Format,
    verbose: u8,
    url: Url,
    output: PathBuf,
    metadata: Vec<(String, String)>,
    counter_map: Option<PathBuf>,
}
impl TryFrom<ArgMatches> for Config {
    type Error = String;

    fn try_from(
        args: ArgMatches,
    ) -> Result<Self, <Self as std::convert::TryFrom<clap::ArgMatches>>::Error> {
        Ok(Config {
            url: args.get_one::<Url>("URL").unwrap().clone(),
            output: args.get_one::<PathBuf>("OUTPUT").unwrap().to_path_buf(),
            verbose: *args.get_one::<u8>("VERBOSE").unwrap_or(&0),
            interval: *args
                .get_one::<humantime::Duration>("INTERVAL")
                .unwrap_or(&humantime::Duration::from_str("1s").unwrap()),
            duration: args.get_one::<humantime::Duration>("DURATION").copied(),
            format: args
                .get_one::<Format>("FORMAT")
                .copied()
                .unwrap_or(Format::Parquet),
            metadata: args
                .get_many::<String>("METADATA")
                .unwrap_or_default()
                .filter_map(|s| {
                    s.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                })
                .collect(),
            counter_map: args.get_one::<PathBuf>("COUNTER_MAP").cloned(),
        })
    }
}

enum SourceType {
    Rezolus {
        client: Client,
        url: Url,
    },
    Prometheus {
        client: Client,
        url: Url,
        converter: prometheus::PrometheusConverter,
    },
    Valkey(valkey::ValkeySource),
}

pub fn command() -> Command {
    Command::new("record")
        .about("On-demand recording to a file")
        .arg(
            clap::Arg::new("URL")
                .help("Metrics endpoint (Rezolus agent, Prometheus, or redis://host:port for Valkey/Redis)")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(Url))
                .required(true)
                .index(1),
        )
        .arg(
            clap::Arg::new("OUTPUT")
                .help("Path to the output file")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(PathBuf))
                .required(true)
                .index(2),
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
            clap::Arg::new("COUNTER_MAP")
                .long("counter-map")
                .help("JSON file mapping Redis/Valkey INFO fields to counter types")
                .action(clap::ArgAction::Set)
                .value_parser(value_parser!(PathBuf)),
        )
}

/// Runs the Rezolus `recorder` which is a Rezolus client that pulls data from
/// the msgpack endpoint and writes it to disk. The caller may use either timed
/// collection or terminate the process to finalize the recording.
///
/// This is intended to be run as ad-hoc collection of high-resolution metrics
/// or in situations where Rezolus is being used outside of a full observability
/// stack, for example in lab environments where experiments are being run using
/// either manual or automated processes.
pub fn run(config: Config) {
    // configure logging
    let _log_drain = configure_logging(verbosity_to_level(config.verbose));

    // initialize async runtime
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

    // open our destination file
    let mut destination = std::fs::File::create(config.output.clone())
        .map_err(|e| {
            eprintln!("failed to open destination file: {e}");
            std::process::exit(1);
        })
        .ok();

    // our writer will either be our destination if the output is raw msgpack or
    // it will be some tempfile
    let mut writer = match config.format {
        Format::Raw => destination.take().unwrap(),
        Format::Parquet => {
            let mut path: PathBuf = config.output.clone();
            path.pop();

            match tempfile_in(path.clone()) {
                Ok(t) => t,
                Err(error) => {
                    eprintln!("could not open temporary file in: {path:?}\n{error}");
                    std::process::exit(1);
                }
            }
        }
    };

    // Determine source type from URL scheme
    let mut source_type: SourceType = match config.url.scheme() {
        "redis" | "rediss" | "valkey" | "valkeys" => {
            let counter_map = config.counter_map.as_ref().map(|path| {
                valkey::CounterMap::load(path).unwrap_or_else(|e| {
                    eprintln!("{e}");
                    std::process::exit(1);
                })
            });
            match rt.block_on(valkey::ValkeySource::connect(
                &config.url,
                &config.interval,
                counter_map,
            )) {
                Ok(source) => SourceType::Valkey(source),
                Err(e) => {
                    eprintln!("failed to connect to Redis/Valkey: {e}");
                    std::process::exit(1);
                }
            }
        }
        "http" | "https" => {
            // our http client
            let client = match Client::builder().http1_only().build() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("error creating http client: {e}");
                    std::process::exit(1);
                }
            };

            // Auto-detect source type by probing the endpoint. For root-path
            // URLs, try the Rezolus binary endpoint first; if it fails, try
            // /metrics as Prometheus. For non-root URLs, use the path as-is.
            let mut url = config.url.clone();

            let is_prometheus = {
                let client = client.clone();

                rt.block_on(async {
                    let candidates: Vec<(Url, bool)> = if url.path() == "/" {
                        let mut rezolus_url = url.clone();
                        rezolus_url.set_path("/metrics/binary");
                        let mut prom_url = url.clone();
                        prom_url.set_path("/metrics");
                        vec![(rezolus_url, false), (prom_url, true)]
                    } else {
                        vec![(url.clone(), true)]
                    };

                    for (candidate_url, is_prom) in &candidates {
                        let start = Instant::now();

                        if let Ok(response) = client.get(candidate_url.clone()).send().await {
                            if !response.status().is_success() {
                                continue;
                            }

                            if let Ok(body) = response.bytes().await {
                                let latency = start.elapsed();

                                if latency.as_nanos() >= config.interval.as_nanos() {
                                    let recommended =
                                        humantime::Duration::from(Duration::from_millis(
                                            (latency * 2).as_nanos().div_ceil(1000000) as u64,
                                        ));
                                    eprintln!(
                                        "sampling latency ({} us) exceeded the sample interval. \
                                         Try setting the interval to: {}",
                                        latency.as_micros(),
                                        recommended
                                    );
                                    std::process::exit(1);
                                } else if latency.as_nanos() >= (3 * config.interval.as_nanos() / 4)
                                {
                                    warn!(
                                        "sampling latency ({} us) is more that 75% of the sample \
                                         interval. Consider increasing the interval",
                                        latency.as_micros()
                                    );
                                } else {
                                    debug!("sampling latency: {} us", latency.as_micros());
                                }

                                if *is_prom {
                                    url = candidate_url.clone();
                                    info!("detected prometheus endpoint: {url}");
                                    return true;
                                }

                                // Try to deserialize as msgpack to confirm it's Rezolus
                                if rmp_serde::from_slice::<metriken_exposition::Snapshot>(&body)
                                    .is_ok()
                                {
                                    url = candidate_url.clone();
                                    info!("detected rezolus agent: {url}");
                                    return false;
                                }

                                // Got a response but it's not valid msgpack - try next
                                continue;
                            }
                        }
                    }

                    eprintln!(
                        "failed to connect or unrecognized response. \
                         Please check that the source is running and the address is correct"
                    );
                    std::process::exit(1);
                })
            };

            if is_prometheus {
                SourceType::Prometheus {
                    client,
                    url,
                    converter: prometheus::PrometheusConverter::new(),
                }
            } else {
                SourceType::Rezolus { client, url }
            }
        }
        other => {
            eprintln!("unsupported URL scheme: {other}");
            std::process::exit(1);
        }
    };

    // Fetch the agent's systeminfo for embedding in parquet metadata
    let agent_systeminfo: Option<String> = match &source_type {
        SourceType::Rezolus { client, .. } => {
            let client = client.clone();
            let mut info_url = config.url.clone();
            info_url.set_path("/systeminfo");
            rt.block_on(async move {
                match client.get(info_url).send().await {
                    Ok(response) if response.status().is_success() => response.text().await.ok(),
                    _ => None,
                }
            })
        }
        SourceType::Valkey(_) => None,
        SourceType::Prometheus { .. } => None,
    };

    if agent_systeminfo.is_some() {
        debug!("fetched systeminfo");
    } else {
        debug!("systeminfo not available");
    }

    // Fetch metric descriptions for embedding in parquet metadata
    let descriptions: Option<String> = match &source_type {
        SourceType::Rezolus { client, .. } => {
            let client = client.clone();
            let mut desc_url = config.url.clone();
            desc_url.set_path("/metrics/descriptions");
            rt.block_on(async move {
                match client.get(desc_url).send().await {
                    Ok(response) if response.status().is_success() => response.text().await.ok(),
                    _ => None,
                }
            })
        }
        // Prometheus descriptions come from # HELP lines during recording;
        // Valkey descriptions are static and collected after recording.
        _ => None,
    };

    if descriptions.is_some() {
        debug!("fetched descriptions from agent");
    } else {
        debug!("agent descriptions not available");
    }

    // Source and version metadata for Valkey/Redis
    let source_metadata: Option<(String, String)> = match &source_type {
        SourceType::Valkey(source) => Some((
            source.source_name().to_string(),
            source.server_version().to_string(),
        )),
        _ => None,
    };

    if config.duration.is_some() {
        info!("recording metrics... ctrl-c to terminate early");
    } else {
        info!("recording metrics... ctrl-c to end the recording");
    }

    rt.block_on(async move {
        // get the approximate time for the first sample
        let interval: Duration = config.interval.into();
        let start = Instant::now() + interval;

        // sampling interval
        let mut interval = crate::common::aligned_interval(interval);

        // sample in a loop until RUNNING is false or duration has completed
        while STATE.load(Ordering::Relaxed) == RUNNING {
            // check if the duration has completed
            if let Some(duration) = config.duration.map(Into::<Duration>::into) {
                if start.elapsed() >= duration {
                    break;
                }
            }

            // wait to sample
            interval.tick().await;

            let sample_start = Instant::now();

            // sample the metrics source
            let body: Option<Vec<u8>> = match &mut source_type {
                SourceType::Rezolus { client, url } => {
                    match client.get(url.clone()).send().await {
                        Ok(response) => response.bytes().await.ok().map(|b| b.to_vec()),
                        Err(_) => None,
                    }
                }
                SourceType::Prometheus {
                    client,
                    url,
                    converter,
                } => match client.get(url.clone()).send().await {
                    Ok(response) => match response.text().await {
                        Ok(text) => {
                            let snapshot = converter.convert(&text);
                            match rmp_serde::encode::to_vec(&snapshot) {
                                Ok(bytes) => Some(bytes),
                                Err(e) => {
                                    error!("error serializing snapshot: {e}");
                                    None
                                }
                            }
                        }
                        Err(_) => None,
                    },
                    Err(_) => None,
                },
                SourceType::Valkey(source) => source.fetch_snapshot().await,
            };

            if let Some(body) = body {
                let latency = sample_start.elapsed();

                if latency.as_nanos() >= config.interval.as_nanos() {
                    error!("sampling latency ({} us) exceeded the sample interval. Samples will be missing", latency.as_micros());
               } else if latency.as_nanos() >= (3 * config.interval.as_nanos() / 4) {
                    warn!("sampling latency ({} us) is more that 75% of the sample interval. Consider increasing the interval", latency.as_micros());
                } else {
                    debug!("sampling latency: {} us", latency.as_micros());
                }

                if let Err(e) = writer.write_all(&body) {
                    eprintln!("error writing to temporary file: {e}");
                    std::process::exit(1);
                }
            } else {
                eprintln!("failed to read response. terminating early");
                break;
            }
        }

        debug!("flushing writer");
        let _ = writer.flush();

        // Collect descriptions accumulated during recording
        let extra_descriptions = match &source_type {
            SourceType::Prometheus { converter, .. } => {
                if !converter.descriptions().is_empty() {
                    serde_json::to_string(converter.descriptions()).ok()
                } else {
                    None
                }
            }
            SourceType::Valkey(source) => {
                let descs = source.descriptions();
                if !descs.is_empty() {
                    serde_json::to_string(descs).ok()
                } else {
                    None
                }
            }
            _ => None,
        };

        // handle any output format specific transforms
        match config.format {
            Format::Raw => {
                debug!("finished");
            }
            Format::Parquet => {
                info!("converting the recording to parquet... please wait");

                let _ = writer.rewind();

                let mut converter = MsgpackToParquet::with_options(ParquetOptions::new())
                    .metadata(
                        "sampling_interval_ms".to_string(),
                        config.interval.as_millis().to_string(),
                    );

                for (key, value) in &config.metadata {
                    converter = converter.metadata(key.clone(), value.clone());
                }

                if let Some(ref json) = agent_systeminfo {
                    converter = converter.metadata("systeminfo".to_string(), json.clone());
                }

                if let Some((ref source, ref version)) = source_metadata {
                    converter = converter.metadata("source".to_string(), source.clone());
                    converter = converter.metadata("version".to_string(), version.clone());
                }

                let desc_json = descriptions.or(extra_descriptions);
                if let Some(ref json) = desc_json {
                    converter = converter.metadata("descriptions".to_string(), json.clone());
                }

                if let Err(e) = converter.convert_file_handle(writer, destination.unwrap())
                {
                    eprintln!("error saving parquet file: {e}");
                }
            }
        }
    })
}
