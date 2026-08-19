use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

use super::MAX_GPUS;

// `gpu_amd_smi`'s `stats.rs` is Linux-only (no cross-platform `include!`
// fallback for GPU metric identity — `gpu/macos/stats.rs` is a genuinely
// separate metric set), so this group can use `super::NAME` directly rather
// than `crate::agent::samplers::bpf_sampler_name` (contrast
// `memory_meminfo`'s `MEMINFO_ACQ`, whose `stats.rs` IS `include!`d
// cross-platform).
//
/// ONE group for the whole per-device sweep in `AmdInner::refresh` (all
/// `rocm_smi` reads for every GPU, one contiguous synchronous loop — not one
/// group per metric family). Grounding: `gpu_amd_pmu` (rocprofiler hardware
/// counters) is already a SEPARATE `SAMPLERS` entry reading a genuinely
/// different source (see `pmu/mod.rs`) — that is the "separate section"
/// principle 18 calls out, and it does not touch this sampler at all.
/// Within THIS sampler, the per-device loop reads many distinct `rocm_smi`
/// library calls (memory, utilization, temperature, power/energy, clocks,
/// pcie) back-to-back with no separate task/phase boundary between them —
/// principle 18 lists "a device sweep" as its own read-section archetype,
/// alongside "a BPF map sweep" and "a batch of like-entity map reads", and
/// this collapses the whole sweep the same way `drivehealth`'s flagship
/// migration does (there too, several distinct metric families share one
/// group because they come from one device's one read pass). Single writer:
/// `AmdInner::refresh`, called only from this sampler's own `Sampler::refresh`
/// (no other task ever touches AMD SMI metrics).
pub static GPU_AMD_SMI_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(super::NAME, "gpu_amd_smi_devices");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static GPU_AMD_SMI_ACQ_REG: &'static AcquisitionGroup = &GPU_AMD_SMI_ACQ;

// Memory

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory free.",
    metadata = { vendor = "amd", state = "free", unit = "bytes", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_MEMORY_FREE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory used.",
    metadata = { vendor = "amd", state = "used", unit = "bytes", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_MEMORY_USED: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// PCIe

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe receive throughput in Bytes/s.",
    metadata = { vendor = "amd", direction = "receive", unit = "bytes/second", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_PCIE_THROUGHPUT_RX: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe transmit throughput in Bytes/s.",
    metadata = { vendor = "amd", direction = "transmit", unit = "bytes/second", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_PCIE_THROUGHPUT_TX: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Power and Energy

#[metric(
    name = "gpu_power_usage",
    description = "The current power usage in milliwatts (mW).",
    metadata = { vendor = "amd", unit = "milliwatts", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_POWER_USAGE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_energy_consumption",
    description = "The energy consumption in milliJoules (mJ).",
    metadata = { vendor = "amd", unit = "milliJoules", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_ENERGY_CONSUMPTION: CounterGroup = CounterGroup::new(MAX_GPUS);

// Thermals

#[metric(
    name = "gpu_temperature",
    description = "The current edge temperature in degrees Celsius (C).",
    metadata = { vendor = "amd", sensor = "edge", unit = "Celsius", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_TEMPERATURE_EDGE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_temperature",
    description = "The current junction (hotspot) temperature in degrees Celsius (C).",
    metadata = { vendor = "amd", sensor = "junction", unit = "Celsius", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_TEMPERATURE_JUNCTION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_temperature",
    description = "The current memory (VRAM) temperature in degrees Celsius (C).",
    metadata = { vendor = "amd", sensor = "memory", unit = "Celsius", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_TEMPERATURE_MEMORY: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Clocks

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "amd", clock = "compute", unit = "Hz", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_CLOCK_COMPUTE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "amd", clock = "graphics", unit = "Hz", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_CLOCK_GRAPHICS: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "amd", clock = "memory", unit = "Hz", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_CLOCK_MEMORY: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Utilization

#[metric(
    name = "gpu_utilization",
    description = "The percentage of time the GPU was busy executing work. (0-100).",
    metadata = { vendor = "amd", unit = "percentage", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory_utilization",
    description = "The percentage of time the GPU memory controller was busy. (0-100).",
    metadata = { vendor = "amd", unit = "percentage", acq_group = "gpu_amd_smi_devices" }
)]
pub static GPU_MEMORY_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);
