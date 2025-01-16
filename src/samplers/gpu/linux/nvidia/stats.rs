use metriken::*;

use crate::common::*;

const MAX_GPUS: usize = 32;

// Memory

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory free.",
    formatter = gpu_metric_formatter,
    metadata = { state = "free", unit = "bytes" }
)]
pub static GPU_MEMORY_FREE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory",
    description = "The amount of GPU memory used.",
    formatter = gpu_metric_formatter,
    metadata = { state = "used", unit = "bytes" }
)]
pub static GPU_MEMORY_USED: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// PCIe

#[metric(
    name = "gpu_pcie_bandwidth",
    description = "The PCIe bandwidth in Bytes/s.",
    formatter = gpu_metric_formatter,
    metadata = { direction = "receive", unit = "bytes/second" }
)]
pub static GPU_PCIE_BANDWIDTH: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe receive throughput in Bytes/s.",
    formatter = gpu_metric_formatter,
    metadata = { direction = "receive", unit = "bytes/second" }
)]
pub static GPU_PCIE_THROUGHPUT_RX: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_pcie_throughput",
    description = "The current PCIe transmit throughput in Bytes/s.",
    formatter = gpu_metric_formatter,
    metadata = { direction = "transmit", unit = "bytes/second" }
)]
pub static GPU_PCIE_THROUGHPUT_TX: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Power and Energy

#[metric(
    name = "gpu_power_usage",
    description = "The current power usage in milliwatts (mW).",
    formatter = gpu_metric_formatter,
    metadata = { unit = "milliwatts" }
)]
pub static GPU_POWER_USAGE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_energy_consumption",
    description = "The energy consumption in milliJoules (mJ).",
    formatter = gpu_metric_formatter,
    metadata = { unit = "milliJoules" }
)]
pub static GPU_ENERGY_CONSUMPTION: CounterGroup = CounterGroup::new(MAX_GPUS);

// Thermals

#[metric(
    name = "gpu_temperature",
    description = "The current temperature in degrees Celsius (C).",
    formatter = gpu_metric_formatter,
    metadata = { unit = "Celsius" }
)]
pub static GPU_TEMPERATURE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Clocks

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    formatter = gpu_metric_formatter,
    metadata = { clock = "compute", unit = "Hz" }
)]
pub static GPU_CLOCK_COMPUTE: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    formatter = gpu_metric_formatter,
    metadata = { clock = "graphics", unit = "Hz" }
)]
pub static GPU_CLOCK_GRAPHICS: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    formatter = gpu_metric_formatter,
    metadata = { clock = "memory", unit = "Hz" }
)]
pub static GPU_CLOCK_MEMORY: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_clock",
    description = "The current clock speed in Hertz (Hz).",
    formatter = gpu_metric_formatter,
    metadata = { clock = "video", unit = "Hz" }
)]
pub static GPU_CLOCK_VIDEO: GaugeGroup = GaugeGroup::new(MAX_GPUS);

// Utilization

#[metric(
    name = "gpu_utilization",
    description = "The running average percentage of time the GPU was executing one or more kernels. (0-100).",
    formatter = gpu_metric_formatter,
    metadata = { unit = "percentage" }
)]
pub static GPU_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

#[metric(
    name = "gpu_memory_utilization",
    description = "The running average percentage of time that GPU memory was being read from or written to. (0-100).",
    formatter = gpu_metric_formatter,
    metadata = { unit = "percentage" }
)]
pub static GPU_MEMORY_UTILIZATION: GaugeGroup = GaugeGroup::new(MAX_GPUS);

/// A function to format the gpu metrics that allows for export of both total
/// and per-GPU metrics.
///
/// For the `Simple` format, the metrics will be formatted according to the
/// a pattern which depends on the metric metadata:
/// `{name}/gpu{id}` eg: `gpu/energy_consumption/gpu0`
/// `{name}/total` eg: `gpu/energy_consumption/total`
///
/// For the `Prometheus` format, if the metric has an `id` set in the metadata,
/// the metric name is left as-is. Otherwise, `/total` is appended. Note: we
/// rely on the exposition logic to convert the `/`s to `_`s in the metric name.
pub fn gpu_metric_formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            let name = if let Some(direction) = metric.metadata().get("direction") {
                format!("{}/{direction}", metric.name())
            } else {
                metric.name().to_string()
            };

            let name = if let Some(state) = metric.metadata().get("state") {
                format!("{name}/{state}")
            } else {
                name
            };

            let name = if let Some(clock) = metric.metadata().get("clock") {
                format!("{name}/{clock}")
            } else {
                name
            };

            format!("{name}/gpu")
        }
        _ => metric.name().to_string(),
    }
}
