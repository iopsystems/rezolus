use metriken::*;

const MAX_GPUS: usize = 32;

// Memory

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory free.",
    metadata = { vendor = "nvidia", state = "free", unit = "bytes" }
)]
pub static GPU_MEMORY_FREE: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory used.",
    metadata = { vendor = "nvidia", state = "used", unit = "bytes" }
)]
pub static GPU_MEMORY_USED: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

// PCIe

#[metric(
    name = "gpu_pcie_bandwidth",
    description = "The PCIe bandwidth in Bytes/s.",
    metadata = { vendor = "nvidia", unit = "bytes/second" }
)]
pub static GPU_PCIE_BANDWIDTH: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe receive throughput in Bytes/s.",
    metadata = { vendor = "nvidia", direction = "receive", unit = "bytes/second" }
)]
pub static GPU_PCIE_THROUGHPUT_RX: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe transmit throughput in Bytes/s.",
    metadata = { vendor = "nvidia", direction = "transmit", unit = "bytes/second" }
)]
pub static GPU_PCIE_THROUGHPUT_TX: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

// Power and Energy

#[metric(
    name = "gpu_power_usage",
    description = "The current power usage in milliwatts (mW).",
    metadata = { vendor = "nvidia", unit = "milliwatts" }
)]
pub static GPU_POWER_USAGE: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_energy_consumption",
    description = "The energy consumption in milliJoules (mJ).",
    metadata = { vendor = "nvidia", unit = "milliJoules" }
)]
pub static GPU_ENERGY_CONSUMPTION: WindowedCounterGroup = WindowedCounterGroup::new(MAX_GPUS);

// Thermals

#[metric(
    name = "gpu_temperature",
    description = "The current temperature in degrees Celsius (C).",
    metadata = { vendor = "nvidia", unit = "Celsius" }
)]
pub static GPU_TEMPERATURE: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

// Clocks

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "nvidia", clock = "compute", unit = "Hz" }
)]
pub static GPU_CLOCK_COMPUTE: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "nvidia", clock = "graphics", unit = "Hz" }
)]
pub static GPU_CLOCK_GRAPHICS: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "nvidia", clock = "memory", unit = "Hz" }
)]
pub static GPU_CLOCK_MEMORY: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "nvidia", clock = "video", unit = "Hz" }
)]
pub static GPU_CLOCK_VIDEO: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

// Utilization

#[metric(
    name = "gpu_utilization",
    description = "The running average percentage of time the GPU was executing one or more kernels. (0-100).",
    metadata = { vendor = "nvidia", unit = "percentage" }
)]
pub static GPU_UTILIZATION: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory_utilization",
    description = "The running average percentage of time that GPU memory was being read from or written to. (0-100).",
    metadata = { vendor = "nvidia", unit = "percentage" }
)]
pub static GPU_MEMORY_UTILIZATION: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

// GPU Performance Monitoring - requires Hopper+ and GPM support

#[metric(
    name = "gpu_sm_utilization",
    description = "The percentage of time each SM had at least 1 warp assigned, averaged over all SMs. (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", unit = "percentage" }
)]
pub static GPU_SM_UTILIZATION: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_sm_occupancy",
    description = "The percentage of warps that were active vs theoretical maximum, averaged over all SMs. (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", unit = "percentage" }
)]
pub static GPU_SM_OCCUPANCY: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_dram_bandwidth_utilization",
    description = "The percentage of DRAM (HBM) bandwidth used. (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", unit = "percentage" }
)]
pub static GPU_DRAM_BW_UTILIZATION: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_tensor_utilization",
    description = "The percentage of time the GPU's SMs were doing any tensor operations. (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", pipe = "any", unit = "percentage" }
)]
pub static GPU_TENSOR_UTILIZATION: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_tensor_utilization",
    description = "The percentage of time the GPU's SMs were doing HMMA tensor operations (FP16/BF16, and FP32 matmul which runs as TF32). (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", pipe = "hmma", unit = "percentage" }
)]
pub static GPU_TENSOR_UTILIZATION_HMMA: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_tensor_utilization",
    description = "The percentage of time the GPU's SMs were doing IMMA tensor operations (integer, e.g. INT8). (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", pipe = "imma", unit = "percentage" }
)]
pub static GPU_TENSOR_UTILIZATION_IMMA: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_tensor_utilization",
    description = "The percentage of time the GPU's SMs were doing DFMA tensor operations (FP64). (0-100). Requires Hopper+ GPU.",
    metadata = { vendor = "nvidia", pipe = "dfma", unit = "percentage" }
)]
pub static GPU_TENSOR_UTILIZATION_DFMA: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);
