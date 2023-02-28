use warp::Filter;
use ringlog::*;

use metriken::Lazy;



use backtrace::Backtrace;
use clap::{Arg, Command};
use std::fmt::Display;
// use std::marker::PhantomData;
use std::path::Path;
// use std::time::Duration;
// use std::time::Instant;

use linkme::distributed_slice;
// use common::{counter, counter_with_heatmap, heatmap};

type Duration = clocksource::Duration<clocksource::Nanoseconds<u64>>;
type Instant = clocksource::Instant<clocksource::Nanoseconds<u64>>;

mod admin;
mod common;
mod samplers;

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
pub static CLASSIC_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];

#[distributed_slice]
pub static BPF_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];

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

    // initialize async runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .build().expect("failed to launch async runtime");

    rt.spawn({
        admin::http()
    });

    // initialize and gather the samplers
    let mut classic_samplers: Vec<Box<dyn Sampler>> = Vec::new();

    for sampler in CLASSIC_SAMPLERS {
        classic_samplers.push(sampler(&config));
    }

    let mut bpf_samplers: Vec<Box<dyn Sampler>> = Vec::new();

    for sampler in BPF_SAMPLERS {
        bpf_samplers.push(sampler(&config));
    }

    // main loop
    loop {
        RUNTIME_SAMPLE_LOOP.increment();

        // get current time
        let start = Instant::now();

        // sample each sampler
        for sampler in &mut classic_samplers {
            sampler.sample();
        }

        for sampler in &mut bpf_samplers {
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

pub trait Sampler: Display {
    // #[allow(clippy::result_unit_err)]
    // fn configure(&self, config: &Config) -> Result<(), ()>;

    /// Do some sampling and updating of stats
    fn sample(&mut self);
}

pub struct Config {}

impl Config {
    pub fn load(_file: &dyn AsRef<Path>) -> Result<Self, String> {
        Ok(Self {})
    }
}

