use super::*;

use clap::ArgMatches;
use histogram::Histogram;
use memmap2::MmapOptions;
use metriken_exposition::Snapshot;

use std::collections::HashMap;

pub struct Config {
    interval: humantime::Duration,
    duration: Option<humantime::Duration>,
    format: Format,
    verbose: u8,
    url: Url,
    output: PathBuf,
    mmap: Option<PathBuf>,
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
            mmap: args.get_one::<PathBuf>("MMAP").map(|v| v.to_path_buf()),
        })
    }
}

pub fn command() -> Command {
    Command::new("record")
        .about("On-demand recording to a file")
        .arg(
            clap::Arg::new("URL")
                .help("Rezolus HTTP endpoint")
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
                .help("Sets the collection interval")
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
            clap::Arg::new("MMAP")
                .long("mmap")
                .short('m')
                .help("Also record metrics from the memory mapped file")
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
    // configure debug log
    let debug_output: Box<dyn Output> = Box::new(Stderr::new());

    let level = match config.verbose {
        0 => Level::Info,
        1 => Level::Debug,
        _ => Level::Trace,
    };

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
            info!("finalizing recording...");
            STATE.store(TERMINATING, Ordering::SeqCst);
        } else {
            info!("terminating immediately");
            std::process::exit(2);
        }
    })
    .expect("failed to set ctrl-c handler");

    // parse source address
    let mut url = config.url.clone();

    if url.path() != "/" {
        eprintln!("URL should not have an non-root path: {url}");
        std::process::exit(1);
    }

    url.set_path("/metrics/binary");

    // our http client
    let client = match Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            error!("error connecting to Rezolus: {e}");
            std::process::exit(1);
        }
    };

    // open our destination file
    let mut destination = std::fs::File::create(config.output.clone())
        .map_err(|e| {
            error!("failed to open destination file: {e}");
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
                    eprintln!("could not open temporary file in: {:?}\n{error}", path);
                    std::process::exit(1);
                }
            }
        }
    };

    let mmap = config.mmap.map(|path| {
        let file = std::fs::File::open(&path)
            .map_err(|e| {
                eprintln!(
                    "failed to open specified memory mapped file: {:?}\n{e}",
                    path
                );
                std::process::exit(1);
            })
            .unwrap();

        // hardcoded for a histogram with grouping power of 5 and max value power of 64
        // let mmap_len = whole_pages::<u64>(1920) * PAGE_SIZE;

        let mmap = unsafe {
            MmapOptions::new().map(&file).map_err(|e| {
                eprintln!("failed to mmap the file: {:?}\n{e}", path);
                std::process::exit(1);
            })
        }
        .unwrap();

        // check the alignment
        let (_prefix, data, _suffix) = unsafe { mmap.align_to::<u64>() };
        // let expected_len = mmap_len / std::mem::size_of::<u64>();

        // hardcoded for a histogram with grouping power of 5 and max value power of 64
        if data.len() != 1920 {
            eprintln!("mmap region not aligned or width doesn't match");
            std::process::exit(1);
        }

        mmap
    });

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

            let start = Instant::now();

            let histogram = if let Some(ref mmap) = mmap {
                let (_prefix, buckets, _suffix) = unsafe { mmap.align_to::<u64>() };

                // hardcoded for a histogram with grouping power of 5 and max value power of 64
                Some(Histogram::from_buckets(5, 64, buckets[0..1920].to_vec()).unwrap())
            } else {
                None
            };

            // sample rezolus
            if let Ok(response) = client.get(url.clone()).send().await {
                if let Ok(body) = response.bytes().await {
                    let latency = start.elapsed();

                    debug!("sampling latency: {} us", latency.as_micros());

                    if let Some(histogram) = histogram {
                        let histogram = metriken_exposition::Histogram {
                            name: "frame_start_delay".to_string(),
                            value: histogram,
                            metadata: [
                                ("metric".to_string(), "frame_start_delay".to_string()),
                                ("grouping_power".to_string(), "5".to_string()),
                                ("max_value_power".to_string(), "64".to_string()),
                            ]
                            .into(),
                        };

                        let mut snapshot: Snapshot = match rmp_serde::from_slice(&body) {
                            Ok(s) => s,
                            Err(e) => {
                                error!("could not decode snapshot from msgpack: {e}");
                                break;
                            }
                        };

                        match snapshot {
                            Snapshot::V1(ref mut s) => {
                                s.histograms.push(histogram);
                            }
                            Snapshot::V2(ref mut s) => {
                                s.histograms.push(histogram);
                            }
                        }

                        match Snapshot::to_msgpack(&snapshot) {
                            Ok(body) => {
                                if let Err(e) = writer.write_all(&body) {
                                    error!("error writing to temporary file: {e}");
                                    std::process::exit(1);
                                }
                            }
                            Err(e) => {
                                error!("failed to serialize snapshot: {e}");
                                break;
                            }
                        }
                    } else if let Err(e) = writer.write_all(&body) {
                        error!("error writing to temporary file: {e}");
                        std::process::exit(1);
                    }
                } else {
                    error!("failed read response. terminating early");
                    break;
                }
            } else {
                error!("failed to get metrics. terminating early");
                break;
            }
        }

        debug!("flushing writer");
        let _ = writer.flush();

        // handle any output format specific transforms
        match config.format {
            Format::Raw => {
                debug!("finished");
            }
            Format::Parquet => {
                debug!("converting temp file to parquet");

                let _ = writer.rewind();

                if let Err(e) = MsgpackToParquet::with_options(ParquetOptions::new())
                    .metadata(
                        "sampling_interval_ms".to_string(),
                        config.interval.as_millis().to_string(),
                    )
                    .convert_file_handle(writer, destination.unwrap())
                {
                    eprintln!("error saving parquet file: {e}");
                }
            }
        }
    })
}
