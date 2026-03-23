use serde::Deserialize;

#[derive(Copy, Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
#[serde(deny_unknown_fields)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    pub fn to_tracing_level(self) -> tracing::Level {
        match self {
            Level::Error => tracing::Level::ERROR,
            Level::Warn => tracing::Level::WARN,
            Level::Info => tracing::Level::INFO,
            Level::Debug => tracing::Level::DEBUG,
            Level::Trace => tracing::Level::TRACE,
        }
    }
}

fn log_level() -> Level {
    Level::Info
}

#[derive(Deserialize)]
pub struct Log {
    #[serde(default = "log_level")]
    level: Level,
}

impl Default for Log {
    fn default() -> Self {
        Self { level: log_level() }
    }
}

impl Log {
    pub fn level(&self) -> Level {
        self.level
    }
}
