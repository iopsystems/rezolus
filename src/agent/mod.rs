use super::*;

mod config;
mod exposition;
mod external_metrics;
mod metrics;
pub mod sampler_status;
mod samplers;

use config::Config;
use external_metrics::{ExternalMetricsStore, Protocol, ServerState};
use samplers::{Sampler, SamplerResult, SAMPLERS};

use std::sync::OnceLock;
use std::time::Instant;

/// Process start time, recorded once at the top of `run()`. Read by the
/// `/status` endpoint to report uptime.
static AGENT_START: OnceLock<Instant> = OnceLock::new();

/// Record the agent's start time. Idempotent; the first call wins.
fn record_agent_start() {
    let _ = AGENT_START.set(Instant::now());
}

/// Seconds since the agent started, or 0 if never recorded (e.g. a unit test
/// that does not call `run()`).
pub(crate) fn agent_uptime_seconds() -> u64 {
    AGENT_START
        .get()
        .map(|start| start.elapsed().as_secs())
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
use metrics::GroupMetadata;

#[cfg(target_os = "linux")]
mod bpf;

#[cfg(target_os = "linux")]
use bpf::*;

#[cfg(target_os = "linux")]
pub use bpf::{kernel_has_btf, process_cgroup_info, CgroupInfo};

// This is the maximum number of CPUs we track with BPF counters.
pub const MAX_CPUS: usize = 1024;

// This is the maximum number of cgroups we track with BPF counters.
pub const MAX_CGROUPS: usize = 4096;

// This is the maximum PID we track with BPF counters.
pub const MAX_PID: usize = 4194304;

/// Runs Rezolus in `agent` mode in which it gathers systems telemetry and
/// exposes metrics on an OTel/Prometheus compatible endpoint and a
/// Rezolus-specific msgpack endpoint.
///
/// This is the default mode for running Rezolus.
pub fn run(config: PathBuf) {
    record_agent_start();

    let config: Arc<Config> = {
        debug!("loading config: {config:?}");
        match Config::load(&config) {
            Ok(c) => c.into(),
            Err(error) => {
                eprintln!("error loading config file: {config:?}\n{error}");
                std::process::exit(1);
            }
        }
    };

    #[cfg(target_os = "linux")]
    config.scheduler().apply();

    let _log_drain = configure_logging(config.log().level().to_tracing_level());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    let mut samplers = Vec::new();

    for entry in SAMPLERS {
        match (entry.init)(config.clone()) {
            Ok(Some(s)) => {
                // BPF samplers already recorded active + per-program detail in
                // the builder; this only fills in non-BPF samplers.
                crate::agent::sampler_status::set_active_if_absent(entry.name);
                samplers.push(s);
            }
            Ok(None) => crate::agent::sampler_status::set_disabled(entry.name),
            Err(e) => crate::agent::sampler_status::set_failed(entry.name, e.to_string()),
        }
    }

    log_sampler_health_summary();

    let samplers = Arc::new(samplers.into_boxed_slice());

    // Initialize external metrics store if enabled
    let external_store = if config.external_metrics().enabled() {
        // Build set of reserved (internal) metric names to prevent collisions
        let reserved_names: std::collections::HashSet<String> = metriken::metrics()
            .iter()
            .map(|m| m.name().to_string())
            .collect();

        debug!(
            "external metrics: {} internal metric names reserved",
            reserved_names.len()
        );

        let store = Arc::new(ExternalMetricsStore::new(
            config.external_metrics().metric_ttl(),
            config.external_metrics().max_metrics(),
            reserved_names,
        ));

        let protocol = Protocol::from_str(config.external_metrics().protocol())
            .expect("invalid protocol (should have been caught by config validation)");

        let server_state = Arc::new(ServerState::new(
            Arc::clone(&store),
            protocol,
            config.external_metrics().max_connections(),
            config.external_metrics().max_metrics_per_connection(),
        ));

        let socket_path = config.external_metrics().socket_path().clone();
        let socket_group = config.external_metrics().socket_group().map(String::from);
        let socket_mode = config.external_metrics().socket_mode();
        rt.spawn(async move {
            if let Err(e) = external_metrics::serve(
                &socket_path,
                server_state,
                socket_group.as_deref(),
                socket_mode,
            )
            .await
            {
                error!("external metrics server error: {}", e);
            }
        });

        Some(store)
    } else {
        None
    };

    rt.spawn(async move {
        exposition::http::serve(config, samplers, external_store).await;
    });

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// Emit a single classified summary of sampler health after init, mirroring
/// the `/samplers` endpoint. Replaces scattered per-probe attach warnings:
/// degraded/failed at `warn!`, unsupported at `info!`, plus a one-line tally.
fn log_sampler_health_summary() {
    use crate::agent::sampler_status::{ProbeVerdict, SamplerHealth, SamplerState};

    let statuses = crate::agent::sampler_status::snapshot();
    let mut healthy = 0usize;
    let mut unsupported = 0usize;
    let mut degraded = 0usize;
    let mut failed = 0usize;

    for s in &statuses {
        match (&s.state, s.health) {
            (SamplerState::Failed { error }, _) => {
                failed += 1;
                warn!("sampler {}: failed — {}", s.name, error);
            }
            (_, Some(SamplerHealth::Failed)) => {
                failed += 1;
                warn!("sampler {}: failed", s.name);
            }
            (_, Some(SamplerHealth::Degraded)) => {
                degraded += 1;
                let probes: Vec<String> = s
                    .programs
                    .iter()
                    .filter(|p| p.verdict == ProbeVerdict::Broken)
                    .map(|p| {
                        format!(
                            "{} ({})",
                            p.label.as_deref().unwrap_or(&p.name),
                            p.error.as_deref().unwrap_or("not attached")
                        )
                    })
                    .collect();
                warn!("sampler {}: degraded — {}", s.name, probes.join(", "));
            }
            (_, Some(SamplerHealth::Unsupported)) => {
                unsupported += 1;
                let probes: Vec<String> = s
                    .programs
                    .iter()
                    .filter(|p| p.verdict == ProbeVerdict::Unsupported)
                    .map(|p| {
                        format!(
                            "{} (no kernel support)",
                            p.label.as_deref().unwrap_or(&p.name)
                        )
                    })
                    .collect();
                info!("sampler {}: unsupported — {}", s.name, probes.join(", "));
            }
            (_, Some(SamplerHealth::Healthy)) => healthy += 1,
            (SamplerState::Disabled, None) => {}
            (SamplerState::Active, None) => healthy += 1, // non-BPF sampler
        }
    }

    info!(
        "samplers: {} healthy, {} unsupported, {} degraded, {} failed",
        healthy, unsupported, degraded, failed
    );
}
