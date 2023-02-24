#[macro_use]
extern crate ringlog;

use backtrace::Backtrace;
use clap::{Arg, Command};
use std::fmt::Display;
use std::marker::PhantomData;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use linkme::distributed_slice;

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

    let mut samplers: Vec<Box<dyn Sampler>> = Vec::new();

    for sampler in SAMPLERS {
        samplers.push(sampler(&config));
    }

    loop {
        let start = Instant::now();

        for sampler in &mut samplers {
            sampler.sample(Instant::now());
        }

        let stop = Instant::now();

        let elapsed = stop - start;

        let sleep = Duration::from_millis(1) - elapsed;

        // for sampler in SAMPLERS {
        //     sampler.sample(now);
        // }

        std::thread::sleep(sleep);
    }
}

pub trait Sampler: Display {
    // #[allow(clippy::result_unit_err)]
    // fn configure(&self, config: &Config) -> Result<(), ()>;

    /// Do some sampling and updating of stats
    fn sample(&mut self, now: Instant);
}

pub struct Config {}

impl Config {
    pub fn load(_file: &dyn AsRef<Path>) -> Result<Self, String> {
        Ok(Self {})
    }
}

#[distributed_slice(SAMPLERS)]
fn tcp(config: &Config) -> Box<dyn Sampler> {
    Box::new(Tcp::new(config))
}

pub struct Tcp<'a> {
    next: Instant,
    interval: Duration,
    _traffic_stats: TrafficStats<'a>,
}

impl<'a> Tcp<'a> {
    fn new(_config: &Config) -> Self {
        Self {
            next: Instant::now(),
            interval: Duration::from_millis(100),
            _traffic_stats: TrafficStats {
                _lifetime: PhantomData,
            },
        }
    }
}

impl Display for Tcp<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "tcp")
    }
}

impl<'a> Sampler for Tcp<'a> {
    fn sample(&mut self, now: Instant) {
        if now < self.next {
            return;
        }

        self.next += self.interval;
    }
}

#[cfg(not(feature = "bpf"))]
pub struct TrafficStats<'a> {
    _lifetime: PhantomData<&'a ()>,
}

#[cfg(feature = "bpf")]
pub struct TrafficStats<'a> {
    _lifetime: PhantomData<&'a ()>,
}
