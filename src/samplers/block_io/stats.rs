use crate::*;
use metriken::*;

bpfhistogram!(
    BLOCKIO_LATENCY,
    "blockio/latency",
    "distribution of block IO latencies"
);
bpfhistogram!(
    BLOCKIO_SIZE,
    "blockio/size",
    "distribution of block IO sizes"
);

#[metric(
    name = "blockio/operations",
    description = "The number of completed read operations for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "read" }
)]
pub static BLOCKIO_READ_OPS: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCKIO_READ_OPS_HISTOGRAM, "blockio/read/operations");

#[metric(
    name = "blockio/operations",
    description = "The number of completed write operations for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "write" }
)]
pub static BLOCKIO_WRITE_OPS: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCKIO_WRITE_OPS_HISTOGRAM, "blockio/write/operations");

#[metric(
    name = "blockio/operations",
    description = "The number of completed discard operations for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "discard" }
)]
pub static BLOCKIO_DISCARD_OPS: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCKIO_DISCARD_OPS_HISTOGRAM, "blockio/discard/operations");

#[metric(
    name = "blockio/operations",
    description = "The number of completed flush operations for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "flush" }
)]
pub static BLOCKIO_FLUSH_OPS: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCKIO_FLUSH_OPS_HISTOGRAM, "blockio/flush/operations");

#[metric(
    name = "blockio/bytes",
    description = "The number of bytes read for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "read" }
)]
pub static BLOCKIO_READ_BYTES: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCKIO_READ_BYTES_HISTOGRAM, "blockio/read/bytes");

#[metric(
    name = "blockio/bytes",
    description = "The number of bytes written for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "write" }
)]
pub static BLOCKIO_WRITE_BYTES: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCKIO_WRITE_BYTES_HISTOGRAM, "blockio/write/bytes");

#[metric(
    name = "blockio/bytes",
    description = "The number of bytes discarded for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "discard" }
)]
pub static BLOCKIO_DISCARD_BYTES: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCKIO_DISCARD_BYTES_HISTOGRAM, "blockio/discard/bytes");

#[metric(
    name = "blockio/bytes",
    description = "The number of bytes flushed for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "flush" }
)]
pub static BLOCKIO_FLUSH_BYTES: LazyCounter = LazyCounter::new(Counter::default);

histogram!(BLOCKIO_FLUSH_BYTES_HISTOGRAM, "blockio/flush/bytes");

/// A function to format the cpu metrics that allows for export of both total
/// and per-CPU metrics.
///
/// For the `Simple` format, the metrics will be formatted according to the
/// a pattern which depends on the metric metadata:
/// `{name}/cpu{id}` eg: `cpu/frequency/cpu0`
/// `{name}/total` eg: `cpu/cycles/total`
/// `{name}/{state}/cpu{id}` eg: `cpu/usage/user/cpu0`
/// `{name}/{state}/total` eg: `cpu/usage/user/total`
///
/// For the `Prometheus` format, if the metric has an `id` set in the metadata,
/// the metric name is left as-is. Otherwise, `/total` is appended. Note: we
/// rely on the exposition logic to convert the `/`s to `_`s in the metric name.
pub fn blockio_metric_formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            let name = if let Some(op) = metric.metadata().get("op") {
                match metric.name() {
                    "blockio/bytes" => {
                        format!("blockio/{op}/bytes")
                    }
                    "blockio/operations" => {
                        format!("blockio/{op}/operations")
                    }
                    _ => {
                        format!("{}/{op}", metric.name())
                    }
                }
            } else {
                metric.name().to_string()
            };

            format!("{name}/total")
        }
        Format::Prometheus => {
            let metadata: Vec<String> = metric
                .metadata()
                .iter()
                .map(|(key, value)| format!("{key}=\"{value}\""))
                .collect();
            let metadata = metadata.join(", ");

            let name = format!("{}/total", metric.name());

            if metadata.is_empty() {
                name
            } else {
                format!("{}{{{metadata}}}", name)
            }
        }
        _ => metriken::default_formatter(metric, format),
    }
}
