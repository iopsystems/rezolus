use metriken::*;

use super::MAX_GPUS;

// Memory

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory free.",
    metadata = { vendor = "amd", state = "free", unit = "bytes" }
)]
pub static GPU_MEMORY_FREE: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory used.",
    metadata = { vendor = "amd", state = "used", unit = "bytes" }
)]
pub static GPU_MEMORY_USED: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

// PCIe

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe receive throughput in Bytes/s.",
    metadata = { vendor = "amd", direction = "receive", unit = "bytes/second" }
)]
pub static GPU_PCIE_THROUGHPUT_RX: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe transmit throughput in Bytes/s.",
    metadata = { vendor = "amd", direction = "transmit", unit = "bytes/second" }
)]
pub static GPU_PCIE_THROUGHPUT_TX: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

// Power and Energy

#[metric(
    name = "gpu_power_usage",
    description = "The current power usage in milliwatts (mW).",
    metadata = { vendor = "amd", unit = "milliwatts" }
)]
pub static GPU_POWER_USAGE: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_energy_consumption",
    description = "The energy consumption in milliJoules (mJ).",
    metadata = { vendor = "amd", unit = "milliJoules" }
)]
pub static GPU_ENERGY_CONSUMPTION: WindowedCounterGroup = WindowedCounterGroup::new(MAX_GPUS);

// Thermals

#[metric(
    name = "gpu_temperature",
    description = "The current edge temperature in degrees Celsius (C).",
    metadata = { vendor = "amd", sensor = "edge", unit = "Celsius" }
)]
pub static GPU_TEMPERATURE_EDGE: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_temperature",
    description = "The current junction (hotspot) temperature in degrees Celsius (C).",
    metadata = { vendor = "amd", sensor = "junction", unit = "Celsius" }
)]
pub static GPU_TEMPERATURE_JUNCTION: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_temperature",
    description = "The current memory (VRAM) temperature in degrees Celsius (C).",
    metadata = { vendor = "amd", sensor = "memory", unit = "Celsius" }
)]
pub static GPU_TEMPERATURE_MEMORY: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

// Clocks

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "amd", clock = "compute", unit = "Hz" }
)]
pub static GPU_CLOCK_COMPUTE: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "amd", clock = "graphics", unit = "Hz" }
)]
pub static GPU_CLOCK_GRAPHICS: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    metadata = { vendor = "amd", clock = "memory", unit = "Hz" }
)]
pub static GPU_CLOCK_MEMORY: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

// Utilization

#[metric(
    name = "gpu_utilization",
    description = "The percentage of time the GPU was busy executing work. (0-100).",
    metadata = { vendor = "amd", unit = "percentage" }
)]
pub static GPU_UTILIZATION: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory_utilization",
    description = "The percentage of time the GPU memory controller was busy. (0-100).",
    metadata = { vendor = "amd", unit = "percentage" }
)]
pub static GPU_MEMORY_UTILIZATION: WindowedGaugeGroup = WindowedGaugeGroup::new(MAX_GPUS);
