use super::*;
use std::fs::OpenOptions;

mod config;

pub use config::Config;

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
/// allows for, for example secondly collection in an environment with only
/// minutely metrics. Additionally the `duration` should allow adequate time to
/// not only cover the duration of an anomalous event but give time for an
/// engineer or automated process to respond and trigger the process to persist
/// the ring buffer.
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

    // our http client
    let client = match Client::builder().http1_only().build() {
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

    // our writer will always be a new temporary file
    let mut writer = {
        let mut path: PathBuf = config.general().output();
        path.pop();

        match tempfile_in(path.clone()) {
            Ok(t) => t,
            Err(error) => {
                eprintln!("could not open temporary file in: {:?}\n{error}", path);
                std::process::exit(1);
            }
        }
    };

    // estimate the snapshot size and latency
    let start = Instant::now();

    let (snap_len, latency) = if let Ok(response) = client.get(url.clone()).send() {
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

    let mut idx = 0;

    rt.block_on(async move {
        // sampling interval
        let mut interval = crate::common::aligned_interval(config.general().interval().into());

        // sampling loop
        while STATE.load(Ordering::Relaxed) < TERMINATING {
            // sample in a loop until RUNNING is false
            while STATE.load(Ordering::Relaxed) == RUNNING {
                // wait to sample
                interval.tick().await;

                let start = Instant::now();

                // sample rezolus
                if let Ok(response) = client.get(url.clone()).send() {
                    if let Ok(body) = response.bytes() {
                        let latency = start.elapsed();

                        debug!("sampling latency: {} us", latency.as_micros());

                        debug!("body size: {}", body.len());

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
                    } else {
                        error!("failed to read response");
                        std::process::exit(1);
                    }
                } else {
                    error!("failed to get metrics");
                    std::process::exit(1);
                }

                idx += 1;

                if idx >= snapshot_count {
                    idx = 0;
                }
            }

            debug!("flushing writer and preparing destination");
            let _ = writer.flush();
            let _ = destination.seek(SeekFrom::Start(0));
            let _ = destination.set_len(0);

            // handle any output format specific transforms
            //
            // note: as the idx is the position we will write to next, it is
            //       also the position to start reading from
            match config.general().format() {
                Format::Raw => {
                    debug!("capturing ringbuffer and writing to raw");

                    for offset in 0..snapshot_count {
                        let mut i = idx + offset;

                        // handle wrap-around in the ring-buffer
                        if i >= snapshot_len {
                            i -= snapshot_len;
                        }

                        // seek to the start of the snapshot slot
                        writer
                            .seek(SeekFrom::Start(i * snapshot_len))
                            .expect("failed to seek");

                        // read the size of the snapshot
                        let mut len = [0, 0, 0, 0, 0, 0, 0, 0];
                        writer
                            .read_exact(&mut len)
                            .expect("failed to read snapshot len");

                        // read the contents of the snapshot
                        let mut buf = vec![0; u64::from_be_bytes(len) as usize];
                        writer
                            .read_exact(&mut buf)
                            .expect("failed to read snapshot");

                        // write the contents of the snapshot to the packed file
                        destination
                            .write_all(&buf)
                            .expect("failed to write to packed file");
                    }

                    let _ = destination.flush();

                    debug!("finished");
                }
                Format::Parquet => {
                    debug!("capturing ringbuffer and writing to parquet");

                    let _ = writer.rewind();

                    // we need another temporary file to consume the empty space
                    // between snapshots

                    // TODO(bmartin): we can probably remove this by using our
                    // own msgpack -> parquet conversion

                    // our writer will always be a temporary file
                    let mut packed = {
                        let mut path: PathBuf = config.general().output();
                        path.pop();

                        match tempfile_in(path.clone()) {
                            Ok(t) => t,
                            Err(error) => {
                                eprintln!("could not open temporary file in: {:?}\n{error}", path);
                                std::process::exit(1);
                            }
                        }
                    };

                    for offset in 0..snapshot_count {
                        let mut i = idx + offset;

                        // handle wrap-around in the ring-buffer
                        if i >= snapshot_count {
                            i -= snapshot_count;
                        }

                        // seek to the start of the snapshot slot
                        writer
                            .seek(SeekFrom::Start(i * snapshot_len))
                            .expect("failed to seek");

                        // read the size of the snapshot
                        let mut len = [0, 0, 0, 0, 0, 0, 0, 0];
                        writer
                            .read_exact(&mut len)
                            .expect("failed to read snapshot len");

                        // read the contents of the snapshot
                        let mut buf = vec![0; u64::from_be_bytes(len) as usize];
                        writer
                            .read_exact(&mut buf)
                            .expect("failed to read snapshot");

                        // write the contents of the snapshot to the packed file
                        packed
                            .write_all(&buf)
                            .expect("failed to write to packed file");
                    }

                    let _ = packed.flush();
                    let _ = packed.rewind();

                    if let Err(e) = MsgpackToParquet::with_options(ParquetOptions::new())
                        .metadata(
                            "sampling_interval_ms".to_string(),
                            config.general().interval().as_millis().to_string(),
                        )
                        .convert_file_handle(packed, &destination)
                    {
                        eprintln!("error saving parquet file: {e}");
                    }

                    let _ = destination.flush();
                }
            }

            debug!("ringbuffer capture complete");

            if STATE.load(Ordering::SeqCst) == TERMINATING {
                return;
            } else {
                STATE.store(RUNNING, Ordering::SeqCst);
            }
        }
    });
}
