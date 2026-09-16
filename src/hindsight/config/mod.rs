use clap::ArgMatches;
use serde::Deserialize;

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

mod general;
mod log;

use general::General;
use log::Log;

fn source() -> String {
    "0.0.0.0:4241".into()
}

fn interval() -> String {
    "1s".into()
}

fn duration() -> String {
    "15m".into()
}

/// Where a snapshot lands when the operator does not choose.
///
/// `/var/lib/rezolus`, per FHS "variable state information" — the conventional
/// home for a daemon's own data, as `/var/lib/prometheus` is. It was
/// `/tmp/rezolus.rez`, which is tmpfs on most modern distributions and cleared
/// at boot.
fn output() -> String {
    format!("{DEFAULT_STATE_DIR}/rezolus.rez")
}

/// The directory the rolling buffer lives in.
///
/// `$STATE_DIRECTORY` when systemd set it — the unit declares
/// `StateDirectory=rezolus`, so systemd creates the directory with ownership
/// matching `User=rezolus` and names it here. Falling back to
/// [`DEFAULT_STATE_DIR`] covers a daemon run by hand.
///
/// **Deliberately not derived from `output`.** It was `dirname(output)`, which
/// conflated two different things: `output` is an artifact the operator keeps
/// and chooses the location of, while the buffer is working state that can be
/// gigabytes and is rewritten continuously. Tying them meant any sensible
/// choice of output path dragged the buffer along with it — and with the old
/// `/tmp` default, dragged a multi-gigabyte rolling buffer into tmpfs, which
/// is RAM. That defeats the reason hindsight exists: it keeps more history
/// than you would hold in memory, at the cost of disk.
fn buffer_dir() -> String {
    std::env::var("STATE_DIRECTORY")
        .ok()
        .filter(|s| !s.is_empty())
        // systemd passes a colon-separated list when several are declared.
        // One is declared, but taking the first is what makes that true rather
        // than assumed.
        .and_then(|s| s.split(':').next().map(str::to_string))
        .unwrap_or_else(|| DEFAULT_STATE_DIR.to_string())
}

/// FHS "variable state information". Used for both the buffer and the default
/// snapshot path when systemd has not named a state directory.
const DEFAULT_STATE_DIR: &str = "/var/lib/rezolus";

#[derive(Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    general: General,
    #[serde(default)]
    log: Log,
}

impl TryFrom<ArgMatches> for Config {
    type Error = String;

    fn try_from(
        args: ArgMatches,
    ) -> Result<Self, <Self as std::convert::TryFrom<clap::ArgMatches>>::Error> {
        let config: PathBuf = args.get_one::<PathBuf>("CONFIG").unwrap().to_path_buf();
        match Config::load(&config) {
            Ok(c) => Ok(c),
            Err(error) => {
                eprintln!("error loading config file: {config:?}\n{error}");
                std::process::exit(1);
            }
        }
    }
}

impl Config {
    pub fn load(path: &dyn AsRef<Path>) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| {
                eprintln!("unable to open config file: {e}");
                std::process::exit(1);
            })
            .unwrap();

        let config: Config = toml::from_str(&content)
            .map_err(|e| {
                eprintln!("failed to parse config file: {e}");
                std::process::exit(1);
            })
            .unwrap();

        config.general.check();

        Ok(config)
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    pub fn general(&self) -> &General {
        &self.general
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The buffer must not default into `/tmp`. It is tmpfs on most modern
    /// distributions, so a rolling buffer sized for a useful lookback would sit
    /// in RAM — which defeats the reason hindsight exists, and competes with
    /// the workload it is recording, on the same host, during the incident.
    #[test]
    fn neither_default_path_lands_in_tmp_or_on_tmpfs() {
        for path in [output(), buffer_dir()] {
            assert!(
                !path.starts_with("/tmp") && !path.starts_with("/run"),
                "{path} is tmpfs on most distributions"
            );
        }
    }

    /// `output` and `buffer_dir` are different kinds of thing — an artifact the
    /// operator keeps, and working state — so neither may be derived from the
    /// other. Pinned because the old code took `dirname(output)`, which is what
    /// dragged a multi-gigabyte buffer wherever the snapshot was pointed.
    #[test]
    fn the_buffer_directory_is_not_derived_from_the_output_path() {
        let cfg: Config = toml::from_str("[general]\noutput = \"/srv/captures/incident.rez\"\n")
            .expect("config parses");
        assert_eq!(
            cfg.general().buffer_dir(),
            PathBuf::from(buffer_dir()),
            "moving `output` must not move the buffer"
        );
    }

    /// systemd names the state directory it created, and it must win over the
    /// compiled-in default — that is the whole mechanism by which a non-root
    /// service gets a writable directory under /var/lib.
    #[test]
    fn systemd_state_directory_is_honored_and_a_list_takes_the_first() {
        // Serialized against the other env-reading test by running both
        // assertions here: `std::env::set_var` is process-global, and these
        // would otherwise race under `cargo test`'s parallelism.
        temp_env_var("STATE_DIRECTORY", Some("/var/lib/rezolus"), || {
            assert_eq!(buffer_dir(), "/var/lib/rezolus");
        });
        // systemd passes a colon-separated list when several are declared.
        temp_env_var(
            "STATE_DIRECTORY",
            Some("/var/lib/first:/var/lib/second"),
            || {
                assert_eq!(buffer_dir(), "/var/lib/first");
            },
        );
        // Empty is not a path; fall back rather than creating a buffer at "".
        temp_env_var("STATE_DIRECTORY", Some(""), || {
            assert_eq!(buffer_dir(), DEFAULT_STATE_DIR);
        });
        temp_env_var("STATE_DIRECTORY", None, || {
            assert_eq!(buffer_dir(), DEFAULT_STATE_DIR);
        });
    }

    /// Set (or clear) an environment variable for the duration of `f`, then put
    /// it back. `set_var` is unsafe and process-global; this keeps the blast
    /// radius to one closure.
    fn temp_env_var(key: &str, value: Option<&str>, f: impl FnOnce()) {
        let previous = std::env::var(key).ok();
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        f();
        unsafe {
            match previous {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}
