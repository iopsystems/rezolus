use metriken::*;

use crate::agent::*;

const MAX_GPUS: usize = 32;

// Memory

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory free.",
    metadata = { state = "free", unit = "bytes" }
)]
pub static GPU_MEMORY_FREE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory used.",
    metadata = { state = "used", unit = "bytes" }
)]
pub static GPU_MEMORY_USED: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// PCIe

#[metric(
    name = "gpu_pcie_bandwidth",
    description = "The PCIe bandwidth in Bytes/s.",
    metadata = { direction = "receive", unit = "bytes/second" }
)]
pub static GPU_PCIE_BANDWIDTH: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe receive throughput in Bytes/s.",
    metadata = { direction = "receive", unit = "bytes/second" }
)]
pub static GPU_PCIE_THROUGHPUT_RX: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe transmit throughput in Bytes/s.",
    metadata = { direction = "transmit", unit = "bytes/second" }
)]
pub static GPU_PCIE_THROUGHPUT_TX: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Power and Energy

#[metric(
    name = "gpu_power_usage",
    description = "The current power usage in milliwatts (mW).",
    metadata = { unit = "milliwatts" }
)]
pub static GPU_POWER_USAGE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_energy_consumption",
    description = "The energy consumption in milliJoules (mJ).",
    metadata = { unit = "milliJoules" }
)]
pub static GPU_ENERGY_CONSUMPTION: CounterGroup = CounterGroup::new(MAX_GPUS);

// Thermals

#[metric(
    name = "gpu_temperature",
    description = "The current temperature in degrees Celsius (C).",
    metadata = { unit = "Celsius" }
)]
pub static GPU_TEMPERATURE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Clocks

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { clock = "compute", unit = "Hz" }
)]
pub static GPU_CLOCK_COMPUTE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { clock = "graphics", unit = "Hz" }
)]
pub static GPU_CLOCK_GRAPHICS: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { clock = "memory", unit = "Hz" }
)]
pub static GPU_CLOCK_MEMORY: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { clock = "video", unit = "Hz" }
)]
pub static GPU_CLOCK_VIDEO: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Utilization

#[metric(
    name = "gpu_utilization",
    description = "The running average percentage of time the GPU was executing one or more kernels. (0-100).",
    metadata = { unit = "percentage" }
)]
pub static GPU_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory_utilization",
    description = "The running average percentage of time that GPU memory was being read from or written to. (0-100).",
    metadata = { unit = "percentage" }
)]
pub static GPU_MEMORY_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// CUPTI PM Sampling Metrics (Turing+ GPUs)

#[metric(
    name = "gpu_sm_utilization",
    description = "SM throughput as percentage of peak (0-100).",
    metadata = { unit = "percentage" }
)]
pub static GPU_SM_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_dram_throughput",
    description = "DRAM throughput as percentage of peak (0-100).",
    metadata = { unit = "percentage" }
)]
pub static GPU_DRAM_THROUGHPUT: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_l2_hit_rate",
    description = "L2 cache hit rate (0-100).",
    metadata = { unit = "percentage" }
)]
pub static GPU_L2_HIT_RATE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_achieved_occupancy",
    description = "Ratio of active warps to max warps (0-100).",
    metadata = { unit = "percentage" }
)]
pub static GPU_ACHIEVED_OCCUPANCY: GaugeGroup = GaugeGroup::new(MAX_GPUS);
