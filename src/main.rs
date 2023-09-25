use backtrace::Backtrace;
use clap::{Arg, Command};
use linkme::distributed_slice;
use metriken::Lazy;
use ringlog::*;

type Duration = clocksource::Duration<clocksource::Nanoseconds<u64>>;
type Instant = clocksource::Instant<clocksource::Nanoseconds<u64>>;

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
    let config = {
        let file = matches.get_one::<String>("CONFIG").unwrap();
        debug!("loading config: {}", file);
        match Config::load(file) {
            Ok(c) => c,
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

    info!("rezolus");

    // spawn http exposition thread
    rt.spawn(exposition::http());

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

        // calculate how long we took during this iteration
        let stop = Instant::now();
        let elapsed = (stop - start).as_nanos();

        // calculate how long to sleep and sleep before next iteration
        // this wakeup period allows a maximum of 1kHz sampling
        let sleep = 1_000_000_u64.saturating_sub(elapsed);
        std::thread::sleep(std::time::Duration::from_nanos(sleep));
    }
}

pub trait Sampler {
    /// Do some sampling and updating of stats
    fn sample(&mut self);
}
