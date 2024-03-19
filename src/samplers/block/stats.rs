use crate::*;
use metriken::metric;
use metriken::Format;
use metriken::{Counter, LazyCounter, MetricEntry};

#[metric(
    name = "block/read/bytes",
    description = "number of read bytes ",
    formatter = block_metric_formatter
)]
pub static BLOCK_READ_BYTES: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCK_READ_BYTES_HISTOGRAM, "block/read/bytes");

#[metric(
    name = "block/read/ios",
    description = "number of read IOs",
    formatter = block_metric_formatter
)]
pub static BLOCK_READ_IOS: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCK_READ_IOS_HISTOGRAM, "block/read/ios");

#[metric(
    name = "block/write/bytes",
    description = "number of write bytes",
    formatter = block_metric_formatter
)]
pub static BLOCK_WRITE_BYTES: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCK_WRITE_BYTES_HISTOGRAM, "block/write/bytes");

#[metric(
    name = "block/write/ios",
    description = "number of writte IOs",
    formatter = block_metric_formatter
)]
pub static BLOCK_WRITE_IOS: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCK_WRITE_IOS_HISTOGRAM, "block/write/ios");

/// A function to format the block metrics that allows for export of both total
/// and per-block metrics.
///
/// For the `Simple` format, the metrics will be formatted according to the
/// a pattern which depends on the metric metadata:
/// `{name}/{id}` eg: `block/read/bytes/eth0`
/// `{name}/total` eg: `block/read/bytes/total`
///
/// For the `Prometheus` format, if the metric has an `id` set in the metadata,
/// the metric name is left as-is. Otherwise, `/total` is appended. Note: we
/// rely on the exposition logic to convert the `/`s to `_`s in the metric name.
pub fn block_metric_formatter(metric: &MetricEntry, format: Format) -> String {
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
