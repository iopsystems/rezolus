use super::*;

mod config;
mod exposition;
mod external_metrics;
mod metrics;
pub mod sampler_status;
mod samplers;
mod timing;

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

pub mod pmu;

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

    // Decide the PMU allocation BEFORE anything opens an event, because a
    // pinned event that loses the race is not refused — it opens, never gets
    // scheduled, and reads nothing. Ranking here rather than inside the
    // samplers is also what makes one budget cover both ways they open events:
    // `cpu_perf` goes through the BPF builder, the other four call
    // `perf_event::Builder` directly, and a refused sampler simply never runs
    // its `init`, so it cannot open anything either way.
    let available = crate::agent::pmu::probe_available();
    let reserved = config.general().reserved_pmu_counters();
    let order = crate::agent::pmu::resolve_order(config.general().pmu_priority());
    // Which CPUs give up counters. Unset means all of them, which is what the
    // agent did before the mask existed.
    let cpu_split = crate::agent::pmu::cpu_split(config.general().reserved_pmu_cpus());
    let pmu_plan = crate::agent::pmu::plan(&order, available, reserved, &cpu_split);
    crate::agent::pmu::publish_allocation(&pmu_plan);

    // Core counters only: uncore and MSR samplers draw on separate hardware, so
    // counting their events here would report a shortage that does not exist.
    let demand = crate::agent::pmu::core_demand(&order);
    if demand > available.saturating_sub(reserved) {
        info!(
            "core PMU budget {available}/cpu ({reserved} reserved), core demand {demand}/cpu — not every PMU sampler can run; see /samplers"
        );
    }

    let mut samplers = Vec::new();
    let mut live: std::collections::HashSet<&'static str> = std::collections::HashSet::new();

    for entry in SAMPLERS {
        if let Some(grant) = pmu_plan.iter().find(|g| g.sampler == entry.name) {
            if let Some((cpus, total)) = grant.partial {
                // Runs, but not everywhere. Recorded before `init` so the
                // reason survives whatever the sampler reports about itself.
                crate::agent::sampler_status::set_pmu_limited(entry.name, cpus, total);
            }
            if !grant.granted {
                // Skipped, not failed: nothing broke, there were not enough
                // counters. Reported with the numbers so an operator can see
                // the trade being made and re-rank if they disagree with it.
                crate::agent::sampler_status::set_pmu_starved(
                    entry.name,
                    grant.wants,
                    grant.free_at_decision,
                );
                continue;
            }
        }

        match (entry.init)(config.clone()) {
            Ok(Some(s)) => {
                // BPF samplers already recorded active + per-program detail in
                // the builder; this only fills in non-BPF samplers.
                crate::agent::sampler_status::set_active_if_absent(entry.name);
                live.insert(entry.name);
                samplers.push(s);
            }
            Ok(None) => crate::agent::sampler_status::set_disabled(entry.name),
            Err(e) => crate::agent::sampler_status::set_failed(entry.name, e.to_string()),
        }
    }

    // Every sampler has now had its chance to initialise and bound its own
    // groups, so anything still unbacked never will be. See
    // `bound_groups_without_a_live_sampler`.
    let bounded = crate::agent::samplers::bound_groups_without_a_live_sampler(&live);
    if bounded > 0 {
        debug!("bounded {bounded} acquisition group(s) with no live sampler to zero members");
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
            // Matched ahead of the health arms so it is never rolled into
            // `unsupported` alongside a missing kernel capability: this one is
            // a resource decision the operator can change, and the numbers are
            // what let them decide.
            (SamplerState::PmuLimited { cpus, of }, _) => {
                degraded += 1;
                info!(
                    "sampler {}: running on {cpus} of {of} CPUs — PMU counters reserved \
                     on the rest",
                    s.name
                );
            }
            (SamplerState::PmuStarved { wants, free }, _) => {
                unsupported += 1;
                info!(
                    "sampler {}: not started — PMU budget exhausted (needs {wants}/cpu, \
                     {free} free)",
                    s.name
                );
            }
            (SamplerState::Failed { error }, _) => {
                failed += 1;
                error!("sampler {}: failed — {}", s.name, error);
            }
            (_, Some(SamplerHealth::Failed)) => {
                failed += 1;
                error!("sampler {}: failed", s.name);
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
