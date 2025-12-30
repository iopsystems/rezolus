use super::*;
use std::fs::OpenOptions;

mod config;
mod http;
mod state;

pub use config::Config;
use state::{DumpToFileRequest, DumpToFileResponse, SharedState, TimeRange};

pub fn command() -> Command {
    Command::new("hindsight")
        .about("Continuous recording to an on-disk ring buffer")
        .arg(
            clap::Arg::new("CONFIG")
                .help("Rezolus Hindsight configuration file")
                .value_parser(value_parser!(PathBuf))
                .action(clap::ArgAction::Set)
                .required(true)
                .index(1),
        )
}

/// Runs the Rezolus `flight-recorder` which is a Rezolus client that pulls data
/// from the msgpack endpoint and maintains an on-disk buffer across some span
/// of time. If the process receives a SIGHUP it will persist the ring buffer to
/// an output file.
///
/// This is intended to be run as a daemon that allows retroactive collection of
/// high-resolution metrics in the event of an anomaly. To be effective the
/// collection `interval` should be more frequent than your observability stack
/// allows for, for example per-second collection in an environment with only
/// minutely metrics. Additionally the `duration` should allow adequate time to
/// not only cover the duration of an anomalous event but give time for an
/// engineer or automated process to respond and trigger the process to persist
/// the ring buffer.
///
/// Optionally, an HTTP endpoint can be enabled to allow remote triggering of
/// ring buffer dumps without terminating the service.
pub fn run(config: Config) {
    // load config from file
    let config: Arc<Config> = config.into();

    // configure debug log
    let debug_output: Box<dyn Output> = Box::new(Stderr::new());

    let level = config.log().level();

    let debug_log = if level <= Level::Info {
        LogBuilder::new().format(ringlog::default_format)
    } else {
        LogBuilder::new()
    }
    .output(debug_output)
    .build()
    .expect("failed to initialize debug log");

    let mut log = MultiLogBuilder::new()
        .level_filter(level.to_level_filter())
        .default(debug_log)
        .build()
        .start();

    // initialize async runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    // spawn logging thread
    rt.spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let _ = log.flush();
        }
    });

    ctrlc::set_handler(move || {
        let state = STATE.load(Ordering::SeqCst);

        if state == RUNNING {
            info!("triggering ringbuffer capture");
            STATE.store(CAPTURING, Ordering::SeqCst);
        } else if state == CAPTURING {
            info!("waiting for capture to complete before exiting");
            STATE.store(TERMINATING, Ordering::SeqCst);
        } else {
            info!("terminating immediately");
            std::process::exit(2);
        }
    })
    .expect("failed to set ctrl-c handler");

    let url = config.general().url();

    // our blocking http client
    let blocking_client = match reqwest::blocking::Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            error!("error connecting to Rezolus: {e}");
            std::process::exit(1);
        }
    };

    // our async http client
    let async_client = match reqwest::Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            error!("error connecting to Rezolus: {e}");
            std::process::exit(1);
        }
    };

    // create our destination file if it doesn't exist, otherwise open the
    // existing file - it will be truncated only before we write into it
    let mut destination = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.general().output())
        .map_err(|e| {
            error!("failed to open destination file: {e}");
            std::process::exit(1);
        })
        .unwrap();

    // Get the directory for temp files
    let temp_dir = {
        let mut path: PathBuf = config.general().output();
        path.pop();
        path
    };

    // our writer will always be a new temporary file
    // Use NamedTempFile so we can get the path for sharing with HTTP handlers
    let writer = match tempfile::NamedTempFile::new_in(&temp_dir) {
        Ok(t) => t,
        Err(error) => {
            eprintln!("could not open temporary file in: {temp_dir:?}\n{error}");
            std::process::exit(1);
        }
    };

    // Get the path before we convert to file handle
    let temp_path = writer.path().to_path_buf();

    // Convert to regular file handle for writing
    let mut writer = writer.into_file();

    // estimate the snapshot size and latency
    let start = Instant::now();

    let (snap_len, latency) = if let Ok(response) = blocking_client.get(url.clone()).send() {
        if let Ok(body) = response.bytes() {
            let latency = start.elapsed();

            debug!("sampling latency: {} us", latency.as_micros());
            debug!("body size: {}", body.len());

            (body.len(), latency)
        } else {
            error!("error reading metrics endpoint");
            std::process::exit(1);
        }
    } else {
        error!("error reading metrics endpoint");
        std::process::exit(1);
    };

    // check that the sampling interval and sample latency are compatible
    if config.general().interval().as_micros() < (latency.as_micros() * 2) {
        error!("the sampling interval is too short to reliably record");
        error!(
            "set the interval to at least: {} us",
            latency.as_micros() * 2
        );
        std::process::exit(1);
    }

    // the snapshot len in blocks
    // note: we allow for more capacity than we need and round to the next
    // nearest whole number of blocks
    let snapshot_len = (1 + snap_len as u64 * 4 / 4096) * 4096;

    // the total number of snapshots
    let snapshot_count = (1 + config.general().duration().as_micros()
        / config.general().interval().as_micros()) as u64;

    // expand the temporary file to hold enough room for all the snapshots
    let _ = writer.set_len(snapshot_len * snapshot_count).map_err(|e| {
        error!("failed to grow temporary file: {e}");
        std::process::exit(1);
    });

    // Create shared state for HTTP handlers
    let shared_state = Arc::new(SharedState::new(
        temp_path,
        snapshot_len,
        snapshot_count,
        config.general().interval().into(),
        config.general().duration().into(),
        config.general().output(),
    ));

    // Create channel for dump-to-file requests
    let (dump_tx, mut dump_rx) = tokio::sync::mpsc::channel::<DumpToFileRequest>(8);

    // Optionally spawn HTTP server
    if let Some(listen_addr) = config.general().listen() {
        let shared = shared_state.clone();
        rt.spawn(async move {
            http::serve(listen_addr, shared, dump_tx).await;
        });
    }

    let shared_for_loop = shared_state.clone();

    rt.block_on(async move {
        let shared = shared_for_loop;

        // sampling interval
        let mut interval = crate::common::aligned_interval(config.general().interval().into());

        // sampling loop
        while STATE.load(Ordering::Relaxed) < TERMINATING {
            // sample in a loop until RUNNING is false
            loop {
                tokio::select! {
                    biased;

                    // Check for dump-to-file requests from HTTP endpoint
                    Some(request) = dump_rx.recv() => {
                        debug!("received dump-to-file request via HTTP");
                        let response = perform_dump_to_file(
                            &mut writer,
                            &mut destination,
                            &shared,
                            &config,
                            &request.time_range,
                        );
                        let _ = request.response_tx.send(response);
                    }

                    // Regular sampling tick
                    _ = interval.tick() => {
                        // Check if we should exit the sampling loop
                        if STATE.load(Ordering::Relaxed) != RUNNING {
                            break;
                        }

                        let start = Instant::now();

                        // sample rezolus
                        if let Ok(response) = async_client.get(url.clone()).send().await {
                            if let Ok(body) = response.bytes().await {
                                let latency = start.elapsed();

                                debug!("sampling latency: {} us", latency.as_micros());
                                debug!("body size: {}", body.len());

                                let idx = shared.idx();

                                // seek to position in snapshot
                                writer
                                    .seek(SeekFrom::Start(idx * snapshot_len))
                                    .expect("failed to seek");

                                // write the size of the snapshot
                                writer
                                    .write_all(&body.len().to_be_bytes())
                                    .expect("failed to write snapshot size");

                                // write the actual snapshot content
                                writer.write_all(&body).expect("failed to write snapshot");

                                // Update shared state atomically
                                shared.advance_idx();
                            } else {
                                error!("failed to read response");
                                std::process::exit(1);
                            }
                        } else {
                            error!("failed to get metrics");
                            std::process::exit(1);
                        }
                    }
                }
            }

            // Handle Ctrl+C triggered dump (CAPTURING state)
            if STATE.load(Ordering::Relaxed) == CAPTURING {
                debug!("flushing writer and preparing destination");
                let _ = writer.flush();

                let response = perform_dump_to_file(
                    &mut writer,
                    &mut destination,
                    &shared,
                    &config,
                    &TimeRange::default(), // no time filter
                );

                if let Some(error) = response.error {
                    error!("dump failed: {}", error);
                } else {
                    info!(
                        "ringbuffer capture complete: {} snapshots written to {}",
                        response.snapshots,
                        response.path.display()
                    );
                }

                if STATE.load(Ordering::SeqCst) == TERMINATING {
                    return;
                } else {
                    STATE.store(RUNNING, Ordering::SeqCst);
                }
            }
        }
    });
}

/// Perform a dump of the ring buffer to the destination file
fn perform_dump_to_file(
    writer: &mut std::fs::File,
    destination: &mut std::fs::File,
    shared: &SharedState,
    config: &Config,
    time_range: &TimeRange,
) -> DumpToFileResponse {
    use metriken_exposition::{MsgpackToParquet, ParquetOptions, Snapshot};
    use std::time::UNIX_EPOCH;

    debug!("capturing ringbuffer and writing to parquet");

    let idx = shared.idx();
    let valid_count = shared.valid_snapshot_count();

    // Prepare destination
    if let Err(e) = destination.seek(SeekFrom::Start(0)) {
        return DumpToFileResponse::error(format!("failed to seek destination: {}", e));
    }
    if let Err(e) = destination.set_len(0) {
        return DumpToFileResponse::error(format!("failed to truncate destination: {}", e));
    }

    let mut snapshots_written = 0u64;
    let mut first_timestamp: Option<u64> = None;
    let mut last_timestamp: Option<u64> = None;

    // Get temp directory for packed file
    let temp_dir = {
        let mut path: PathBuf = config.general().output();
        path.pop();
        path
    };

    // Create temporary file for packed msgpack data
    let mut packed = match tempfile_in(temp_dir.clone()) {
        Ok(t) => t,
        Err(error) => {
            return DumpToFileResponse::error(format!(
                "could not open temporary file in: {:?}\n{}",
                temp_dir, error
            ));
        }
    };

    for offset in 0..valid_count {
        let mut i = idx + offset;
        if i >= shared.snapshot_count {
            i -= shared.snapshot_count;
        }

        // seek to the start of the snapshot slot
        if writer
            .seek(SeekFrom::Start(i * shared.snapshot_len))
            .is_err()
        {
            continue;
        }

        // read the size of the snapshot
        let mut len = [0u8; 8];
        if writer.read_exact(&mut len).is_err() {
            continue;
        }

        let size = u64::from_be_bytes(len) as usize;
        if size == 0 {
            continue;
        }

        // read the contents of the snapshot
        let mut buf = vec![0u8; size];
        if writer.read_exact(&mut buf).is_err() {
            continue;
        }

        // Apply time filter if specified
        if time_range.start.is_some() || time_range.end.is_some() {
            if let Ok(snapshot) = rmp_serde::from_slice::<Snapshot>(&buf) {
                if let Snapshot::V2(ref s) = snapshot {
                    if !time_range.contains(s.systemtime) {
                        continue;
                    }
                    // Track timestamps
                    if let Ok(dur) = s.systemtime.duration_since(UNIX_EPOCH) {
                        let ts = dur.as_secs();
                        if first_timestamp.is_none() {
                            first_timestamp = Some(ts);
                        }
                        last_timestamp = Some(ts);
                    }
                }
            }
        } else {
            // No time filter, still track timestamps for response
            if let Ok(snapshot) = rmp_serde::from_slice::<Snapshot>(&buf) {
                if let Snapshot::V2(ref s) = snapshot {
                    if let Ok(dur) = s.systemtime.duration_since(UNIX_EPOCH) {
                        let ts = dur.as_secs();
                        if first_timestamp.is_none() {
                            first_timestamp = Some(ts);
                        }
                        last_timestamp = Some(ts);
                    }
                }
            }
        }

        // write the contents of the snapshot to the packed file
        if packed.write_all(&buf).is_err() {
            return DumpToFileResponse::error("failed to write to packed file".to_string());
        }
        snapshots_written += 1;
    }

    let _ = packed.flush();
    if packed.rewind().is_err() {
        return DumpToFileResponse::error("failed to rewind packed file".to_string());
    }

    if let Err(e) = MsgpackToParquet::with_options(ParquetOptions::new())
        .metadata(
            "sampling_interval_ms".to_string(),
            config.general().interval().as_millis().to_string(),
        )
        .convert_file_handle(packed, &mut *destination)
    {
        return DumpToFileResponse::error(format!("error saving parquet file: {}", e));
    }

    let _ = destination.flush();
    debug!("finished parquet dump");

    DumpToFileResponse::success(
        config.general().output(),
        snapshots_written,
        first_timestamp,
        last_timestamp,
    )
}
