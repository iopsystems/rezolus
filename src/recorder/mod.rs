use std::collections::VecDeque;
use notify::EventKind;
use notify::Watcher;
use notify::RecursiveMode;
use notify::Event;
use std::sync::mpsc::Receiver;
use notify::RecommendedWatcher;
use std::path::Path;
use memmap2::Mmap;
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
            eprintln!("error creating http client: {e}");
            std::process::exit(1);
        }
    };

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
                    eprintln!("could not open temporary file in: {:?}\n{error}", path);
                    std::process::exit(1);
                }
            }
        }
    };

    // open any memory mapped sources
    let mut memmap_source = if let Some(path) = config.mmap {
        if path.is_file() {
            match MemmapFile::new(path.clone()) {
                Ok(f) => Some(MemmapSource::File(f)),
                Err(e) => {
                    eprintln!(
                        "failed to open specified memory mapped file: {:?}\n{e}",
                        path
                    );
                    std::process::exit(1);
                }
            }
        } else if path.is_dir() {
            match MemmapWatcher::new(&path) {
                Ok(f) => Some(MemmapSource::Directory(f)),
                Err(e) => {
                    eprintln!(
                        "failed to open watcher for memory mapped files: {:?}\n{e}",
                        path
                    );
                    std::process::exit(1);
                }
            }
        } else {
            eprintln!(
                "mmap argument was not a directory or file: {:?}\n",
                path
            );
            std::process::exit(1);
        }
    } else {
        None
    };

    // test connectivity to the agent
    {
        let client = client.clone();
        let url = url.clone();

        rt.block_on(async move {
            let start = Instant::now();

            // sample rezolus to make sure:
            // * we can reach it
            // * we get a response
            // * the sample interval is not too close to the sample latency
            if let Ok(response) = client.get(url.clone()).send().await {
                if let Ok(_body) = response.bytes().await {
                    let latency = start.elapsed();

                    if latency.as_nanos() >= config.interval.as_nanos() {
                        let recommended = humantime::Duration::from(Duration::from_millis((latency * 2).as_nanos().div_ceil(1000000) as u64));

                        eprintln!("sampling latency ({} us) exceeded the sample interval. Try setting the interval to: {}", latency.as_micros(), recommended);
                        std::process::exit(1);
                    } else if latency.as_nanos() >= (3 * config.interval.as_nanos() / 4) {
                        warn!("sampling latency ({} us) is more that 75% of the sample interval. Consider increasing the interval", latency.as_micros());
                    } else {
                        debug!("sampling latency: {} us", latency.as_micros());
                    }
                } else {
                    eprintln!("failed read response. Please check that the source address is correct");
                    std::process::exit(1);
                }
            } else {
                eprintln!("failed to connect. Please check that the agent is running and that the source address is correct");
                std::process::exit(1);
            }
        });
    }

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

            let start = Instant::now();

            let mut histograms = Vec::new();

            match memmap_source {
                Some(MemmapSource::File(ref f)) => {
                    let (_prefix, buckets, _suffix) = unsafe { f.mmap.align_to::<u64>() };

                    // hardcoded for a histogram with grouping power of 5 and max value power of 64
                    let histogram = Histogram::from_buckets(5, 64, buckets[0..1920].to_vec()).unwrap();

                    let histogram = metriken_exposition::Histogram {
                        name: "frame_start_delay".to_string(),
                        value: histogram,
                        metadata: [("metric".to_string(), "frame_start_delay".to_string()), ("grouping_power".to_string(), "5".to_string()), ("max_value_power".to_string(), "64".to_string()), ("name".to_string(), f.name.clone())].into(),
                    };

                    histograms.push(histogram);
                }
                Some(MemmapSource::Directory(ref mut w)) => {
                    for f in w.mapped_files.values() {
                        let (_prefix, buckets, _suffix) = unsafe { f.mmap.align_to::<u64>() };

                        // hardcoded for a histogram with grouping power of 5 and max value power of 64
                        let histogram = Histogram::from_buckets(5, 64, buckets[0..1920].to_vec()).unwrap();

                        let histogram = metriken_exposition::Histogram {
                            name: "frame_start_delay".to_string(),
                            value: histogram,
                            metadata: [("metric".to_string(), "frame_start_delay".to_string()), ("grouping_power".to_string(), "5".to_string()), ("max_value_power".to_string(), "64".to_string()), ("name".to_string(), f.name.clone())].into(),
                        };

                        histograms.push(histogram);
                    }
                }
                _ => {}
            }

            // sample rezolus
            if let Ok(response) = client.get(url.clone()).send().await {
                if let Ok(body) = response.bytes().await {
                    let latency = start.elapsed();

                    if latency.as_nanos() >= config.interval.as_nanos() {
                        error!("sampling latency ({} us) exceeded the sample interval. Samples will be missing", latency.as_micros());
                   } else if latency.as_nanos() >= (3 * config.interval.as_nanos() / 4) {
                        warn!("sampling latency ({} us) is more that 75% of the sample interval. Consider increasing the interval", latency.as_micros());
                    } else {
                        debug!("sampling latency: {} us", latency.as_micros());
                    }

                    if histograms.is_empty() {
                        if let Err(e) = writer.write_all(&body) {
                            eprintln!("error writing to temporary file: {e}");
                            std::process::exit(1);
                        }
                    } else {
                        let mut snapshot: Snapshot = match rmp_serde::from_slice(&body) {
                            Ok(s) => s,
                            Err(e) => {
                                error!("could not decode snapshot from msgpack: {e}");
                                break;
                            }
                        };

                        match snapshot {
                            Snapshot::V1(ref mut s) => {
                                for histogram in histograms.drain(..) {
                                    s.histograms.push(histogram);
                                }
                            }
                            Snapshot::V2(ref mut s) => {
                                for histogram in histograms.drain(..) {
                                    s.histograms.push(histogram);
                                }
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
                    }
                } else {
                    eprintln!("failed read response. terminating early");
                    break;
                }
            } else {
                eprintln!("failed to get metrics. terminating early");
                break;
            }

            // update the mmap directory after sampling to avoid introducing skew
            if let Some(MemmapSource::Directory(ref mut w)) = memmap_source {
                w.process_events();
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

enum MemmapSource {
    File(MemmapFile),
    Directory(MemmapWatcher),
}

#[derive(Debug)]
struct MemmapFile {
    // pub path: PathBuf,
    name: String,
    mmap: Mmap,
}

impl MemmapFile {
    fn new(path: PathBuf) -> Result<Self, Box<dyn std::error::Error>> {
        let file = std::fs::File::open(&path)?;

        let mmap = unsafe {
            MmapOptions::new().map(&file).map_err(|e| {
                error!("failed to mmap the file: {:?}\n{e}", path);
                e
            })?
        };

        // Check the alignment
        let (_prefix, data, _suffix) = unsafe { mmap.align_to::<u64>() };

        // Hardcoded for a histogram with grouping power of 5 and max value power of 64
        if data.len() != 1920 {
            return Err(format!(
                "mmap region not aligned or width doesn't match. Expected 1920, got {}",
                data.len()
            ).into());
        }

        Ok(Self {
            name: path.file_stem().unwrap().to_string_lossy().to_string(),
            mmap,
        })
    }
}

struct MemmapWatcher {
    watch_path: PathBuf,
    mapped_files: HashMap<PathBuf, MemmapFile>,
    _watcher: RecommendedWatcher, // Keep watcher alive
    event_receiver: Receiver<Event>,
    backlog: VecDeque<PathBuf>,
}

impl MemmapWatcher {
    fn new<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let watch_path = path.as_ref().to_path_buf();

        // Set up file system watcher
        let (tx, rx) = std::sync::mpsc::channel();

        let watcher = RecommendedWatcher::new(
            move |result: Result<Event, notify::Error>| {
                match result {
                    Ok(event) => {
                        if let Err(e) = tx.send(event) {
                            error!("Error sending event: {}", e);
                        }
                    }
                    Err(e) => error!("Watch error: {}", e),
                }
            },
            Default::default(),
        )?;

        let mut directory_watcher = Self {
            watch_path: watch_path.clone(),
            mapped_files: HashMap::new(),
            _watcher: watcher,
            event_receiver: rx,
            backlog: Default::default(),
        };

        // Process existing files
        directory_watcher.process_existing_files()?;

        // Start watching the directory
        directory_watcher._watcher.watch(&watch_path, RecursiveMode::NonRecursive)?;

        Ok(directory_watcher)
    }

    fn process_existing_files(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let entries = std::fs::read_dir(&self.watch_path)?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                self.try_add_file(path);
            }
        }

        Ok(())
    }

    fn try_add_file(&mut self, path: PathBuf) {
        // Skip if already mapped
        if self.mapped_files.contains_key(&path) {
            return;
        }

        match MemmapFile::new(path.clone()) {
            Ok(mapped_file) => {
                self.mapped_files.insert(path, mapped_file);
            }
            Err(e) => {
                error!("Failed to map file {}: {}", path.display(), e);
            }
        }
    }

    fn remove_file(&mut self, path: &Path) {
        let _ = self.mapped_files.remove(path);
    }

    pub fn process_events(&mut self) {
        while let Ok(event) = self.event_receiver.try_recv() {
            self.handle_file_event(event);
        }

        for _ in 0..self.backlog.len() {
            if let Some(path) = self.backlog.pop_front() {
                if path.is_file() && path.exists() {
                    if self.is_file_ready(&path) {
                        self.try_add_file(path);
                    } else {
                        self.backlog.push_back(path);
                    }
                }
            }
        }
    }

    fn handle_file_event(&mut self, event: Event) {
        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) => {
                for path in event.paths {
                    if path.is_file() && path.exists() {
                        if self.is_file_ready(&path) {
                            self.try_add_file(path);
                        } else {
                            self.backlog.push_back(path);
                        }
                    }
                }
            }
            EventKind::Remove(_) => {
                for path in event.paths {
                    self.remove_file(&path);
                }
            }
            _ => {
                // Handle other events if needed
            }
        }
    }

    fn is_file_ready(&self, path: &Path) -> bool {
        std::fs::File::open(path).is_ok()
    }
}
