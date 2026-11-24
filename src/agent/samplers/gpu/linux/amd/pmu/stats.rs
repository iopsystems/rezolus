//! Metrics for the AMD GPU hardware performance counter (PMU) sampler.
//!
//! The hardware counters are **monotonic raw counters** read device-wide via
//! rocprofiler-sdk and summed across hardware instances. They are exposed as
//! counters so the viewer can compute rates; derived quantities (utilization,
//! cache hit rate, memory bandwidth in bytes) are computed downstream from
//! these, matching how Rezolus treats other throughput metrics.
//!
//! Counter names follow the RDNA (gfx12xx) naming. CDNA/MI300 uses different
//! block names (e.g. `TCC_*` for L2); see `docs/amd_gpu_pmu_events.md`.

use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

use super::super::MAX_GPUS;

/// Every counter below, one member per GPU agent. Single writer: `refresh` in
/// `mod.rs`, which reads every GPU's running totals under one acquisition.
/// One group for all fourteen because one `sample` call per GPU returns them
/// together: principle 18's device-visit sweep.
///
/// Declared so that a counter at zero is published as zero; the default group
/// skips a zero-valued `CounterGroup` entry.
pub static GPU_AMD_PMU_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(super::NAME, "gpu_amd_pmu_devices");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static GPU_AMD_PMU_ACQ_REG: &'static AcquisitionGroup = &GPU_AMD_PMU_ACQ;

// ----- GRBM: front-end clocks (GPU busy) -----

#[metric(
    name = "gpmu_clock",
    description = "Wall clock cycles elapsed",
    metadata = { vendor = "amd", counter = "GRBM_COUNT", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_GRBM_COUNT: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_active_clock",
    description = "Wall clock at least one active wave in GPU",
    metadata = { vendor = "amd", counter = "GRBM_GUI_ACTIVE", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_GRBM_GUI_ACTIVE: CounterGroup = CounterGroup::new(MAX_GPUS);

// ----- SQ: waves, busy cycles, instruction mix -----

#[metric(
    name = "gpmu_waves",
    description = "Wavefronts dispatched",
    metadata = { vendor = "amd", counter = "SQ_WAVES", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_SQ_WAVES: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_busy_cycles",
    description = "Total active cycles of all Workgroup processors",
    metadata = { vendor = "amd", counter = "SQ_BUSY_CYCLES", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_SQ_BUSY_CYCLES: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_wave_cycles",
    description = "The number of residence waves * active cycles of all Workgroup processors",
    metadata = { vendor = "amd", counter = "SQ_WAVE_CYCLES", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_SQ_WAVE_CYCLES: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_valu_instructions",
    description = "Total Vector-ALU instructions issued by all Workgroup processors",
    metadata = { vendor = "amd", counter = "SQ_INSTS_VALU", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_SQ_INSTS_VALU: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_salu_instructions",
    description = "Total Scalar-ALU instructions issued by all Workgroup processors",
    metadata = { vendor = "amd", counter = "SQ_INSTS_SALU", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_SQ_INSTS_SALU: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_lds_instructions",
    description = "Total LDS (local data share) instructions issued by all Workgroup processors",
    metadata = { vendor = "amd", counter = "SQ_INSTS_LDS", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_SQ_INSTS_LDS: CounterGroup = CounterGroup::new(MAX_GPUS);

// ----- SQC: instruction cache -----

#[metric(
    name = "gpmu_icache_requests",
    description = "Instruction-cache requests",
    metadata = { vendor = "amd", counter = "SQC_ICACHE_REQ", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_SQC_ICACHE_REQ: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_icache_hits",
    description = "Instruction-cache hits",
    metadata = { vendor = "amd", counter = "SQC_ICACHE_HITS", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_SQC_ICACHE_HITS: CounterGroup = CounterGroup::new(MAX_GPUS);

// ----- GL2C: L2 cache + L2<->VRAM (memory bandwidth) -----

#[metric(
    name = "gpmu_vram_read_requests",
    description = "Total VRAM read requests from L2 cache",
    metadata = { vendor = "amd", counter = "GL2C_EA_RDREQ", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_GL2C_EA_RDREQ: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_vram_write_requests",
    description = "Total VRAM write requests from l2 cache",
    metadata = { vendor = "amd", counter = "GL2C_EA_WRREQ", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_GL2C_EA_WRREQ: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_l2_hits",
    description = "L2 cache hits.",
    metadata = { vendor = "amd", counter = "GL2C_HIT", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_GL2C_HIT: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_l2_misses",
    description = "L2 cache misses.",
    metadata = { vendor = "amd", counter = "GL2C_MISS", acq_group = "gpu_amd_pmu_devices" }
)]
pub static GPU_GL2C_MISS: CounterGroup = CounterGroup::new(MAX_GPUS);
