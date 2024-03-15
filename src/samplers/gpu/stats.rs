use metriken::{metric, Format, Gauge, LazyGauge, MetricEntry};

#[metric(
    name = "gpu/memory",
    description = "The total amount of GPU memory free.",
    formatter = gpu_metric_formatter,
    metadata = { state = "free" }
)]
pub static GPU_MEMORY_FREE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "gpu/memory",
    description = "The total amount of GPU memory used.",
    formatter = gpu_metric_formatter,
    metadata = { state = "used" }
)]
pub static GPU_MEMORY_USED: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "gpu/pcie/bandwidth",
    description = "The total PCIe bandwidth in Bytes/s.",
    formatter = gpu_metric_formatter,
    metadata = { direction = "receive" }
)]
pub static GPU_PCIE_BANDWIDTH: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "gpu/pcie/throughput",
    description = "The current PCIe throughput in Bytes/s.",
    formatter = gpu_metric_formatter,
    metadata = { direction = "receive" }
)]
pub static GPU_PCIE_THROUGHPUT_RX: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "gpu/pcie/throughput",
    description = "The current PCIe throughput in Bytes/s.",
    formatter = gpu_metric_formatter,
    metadata = { direction = "transmit" }
)]
pub static GPU_PCIE_THROUGHPUT_TX: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "gpu/power/usage",
    description = "The current power usage in milliwatts (mW).",
    formatter = gpu_metric_formatter
)]
pub static GPU_POWER_USAGE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "gpu/utilization/gpu",
    description = "The running average percentage of time the GPU was executing one or more kernels. (0-100).",
    formatter = gpu_metric_formatter
)]
pub static GPU_UTILIZATION: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "gpu/memory_utilization",
    description = "The running average percentage of time that GPU memory was being read from or written to. (0-100).",
    formatter = gpu_metric_formatter
)]
pub static GPU_MEMORY_UTILIZATION: LazyGauge = LazyGauge::new(Gauge::default);

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

            let name = if let Some(ty) = metric.metadata().get("type") {
                format!("{name}/{ty}")
            } else {
                name
            };

            if metric.metadata().contains_key("id") {
                format!(
                    "{name}/gpu{}",
                    metric.metadata().get("id").unwrap_or("unknown"),
                )
            } else {
                format!("{name}/total",)
            }
        }
        Format::Prometheus => {
            let metadata: Vec<String> = metric
                .metadata()
                .iter()
                .map(|(key, value)| format!("{key}=\"{value}\""))
                .collect();
            let metadata = metadata.join(", ");

            let name = if metric.metadata().contains_key("id") {
                metric.name().to_string()
            } else {
                format!("{}/total", metric.name())
            };

            if metadata.is_empty() {
                name
            } else {
                format!("{}{{{metadata}}}", name)
            }
        }
        _ => metriken::default_formatter(metric, format),
    }
}
