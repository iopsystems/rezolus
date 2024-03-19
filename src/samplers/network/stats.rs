use crate::*;
use metriken::metric;
use metriken::Format;
use metriken::{Counter, LazyCounter, MetricEntry};

#[metric(
    name = "network/receive/bytes",
    description = "number of bytes received over network",
    formatter = network_metric_formatter
)]
pub static NETWORK_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

histogram!(NETWORK_RX_BYTES_HISTOGRAM, "network/receive/bytes");

#[metric(
    name = "network/receive/packets",
    description = "number of packets received over network",
    formatter = network_metric_formatter
)]
pub static NETWORK_RX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

histogram!(NETWORK_RX_PACKETS_HISTOGRAM, "network/receive/packets");

#[metric(
    name = "network/transmit/bytes",
    description = "number of bytes transmitted over network",
    formatter = network_metric_formatter
)]
pub static NETWORK_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

histogram!(NETWORK_TX_BYTES_HISTOGRAM, "network/transmit/bytes");

#[metric(
    name = "network/transmit/packets",
    description = "number of packets transmitted over network",
    formatter = network_metric_formatter
)]
pub static NETWORK_TX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

histogram!(NETWORK_TX_PACKETS_HISTOGRAM, "network/transmit/packets");

/// A function to format the network metrics that allows for export of both total
/// and per-nic metrics.
///
/// For the `Simple` format, the metrics will be formatted according to the
/// a pattern which depends on the metric metadata:
/// `{name}/{id}` eg: `network/rx_bytes/eth0`
/// `{name}/total` eg: `network/rx_bytes/total`
///
/// For the `Prometheus` format, if the metric has an `id` set in the metadata,
/// the metric name is left as-is. Otherwise, `/total` is appended. Note: we
/// rely on the exposition logic to convert the `/`s to `_`s in the metric name.
pub fn network_metric_formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            let name = metric.name().to_string();

            if metric.metadata().contains_key("id") {
                format!(
                    "{name}/{}",
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
