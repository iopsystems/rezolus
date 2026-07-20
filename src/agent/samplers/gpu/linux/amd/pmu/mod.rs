//! AMD GPU hardware performance counter (PMU) sampler.
//!
//! Reads **device-wide** GPU hardware counters via rocprofiler-sdk's device
//! counting service (see [`rocprofiler`]). The agent runs no GPU kernels; it
//! samples the whole GPU over a window and sees work from all processes.
//!
//! ## Reading model
//!
//! The RDNA per-WGP hardware counters are **32-bit and saturate** (clamp, not
//! wrap) within ~34-275ms of busy time, so they cannot be read as a running
//! cumulative total — a continuously-running context would pin them at their
//! ceiling. Instead, a per-GPU background worker thread brackets each short
//! window with `start_context` (which resets the counters to 0) ... sleep ...
//! `sample` ... `stop_context`, and accumulates each window's delta into a
//! running total. This keeps every per-window read well under the 2^32 ceiling
//! while still presenting monotonic counters that the viewer turns into rates.
//!
//! Bracketing every window with start/stop is only cheap (~150us instead of
//! ~18ms) because we set `ROCPROFILER_DEVICE_LOCK_AT_START=1`, which acquires
//! the KFD device-profiling lock once at config time rather than per start.
//!
//! rocprofiler is a single per-process tool, so the worker threads' start/
//! sample/stop calls are serialized by the `STATE` mutex inside [`rocprofiler`];
//! the per-window sleep is done without the lock. `refresh()` just reads the
//! accumulated totals (no GPU I/O).
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
//!
//! ## GPU `id` labels
//!
//! The `id` label is the **rocprofiler GPU-agent enumeration order**, while the
//! `gpu_amd_smi` sampler's `id` follows `rsmi_num_monitor_devices` order. In
//! practice both enumerate GPUs by ascending PCI bus address, so the two `id`s
//! line up on typical hosts (verified on a 2-GPU gfx1201+gfx1036 box: both map
//! `id=0 -> 0000:03:00.0`, `id=1 -> 0000:36:00.0`). The orders are *not
//! contractually guaranteed* to match, though, so for hard correlation across
//! the two samplers — especially on unusual topologies or partitioned GPUs —
//! prefer physical identity (e.g. PCI BDF) over `id`.

const NAME: &str = "gpu_amd_pmu";

use crate::agent::*;

mod perf_level;
mod rocprofiler;
mod stats;

use perf_level::PerfLevel;
use rocprofiler::Rocprofiler;
use stats::*;

use std::sync::Arc;

/// The single-pass counter set we collect. These fit the RDNA per-block slot
/// budget (SQ ≤ 8, GL2C ≤ 4, etc.). Each maps to a metric in `pmu_stats`.
const COUNTERS: &[&str] = &[
    // GRBM (2): GPU busy
    "GRBM_COUNT",
    "GRBM_GUI_ACTIVE",
    // SQ (6): waves, busy/wave cycles, VALU/SALU/LDS instruction mix.
    // SQ_WAVE_CYCLES is a per-WGP 32-bit accumulator that saturates within
    // ~34-275ms of busy time; the worker thread brackets each window with
    // start/stop (which resets the counters) so it stays unsaturated. See
    // docs/amd_gpu_pmu_events.md and `rocprofiler.rs`.
    "SQ_WAVES",
    "SQ_BUSY_CYCLES",
    "SQ_WAVE_CYCLES",
    "SQ_INSTS_VALU",
    "SQ_INSTS_SALU",
    "SQ_INSTS_LDS",
    // SQC (2): instruction cache. SQ and SQC share an 8-counter register pool on
    // RDNA4 (validated on gfx1201); SQ(6) + SQC(2) = 8 is at the ceiling.
    "SQC_ICACHE_REQ",
    "SQC_ICACHE_HITS",
    // GL2C (4): L2 cache + memory bandwidth
    "GL2C_EA_RDREQ",
    "GL2C_EA_WRREQ",
    "GL2C_HIT",
    "GL2C_MISS",
];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Optionally pin the GPU performance level before rocprofiler init, so the
    // counting contexts are armed while the GPU is already in the requested
    // state. On RDNA the per-SIMD counters only accumulate in a stable power
    // state; see `perf_level`. Default (None) leaves the power state untouched.
    if let Some(level_str) = config.gpu_perf_level(NAME) {
        let level = level_str
            .parse::<PerfLevel>()
            .map_err(|e| anyhow::anyhow!("{NAME}: {e}"))?;
        perf_level::apply_all(level);
    }

    // Loading the libraries, registering, HSA init (which builds the per-agent
    // device-counting contexts and counter configs), and spawning the per-GPU
    // worker threads all happen here. rocprofiler is a single per-process tool;
    // the state lives in a global it owns (see `rocprofiler.rs`).
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
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct AmdPmu {
    rocp: Rocprofiler,
}

#[async_trait]
impl Sampler for AmdPmu {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        // Read the running counter totals for each GPU. The totals are maintained
        // by per-GPU background worker threads (which bracket each window with
        // start/stop); this is just a cheap in-memory read, no GPU I/O.
        for idx in 0..self.rocp.num_agents() {
            match self.rocp.sample(idx) {
                Ok(sums) => publish(idx, &sums),
                Err(e) => debug!("{NAME}: GPU {idx}: read failed: {e}"),
            }
        }
    }
}

/// Publish the running device-level counter totals for GPU `id` into the
/// metrics. Each value is a monotonic running total (the sum of per-window
/// deltas accumulated by the worker thread), but `CounterGroup` only exposes
/// `add`, so we advance the stored counter by the delta from its current value.
/// This keeps it monotonic and converged to the total.
fn publish(id: usize, sums: &std::collections::HashMap<String, f64>) {
    // Metric groups are sized for `MAX_GPUS`; a higher id would be silently
    // dropped by `CounterGroup::add` (returns Err). Skip it and warn once.
    if id >= super::MAX_GPUS {
        use std::sync::atomic::{AtomicBool, Ordering};
        static WARNED: AtomicBool = AtomicBool::new(false);
        if !WARNED.swap(true, Ordering::Relaxed) {
            warn!(
                "{NAME}: GPU {id} exceeds MAX_GPUS ({}); its counters are not recorded",
                super::MAX_GPUS
            );
        }
        return;
    }

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
    advance("GRBM_GUI_ACTIVE", &GPU_GRBM_GUI_ACTIVE);
    advance("SQ_WAVES", &GPU_SQ_WAVES);
    advance("SQ_BUSY_CYCLES", &GPU_SQ_BUSY_CYCLES);
    advance("SQ_WAVE_CYCLES", &GPU_SQ_WAVE_CYCLES);
    advance("SQ_INSTS_VALU", &GPU_SQ_INSTS_VALU);
    advance("SQ_INSTS_SALU", &GPU_SQ_INSTS_SALU);
    advance("SQ_INSTS_LDS", &GPU_SQ_INSTS_LDS);
    advance("SQC_ICACHE_REQ", &GPU_SQC_ICACHE_REQ);
    advance("SQC_ICACHE_HITS", &GPU_SQC_ICACHE_HITS);
    advance("GL2C_EA_RDREQ", &GPU_GL2C_EA_RDREQ);
    advance("GL2C_EA_WRREQ", &GPU_GL2C_EA_WRREQ);
    advance("GL2C_HIT", &GPU_GL2C_HIT);
    advance("GL2C_MISS", &GPU_GL2C_MISS);
}
