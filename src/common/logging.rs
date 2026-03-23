use tracing_appender::non_blocking::WorkerGuard;

/// Holds the tracing worker guard. Must be kept alive for the process lifetime
/// to ensure logs are flushed.
pub struct LogDrain {
    _guard: WorkerGuard,
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
