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

const MAX_GPUS: usize = 32;

// ----- GRBM: front-end clocks (GPU busy) -----

#[metric(
    name = "gpmu_clock",
    description = "Wall clock cycles elapsed",
    metadata = { vendor = "amd", counter = "GRBM_COUNT" }
)]
pub static GPU_GRBM_COUNT: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_active_clock",
    description = "Wall clock at least one active wave in GPU",
    metadata = { vendor = "amd", counter = "GRBM_GUI_ACTIVE" }
)]
pub static GPU_GRBM_GUI_ACTIVE: CounterGroup = CounterGroup::new(MAX_GPUS);

// ----- SQ: waves, busy cycles, instruction mix -----

#[metric(
    name = "gpmu_waves",
    description = "Wavefronts dispatched",
    metadata = { vendor = "amd", counter = "SQ_WAVES" }
)]
pub static GPU_SQ_WAVES: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_busy_cycles",
    description = "Total active cycles of all Workgroup processors",
    metadata = { vendor = "amd", counter = "SQ_BUSY_CYCLES" }
)]
pub static GPU_SQ_BUSY_CYCLES: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_wave_cycles",
    description = "The number of residence waves * active cycles of all Workgroup processors"
    metadata = { vendor = "amd", counter = "SQ_WAVE_CYCLES" }
)]
pub static GPU_SQ_WAVE_CYCLES: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_valu_instructions",
    description = "Total Vector-ALU instructions issued by all Workgroup processors",
    metadata = { vendor = "amd", counter = "SQ_INSTS_VALU" }
)]
pub static GPU_SQ_INSTS_VALU: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_salu_instructions",
    description = "Total Scalar-ALU instructions issued by all Workgroup processors",
    metadata = { vendor = "amd", counter = "SQ_INSTS_SALU" }
)]
pub static GPU_SQ_INSTS_SALU: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_lds_instructions",
    description = " Total LDS (local data share) instructions issued by all Workgroup processors",
    metadata = { vendor = "amd", counter = "SQ_INSTS_LDS" }
)]
pub static GPU_SQ_INSTS_LDS: CounterGroup = CounterGroup::new(MAX_GPUS);

// ----- SQC: instruction cache + LDS activity -----

#[metric(
    name = "gpmu_icache_requests",
    description = "Instruction-cache requests",
    metadata = { vendor = "amd", counter = "SQC_ICACHE_REQ" }
)]
pub static GPU_SQC_ICACHE_REQ: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_icache_hits",
    description = "Instruction-cache hits",
    metadata = { vendor = "amd", counter = "SQC_ICACHE_HITS" }
)]
pub static GPU_SQC_ICACHE_HITS: CounterGroup = CounterGroup::new(MAX_GPUS);

// ----- GL2C: L2 cache + L2<->VRAM (memory bandwidth) -----

#[metric(
    name = "gpmu_vram_read_requests",
    description = "Total VRAM read requests from L2 cache",
    metadata = { vendor = "amd", counter = "GL2C_EA_RDREQ" }
)]
pub static GPU_GL2C_EA_RDREQ: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_vram_write_requests",
    description = "Total VRAM write requests from l2 cache",
    metadata = { vendor = "amd", counter = "GL2C_EA_WRREQ" }
)]
pub static GPU_GL2C_EA_WRREQ: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_l2_hits",
    description = "L2 cache hits.",
    metadata = { vendor = "amd", counter = "GL2C_HIT" }
)]
pub static GPU_GL2C_HIT: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpmu_l2_misses",
    description = "L2 cache misses.",
    metadata = { vendor = "amd", counter = "GL2C_MISS" }
)]
pub static GPU_GL2C_MISS: CounterGroup = CounterGroup::new(MAX_GPUS);
