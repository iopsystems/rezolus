use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

const MAX_GPUS: usize = 32;

// `gpu_nvidia`'s `stats.rs` is Linux-only (no cross-platform `include!`
// fallback — `gpu/macos/stats.rs` is a genuinely separate metric set), so
// `super::NAME` is used directly (see the identical note on
// `gpu_amd_smi`'s `GPU_AMD_SMI_ACQ`).
//
/// ONE group for the whole per-device sweep in `NvidiaInner::refresh_nvml`
/// (all NVML/GPM reads for every GPU, one contiguous synchronous loop, same
/// "device sweep" read-section shape and grounding as `gpu_amd_smi`'s
/// `GPU_AMD_SMI_ACQ` — see its doc comment). Single writer:
/// `NvidiaInner::refresh_nvml`, called only from this sampler's own
/// `Sampler::refresh`.
pub static GPU_NVIDIA_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(super::NAME, "gpu_nvidia_devices");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static GPU_NVIDIA_ACQ_REG: &'static AcquisitionGroup = &GPU_NVIDIA_ACQ;

// Memory

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory free.",
    metadata = { vendor = "nvidia", state = "free", unit = "bytes", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_MEMORY_FREE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory used.",
    metadata = { vendor = "nvidia", state = "used", unit = "bytes", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_MEMORY_USED: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// PCIe

#[metric(
    name = "gpu_pcie_bandwidth",
    description = "The PCIe bandwidth in Bytes/s.",
    metadata = { vendor = "nvidia", unit = "bytes/second", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_PCIE_BANDWIDTH: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe receive throughput in Bytes/s.",
    metadata = { vendor = "nvidia", direction = "receive", unit = "bytes/second", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_PCIE_THROUGHPUT_RX: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe transmit throughput in Bytes/s.",
    metadata = { vendor = "nvidia", direction = "transmit", unit = "bytes/second", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_PCIE_THROUGHPUT_TX: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Power and Energy

#[metric(
    name = "gpu_power_usage",
    description = "The current power usage in milliwatts (mW).",
    metadata = { vendor = "nvidia", unit = "milliwatts", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_POWER_USAGE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_energy_consumption",
    description = "The energy consumption in milliJoules (mJ).",
    metadata = { vendor = "nvidia", unit = "milliJoules", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_ENERGY_CONSUMPTION: CounterGroup = CounterGroup::new(MAX_GPUS);

// Thermals

#[metric(
    name = "gpu_temperature",
    description = "The current temperature in degrees Celsius (C).",
    metadata = { vendor = "nvidia", unit = "Celsius", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_TEMPERATURE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Clocks

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "nvidia", clock = "compute", unit = "Hz", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_CLOCK_COMPUTE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "nvidia", clock = "graphics", unit = "Hz", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_CLOCK_GRAPHICS: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "nvidia", clock = "memory", unit = "Hz", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_CLOCK_MEMORY: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "nvidia", clock = "video", unit = "Hz", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_CLOCK_VIDEO: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Utilization

#[metric(
    name = "gpu_utilization",
    description = "The running average percentage of time the GPU was executing one or more kernels. (0-100).",
    metadata = { vendor = "nvidia", unit = "percentage", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory_utilization",
    description = "The running average percentage of time that GPU memory was being read from or written to. (0-100).",
    metadata = { vendor = "nvidia", unit = "percentage", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_MEMORY_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// GPU Performance Monitoring - requires Hopper+ and GPM support

#[metric(
    name = "gpu_sm_utilization",
    description = "The percentage of time each SM had at least 1 warp assigned, averaged over all SMs. (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", unit = "percentage", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_SM_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_sm_occupancy",
    description = "The percentage of warps that were active vs theoretical maximum, averaged over all SMs. (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", unit = "percentage", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_SM_OCCUPANCY: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_dram_bandwidth_utilization",
    description = "The percentage of DRAM (HBM) bandwidth used. (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", unit = "percentage", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_DRAM_BW_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_tensor_utilization",
    description = "The percentage of time the GPU's SMs were doing any tensor operations. (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", pipe = "any", unit = "percentage", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_TENSOR_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_tensor_utilization",
    description = "The percentage of time the GPU's SMs were doing HMMA tensor operations (FP16/BF16, and FP32 matmul which runs as TF32). (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", pipe = "hmma", unit = "percentage", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_TENSOR_UTILIZATION_HMMA: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_tensor_utilization",
    description = "The percentage of time the GPU's SMs were doing IMMA tensor operations (integer, e.g. INT8). (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", pipe = "imma", unit = "percentage", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_TENSOR_UTILIZATION_IMMA: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_tensor_utilization",
    description = "The percentage of time the GPU's SMs were doing DFMA tensor operations (FP64). (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", pipe = "dfma", unit = "percentage", acq_group = "gpu_nvidia_devices" }
)]
pub static GPU_TENSOR_UTILIZATION_DFMA: GaugeGroup = GaugeGroup::new(MAX_GPUS);
