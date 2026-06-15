//! Metrics for the AMD GPU hardware performance counter (PMU) sampler.
//!
//! All of these are **monotonic raw hardware counters** read device-wide via
//! rocprofiler-sdk and summed across hardware instances. They are exposed as
//! counters so the viewer can compute rates; derived quantities (utilization,
//! cache hit rate, memory bandwidth in bytes) are computed downstream from
//! these, matching how Rezolus treats other throughput metrics.
//!
//! Counter names follow the RDNA (gfx12xx) naming. CDNA/MI300 uses different
//! block names (e.g. `TCC_*` for L2); see `docs/amd_gpu_counters.md`.

use metriken::*;

const MAX_GPUS: usize = 32;

// ----- GRBM: front-end clocks (GPU busy denominator) -----

#[metric(
    name = "gpu_grbm_clocks",
    description = "Total GRBM clock cycles elapsed. Denominator for GPU busy percentage.",
    metadata = { vendor = "amd", counter = "GRBM_COUNT" }
)]
pub static GPU_GRBM_COUNT: CounterGroup = CounterGroup::new(MAX_GPUS);

// ----- SQ: waves, busy cycles, vector-ALU work -----

#[metric(
    name = "gpu_waves",
    description = "Wavefronts dispatched to the sequencers (SQ_WAVES_sum).",
    metadata = { vendor = "amd", counter = "SQ_WAVES_sum" }
)]
pub static GPU_SQ_WAVES: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_sq_busy_cycles",
    description = "Clock cycles with at least one active wave in a shader engine (SQ_BUSY_CYCLES).",
    metadata = { vendor = "amd", counter = "SQ_BUSY_CYCLES" }
)]
pub static GPU_SQ_BUSY_CYCLES: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_valu_instructions",
    description = "Vector-ALU instructions issued (SQ_INSTS_VALU).",
    metadata = { vendor = "amd", counter = "SQ_INSTS_VALU" }
)]
pub static GPU_SQ_INSTS_VALU: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_valu_cycles",
    description = "Cycles needed to execute vector-ALU operations (SQ_INST_CYCLES_VALU). Ratio to instructions indicates VALU contention.",
    metadata = { vendor = "amd", counter = "SQ_INST_CYCLES_VALU" }
)]
pub static GPU_SQ_INST_CYCLES_VALU: CounterGroup = CounterGroup::new(MAX_GPUS);

// ----- SQC: instruction cache -----

#[metric(
    name = "gpu_icache_requests",
    description = "Instruction-cache requests (SQC_ICACHE_REQ).",
    metadata = { vendor = "amd", counter = "SQC_ICACHE_REQ" }
)]
pub static GPU_SQC_ICACHE_REQ: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_icache_hits",
    description = "Instruction-cache hits (SQC_ICACHE_HITS).",
    metadata = { vendor = "amd", counter = "SQC_ICACHE_HITS" }
)]
pub static GPU_SQC_ICACHE_HITS: CounterGroup = CounterGroup::new(MAX_GPUS);

// ----- TCP: L1 vector cache -----

#[metric(
    name = "gpu_l1_requests",
    description = "L1 vector cache (TCP) cache-line accesses (TCP_REQ).",
    metadata = { vendor = "amd", counter = "TCP_REQ" }
)]
pub static GPU_TCP_REQ: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_l1_misses",
    description = "L1 vector cache (TCP) requests that missed (TCP_REQ_MISS).",
    metadata = { vendor = "amd", counter = "TCP_REQ_MISS" }
)]
pub static GPU_TCP_REQ_MISS: CounterGroup = CounterGroup::new(MAX_GPUS);

// ----- GL2C: L2 cache + L2<->VRAM (memory bandwidth) -----

#[metric(
    name = "gpu_l2_memory_read_requests",
    description = "L2<->memory read requests (GL2C_EA_RDREQ_sum). Read memory bandwidth source.",
    metadata = { vendor = "amd", counter = "GL2C_EA_RDREQ_sum" }
)]
pub static GPU_GL2C_EA_RDREQ: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_l2_memory_write_requests",
    description = "L2<->memory write requests (GL2C_EA_WRREQ_sum). Write memory bandwidth source.",
    metadata = { vendor = "amd", counter = "GL2C_EA_WRREQ_sum" }
)]
pub static GPU_GL2C_EA_WRREQ: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_l2_hits",
    description = "L2 cache hits (GL2C_HIT_sum).",
    metadata = { vendor = "amd", counter = "GL2C_HIT_sum" }
)]
pub static GPU_GL2C_HIT: CounterGroup = CounterGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_l2_misses",
    description = "L2 cache misses (GL2C_MISS_sum).",
    metadata = { vendor = "amd", counter = "GL2C_MISS_sum" }
)]
pub static GPU_GL2C_MISS: CounterGroup = CounterGroup::new(MAX_GPUS);
