#[macro_use]
extern crate ringlog;

use std::path::Path;
use backtrace::Backtrace;
use clap::{Arg, Command};

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
        .long_about(
            "Rezolus provides high-resolution systems performance telemetry.",
        )
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

    loop {
        std::thread::sleep(core::time::Duration::from_millis(1));
    }
}

pub struct Config {

}

impl Config {
    pub fn load(file: &dyn AsRef<Path>) -> Result<Self, String> {
        Ok(Self { })
    }
}