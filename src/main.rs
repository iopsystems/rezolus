use backtrace::Backtrace;
use clap::{Arg, Command};
use linkme::distributed_slice;
use metriken::Lazy;
use metriken_exposition::HistogramSnapshot;
use ringlog::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

type HistogramSnapshots = HashMap<String, HistogramSnapshot>;

static SNAPSHOTS: Lazy<Arc<RwLock<Snapshots>>> =
    Lazy::new(|| Arc::new(RwLock::new(Snapshots::new())));

pub struct Snapshots {
    timestamp: SystemTime,
    previous: HistogramSnapshots,
    delta: HistogramSnapshots,
}

impl Default for Snapshots {
    fn default() -> Self {
        Self::new()
    }
}

impl Snapshots {
    pub fn new() -> Self {
        let snapshot = metriken_exposition::Snapshotter::default().snapshot();

        let previous: HistogramSnapshots = snapshot.histograms.into_iter().collect();
        let delta = previous.clone();

        Self {
            timestamp: snapshot.systemtime,
            previous,
            delta,
        }
    }

    pub fn update(&mut self) {
        let snapshot = metriken_exposition::Snapshotter::default().snapshot();
        self.timestamp = snapshot.systemtime;

        let current: HistogramSnapshots = snapshot.histograms.into_iter().collect();
        let mut delta = HistogramSnapshots::new();

        for (name, previous) in &self.previous {
            if let Some(histogram) = current.get(name).cloned() {
                let _ = histogram.wrapping_sub(previous);
                delta.insert(name.to_string(), histogram);
            }
        }

        self.previous = current;
        self.delta = delta;
    }
}

mod common;
mod config;
mod exposition;
mod samplers;

use config::Config;

pub static PERCENTILES: &[(&str, f64)] = &[
    ("p25", 25.0),
    ("p50", 50.0),
    ("p75", 75.0),
    ("p90", 90.0),
    ("p99", 99.0),
    ("p999", 99.9),
    ("p9999", 99.99),
];

#[distributed_slice]
pub static SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];

counter!(RUNTIME_SAMPLE_LOOP, "runtime/sample/loop");

fn main() {
    // custom panic hook to terminate whole process after unwinding
    std::panic::set_hook(Box::new(|s| {
        eprintln!("{s}");
        eprintln!("{:?}", Backtrace::new());
        std::process::exit(101);
    }));

    // parse command line options
    let matches = Command::new(env!("CARGO_BIN_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .long_about("Rezolus provides high-resolution systems performance telemetry.")
        .arg(
            Arg::new("CONFIG")
                .help("Server configuration file")
                .action(clap::ArgAction::Set)
                .required(true)
                .index(1),
        )
        .get_matches();

    // load config from file
    let config: Arc<Config> = {
        let file = matches.get_one::<String>("CONFIG").unwrap();
        debug!("loading config: {}", file);
        match Config::load(file) {
            Ok(c) => c.into(),
            Err(error) => {
                eprintln!("error loading config file: {file}\n{error}");
                std::process::exit(1);
            }
        }
    };

    // configure debug log
    let debug_output: Box<dyn Output> = Box::new(Stderr::new());

    let level = Level::Info;

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
        .build()
        .expect("failed to launch async runtime");

    // spawn logging thread
    rt.spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let _ = log.flush();
        }
    });

    // spawn thread to maintain histogram snapshots
    rt.spawn(async {
        loop {
            // acquire a lock and update the snapshots
            {
                let mut snapshots = SNAPSHOTS.write().await;
                snapshots.update();
            }

            // delay until next update
            tokio::time::sleep(core::time::Duration::from_secs(1)).await;
        }
    });

    info!("rezolus");

    // spawn http exposition thread
    rt.spawn(exposition::http(config.clone()));

    // initialize and gather the samplers
    let mut samplers: Vec<Box<dyn Sampler>> = Vec::new();

    for sampler in SAMPLERS {
        samplers.push(sampler(&config));
    }

    info!("initialization complete");

    // main loop
    loop {
        RUNTIME_SAMPLE_LOOP.increment();

        // get current time
        let start = Instant::now();

        // sample each sampler
        for sampler in &mut samplers {
            sampler.sample();
        }

        // Sleep for the remainder of one millisecond minus the sampling time.
        // This wakeup period allows a maximum of 1kHz sampling
        let delay = Duration::from_millis(1).saturating_sub(start.elapsed());
        std::thread::sleep(delay);
    }
}

pub trait Sampler {
    /// Do some sampling and updating of stats
    fn sample(&mut self);
}
