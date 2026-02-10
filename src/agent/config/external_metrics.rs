use serde::Deserialize;
use std::ffi::CString;
use std::path::PathBuf;
use std::time::Duration;

fn default_socket_path() -> PathBuf {
    PathBuf::from("/var/run/rezolus/external.sock")
}

fn default_protocol() -> String {
    "auto".into()
}

fn default_metric_ttl() -> String {
    "60s".into()
}

fn default_max_connections() -> usize {
    1000
}

fn default_max_metrics() -> usize {
    100000
}

fn default_max_metrics_per_connection() -> usize {
    10000
}

#[derive(Deserialize, Default)]
pub struct ExternalMetrics {
    #[serde(default)]
    enabled: bool,

    #[serde(default = "default_socket_path")]
    socket_path: PathBuf,

    #[serde(default = "default_protocol")]
    protocol: String,

    #[serde(default = "default_metric_ttl")]
    metric_ttl: String,

    #[serde(default = "default_max_connections")]
    max_connections: usize,

    #[serde(default = "default_max_metrics")]
    max_metrics: usize,

    #[serde(default = "default_max_metrics_per_connection")]
    max_metrics_per_connection: usize,

    #[serde(default)]
    socket_group: Option<String>,

    #[serde(default)]
    socket_mode: Option<u32>,
}

impl ExternalMetrics {
    pub fn check(&self) {
        if !self.enabled {
            return;
        }

        // Validate metric_ttl
        if let Err(e) = self.metric_ttl.parse::<humantime::Duration>() {
            eprintln!("external_metrics.metric_ttl couldn't be parsed: {e}");
            std::process::exit(1);
        }

        // Validate protocol
        let valid_protocols = ["binary", "line", "auto"];
        if !valid_protocols.contains(&self.protocol.to_lowercase().as_str()) {
            eprintln!(
                "external_metrics.protocol must be one of: {}",
                valid_protocols.join(", ")
            );
            std::process::exit(1);
        }

        // Validate max_connections
        if self.max_connections == 0 {
            eprintln!("external_metrics.max_connections must be greater than 0");
            std::process::exit(1);
        }

        // Validate max_metrics
        if self.max_metrics == 0 {
            eprintln!("external_metrics.max_metrics must be greater than 0");
            std::process::exit(1);
        }

        // Validate max_metrics_per_connection
        if self.max_metrics_per_connection == 0 {
            eprintln!("external_metrics.max_metrics_per_connection must be greater than 0");
            std::process::exit(1);
        }

        // Validate socket_group
        if let Some(ref group) = self.socket_group {
            let c_group = match CString::new(group.as_str()) {
                Ok(s) => s,
                Err(_) => {
                    eprintln!("external_metrics.socket_group contains invalid characters");
                    std::process::exit(1);
                }
            };
            unsafe {
                if libc::getgrnam(c_group.as_ptr()).is_null() {
                    eprintln!("external_metrics.socket_group: group '{group}' not found");
                    std::process::exit(1);
                }
            }
        }

        // Validate socket_mode
        if let Some(mode) = self.socket_mode {
            if mode > 0o777 {
                eprintln!("external_metrics.socket_mode must be <= 0o777");
                std::process::exit(1);
            }
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    pub fn metric_ttl(&self) -> Duration {
        *self.metric_ttl.parse::<humantime::Duration>().unwrap()
    }

    pub fn max_connections(&self) -> usize {
        self.max_connections
    }

    pub fn max_metrics(&self) -> usize {
        self.max_metrics
    }

    pub fn max_metrics_per_connection(&self) -> usize {
        self.max_metrics_per_connection
    }

    pub fn socket_group(&self) -> Option<&str> {
        self.socket_group.as_deref()
    }

    pub fn socket_mode(&self) -> Option<u32> {
        self.socket_mode
    }
}
