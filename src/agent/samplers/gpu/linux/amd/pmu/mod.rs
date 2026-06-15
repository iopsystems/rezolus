//! AMD GPU hardware performance counter (PMU) sampler.
//!
//! Reads **device-wide** GPU hardware counters via rocprofiler-sdk's device
//! counting service (see [`rocprofiler`]). The agent runs no GPU kernels; it
//! samples the whole GPU over a window and sees work from all processes.
//!
//! ## Threading model
//!
//! `rocprofiler_sample_device_counting_service` requires bracketing a
//! `start_context` / sleep / `stop_context` window, so a sample inherently
//! blocks for the window duration. To keep the async `refresh()` cheap (it runs
//! on every scrape, concurrently with all other samplers), the actual sampling
//! runs in a dedicated **background OS thread** that loops continuously and
//! writes the latest device-level counter values into the metrics. `refresh()`
//! is therefore a no-op — the metrics are always current, mirroring how the BPF
//! samplers expose continuously-aggregated values read on demand.
//!
//! ## Power state
//!
//! On RDNA GPUs many counters only read non-zero when the GPU is in a stable
//! power state (`amd-smi set -g N -l stable_std`), which pins clocks (roughly
//! halving the core clock) and perturbs real workloads. We therefore do **not**
//! change the power state by default; counters that need it will read zero
//! unless an operator sets it. See `docs/amd_gpu_counters.md`.
//!
//! Requires the `CAP_PERFMON` capability.

const NAME: &str = "gpu_amd_pmu";

use crate::agent::*;

mod rocprofiler;
mod stats;

use rocprofiler::Rocprofiler;
use stats::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The single-pass counter set we collect. These fit the RDNA per-block slot
/// budget (SQ ≤ 8, GL2C ≤ 4, etc.). Each maps to a metric in `pmu_stats`.
const COUNTERS: &[&str] = &[
    "GRBM_COUNT",
    "SQ_WAVES_sum",
    "SQ_BUSY_CYCLES",
    "SQ_INSTS_VALU",
    "SQ_INST_CYCLES_VALU",
    "SQC_ICACHE_REQ",
    "SQC_ICACHE_HITS",
    "TCP_REQ",
    "TCP_REQ_MISS",
    "GL2C_EA_RDREQ_sum",
    "GL2C_EA_WRREQ_sum",
    "GL2C_HIT_sum",
    "GL2C_MISS_sum",
];

/// Default per-sample window when `sampling_window` is not set in the config.
/// The background thread spends roughly this long in each `start`/`stop`
/// bracket; shorter windows give finer time resolution (lower metric staleness)
/// at higher overhead. Override with `[samplers.gpu_amd_pmu] sampling_window`.
const DEFAULT_SAMPLE_WINDOW: Duration = Duration::from_millis(1000);

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Loading the libraries, registering, and HSA init (which builds the
    // per-agent device-counting contexts and counter configs) all happen here.
    // rocprofiler is a single per-process tool; the state lives in a global it
    // owns (see `rocprofiler.rs`).
    let rocp = match Rocprofiler::new(COUNTERS) {
        Ok(Some(r)) => r,
        Ok(None) => {
            debug!("{NAME}: rocprofiler-sdk / HSA runtime not present");
            return Ok(None);
        }
        Err(e) => {
            debug!("{NAME}: disabled: {e}");
            return Ok(None);
        }
    };
    let window = config.sampling_window(NAME, DEFAULT_SAMPLE_WINDOW);
    debug!(
        "{NAME}: initialized for {} GPU agent(s), {}ms sampling window",
        rocp.num_agents(),
        window.as_millis()
    );

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();

    // Sampling runs on a dedicated thread because each sample blocks for the
    // window. rocprofiler's device-counting callback fires on whichever thread
    // calls `start_context`, so all sampling happens on this one thread.
    let handle = std::thread::Builder::new()
        .name("gpu_amd_pmu".into())
        .spawn(move || sampling_loop(rocp, window, thread_stop))
        .map_err(|e| anyhow::anyhow!("{NAME}: failed to spawn sampling thread: {e}"))?;

    Ok(Some(Box::new(AmdPmu {
        stop,
        handle: Some(handle),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry =
    crate::agent::samplers::SamplerEntry { name: NAME, init };

struct AmdPmu {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[async_trait]
impl Sampler for AmdPmu {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        // No-op: the background thread keeps the metrics current.
    }
}

impl Drop for AmdPmu {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Continuously sample every agent and publish the latest device-level counter
/// values into the metrics. Runs until `stop` is set.
fn sampling_loop(rocp: Rocprofiler, window: Duration, stop: Arc<AtomicBool>) {
    let n = rocp.num_agents();
    debug!("{NAME}: sampling loop started for {n} agent(s)");
    let mut first = true;
    while !stop.load(Ordering::Relaxed) {
        for idx in 0..n {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            match rocp.sample(idx, window) {
                Ok(sums) => {
                    if first {
                        debug!("{NAME}: GPU {idx} first sample: {sums:?}");
                        first = false;
                    }
                    publish(idx, &sums);
                }
                Err(e) => debug!("{NAME}: GPU {idx}: sample failed: {e}"),
            }
        }
    }
}

/// Map a counter name -> device-level sum to its metric, accumulating into the
/// monotonic counter. The raw counters report deltas over the sample window, so
/// we add each window's sum to the running counter.
fn publish(id: usize, sums: &std::collections::HashMap<String, f64>) {
    let add = |name: &str, metric: &metriken::CounterGroup| {
        if let Some(&v) = sums.get(name) {
            if v.is_finite() && v >= 0.0 {
                let _ = metric.add(id, v as u64);
            }
        }
    };

    add("GRBM_COUNT", &GPU_GRBM_COUNT);
    add("SQ_WAVES_sum", &GPU_SQ_WAVES);
    add("SQ_BUSY_CYCLES", &GPU_SQ_BUSY_CYCLES);
    add("SQ_INSTS_VALU", &GPU_SQ_INSTS_VALU);
    add("SQ_INST_CYCLES_VALU", &GPU_SQ_INST_CYCLES_VALU);
    add("SQC_ICACHE_REQ", &GPU_SQC_ICACHE_REQ);
    add("SQC_ICACHE_HITS", &GPU_SQC_ICACHE_HITS);
    add("TCP_REQ", &GPU_TCP_REQ);
    add("TCP_REQ_MISS", &GPU_TCP_REQ_MISS);
    add("GL2C_EA_RDREQ_sum", &GPU_GL2C_EA_RDREQ);
    add("GL2C_EA_WRREQ_sum", &GPU_GL2C_EA_WRREQ);
    add("GL2C_HIT_sum", &GPU_GL2C_HIT);
    add("GL2C_MISS_sum", &GPU_GL2C_MISS);
}
