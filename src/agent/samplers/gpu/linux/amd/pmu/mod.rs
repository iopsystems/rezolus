//! AMD GPU hardware performance counter (PMU) sampler.
//!
//! Reads **device-wide** GPU hardware counters via rocprofiler-sdk's device
//! counting service (see [`rocprofiler`]). The agent runs no GPU kernels; it
//! samples the whole GPU over a window and sees work from all processes.
//!
//! ## Reading model
//!
//! The device counting context is started **once** at init and runs
//! continuously; the hardware counters then accumulate from that point. Each
//! `rocprofiler_sample_device_counting_service` call returns the running
//! cumulative total (the values reset only on `stop_context`, which we do only
//! at shutdown). So `refresh()` simply reads the current totals on demand — no
//! background thread, no blocking window — and publishes them as monotonic
//! counters that the viewer turns into rates.
//!
//! rocprofiler rejects overlapping reads on a context with `CONTEXT_ERROR`, and
//! `refresh()` runs concurrently with other samplers, so reads are serialized
//! by the `STATE` mutex inside [`rocprofiler`].
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

use std::sync::Arc;

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

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Loading the libraries, registering, and HSA init (which builds the
    // per-agent device-counting contexts and counter configs, and starts each
    // context) all happen here. rocprofiler is a single per-process tool; the
    // state lives in a global it owns (see `rocprofiler.rs`).
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
    debug!("{NAME}: initialized for {} GPU agent(s)", rocp.num_agents());

    Ok(Some(Box::new(AmdPmu { rocp })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry =
    crate::agent::samplers::SamplerEntry { name: NAME, init };

struct AmdPmu {
    rocp: Rocprofiler,
}

#[async_trait]
impl Sampler for AmdPmu {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        // Read the current cumulative counter totals for each GPU on demand.
        // Reads are cheap (no blocking window) and serialized inside the
        // rocprofiler layer.
        for idx in 0..self.rocp.num_agents() {
            match self.rocp.sample(idx) {
                Ok(sums) => publish(idx, &sums),
                Err(e) => debug!("{NAME}: GPU {idx}: read failed: {e}"),
            }
        }
    }
}

/// Publish the cumulative device-level counter totals for GPU `id` into the
/// metrics. Each sampled value is the running total since the context started,
/// but `CounterGroup` only exposes `add`, so we advance the stored counter by
/// the delta from its current value. This keeps it monotonic and converged to
/// the absolute total while tolerating the rare case where a re-read returns a
/// slightly lower value (treated as no change).
fn publish(id: usize, sums: &std::collections::HashMap<String, f64>) {
    let advance = |name: &str, metric: &metriken::CounterGroup| {
        if let Some(&v) = sums.get(name) {
            if v.is_finite() && v >= 0.0 {
                let target = v as u64;
                let current = metric.value(id).unwrap_or(0);
                if target > current {
                    let _ = metric.add(id, target - current);
                }
            }
        }
    };

    advance("GRBM_COUNT", &GPU_GRBM_COUNT);
    advance("SQ_WAVES_sum", &GPU_SQ_WAVES);
    advance("SQ_BUSY_CYCLES", &GPU_SQ_BUSY_CYCLES);
    advance("SQ_INSTS_VALU", &GPU_SQ_INSTS_VALU);
    advance("SQ_INST_CYCLES_VALU", &GPU_SQ_INST_CYCLES_VALU);
    advance("SQC_ICACHE_REQ", &GPU_SQC_ICACHE_REQ);
    advance("SQC_ICACHE_HITS", &GPU_SQC_ICACHE_HITS);
    advance("TCP_REQ", &GPU_TCP_REQ);
    advance("TCP_REQ_MISS", &GPU_TCP_REQ_MISS);
    advance("GL2C_EA_RDREQ_sum", &GPU_GL2C_EA_RDREQ);
    advance("GL2C_EA_WRREQ_sum", &GPU_GL2C_EA_WRREQ);
    advance("GL2C_HIT_sum", &GPU_GL2C_HIT);
    advance("GL2C_MISS_sum", &GPU_GL2C_MISS);
}
