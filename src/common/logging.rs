use serde::Deserialize;
use tracing_appender::non_blocking::WorkerGuard;

#[derive(Copy, Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
#[serde(deny_unknown_fields)]
pub enum Level {
    Error,
    Warn,
    #[default]
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

#[derive(Default, Deserialize)]
pub struct LogConfig {
    #[serde(default)]
    level: Level,
}

impl LogConfig {
    pub fn level(&self) -> Level {
        self.level
    }
}

/// Holds the tracing worker guard. Must be kept alive for the process lifetime
/// to ensure logs are flushed.
pub struct LogDrain {
    _guard: WorkerGuard,
}

/// Map CLI verbosity flags (-v, -vv) to a tracing level.
pub fn verbosity_to_level(verbose: u8) -> tracing::Level {
    match verbose {
        0 => tracing::Level::INFO,
        1 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    }
}

/// Initialize tracing with a non-blocking stderr writer at the given level.
pub fn configure_logging(level: tracing::Level) -> LogDrain {
    let (non_blocking, guard) = tracing_appender::non_blocking(std::io::stderr());

    let subscriber = tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_max_level(level)
        .with_target(true)
        .with_ansi(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("failed to set tracing subscriber");

    // Bridge any transitive `log` crate users into tracing
    let _ = tracing_log::LogTracer::init();

    LogDrain { _guard: guard }
}
