#[macro_use]
extern crate ringlog;

use metriken::Counter;
use metriken::Gauge;
use metriken::Heatmap;
use backtrace::Backtrace;
use clap::{Arg, Command};
use std::fmt::Display;
// use std::marker::PhantomData;
use std::path::Path;
// use std::time::Duration;
// use std::time::Instant;

use linkme::distributed_slice;

type Duration = clocksource::Duration<clocksource::Nanoseconds<u64>>;
type Instant = clocksource::Instant<clocksource::Nanoseconds<u64>>;

mod common;
mod samplers;

const PERCENTILES: &[f64] = &[0.50, 0.90, 0.99];

#[distributed_slice]
pub static SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];

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

    // initialize and gather the samplers
    let mut samplers: Vec<Box<dyn Sampler>> = Vec::new();

    for sampler in SAMPLERS {
        samplers.push(sampler(&config));
    }

    std::thread::spawn(|| {
        loop {
            admin();
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });

    // main loop
    loop {
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
        // this wakeup period allows 1kHz sampling
        let sleep = 1_000_000 - elapsed;
        std::thread::sleep(std::time::Duration::from_nanos(sleep));
    }
}

pub fn admin() {
    let mut data = Vec::new();

    for metric in &metriken::metrics() {
        let any = match metric.as_any() {
            Some(any) => any,
            None => {
                continue;
            }
        };

        if let Some(counter) = any.downcast_ref::<Counter>() {
            data.push(format!("{}: {}", metric.name(), counter.value()));
        } else if let Some(gauge) = any.downcast_ref::<Gauge>() {
            data.push(format!("{}: {}", metric.name(), gauge.value()));
        } else if let Some(heatmap) = any.downcast_ref::<Heatmap>() {
            for p in PERCENTILES {
                let percentile = heatmap.percentile(*p).map(|b| b.high()).unwrap_or(0);
                data.push(format!("{}_p({:.2}): {}", metric.name(), p, percentile));
            }
        }
    }

    data.sort();

    for line in data {
        println!("{}", line);
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

