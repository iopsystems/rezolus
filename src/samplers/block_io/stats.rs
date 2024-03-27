use crate::common::HISTOGRAM_GROUPING_POWER;
use crate::*;
use metriken::*;

#[metric(
    name = "blockio/latency",
    description = "Distribution of blockio operation latency in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static BLOCKIO_LATENCY: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/size",
    description = "Distribution of blockio operation sizes in bytes",
    metadata = { unit = "bytes" }
)]
pub static BLOCKIO_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/operations",
    description = "The number of completed read operations for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "read", unit = "operations" }
)]
pub static BLOCKIO_READ_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/read/operations",
    description = "Distribution of blockio read operation rates from sample to sample",
    metadata = { unit = "operations/second" }
)]
pub static BLOCKIO_READ_OPS_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/operations",
    description = "The number of completed write operations for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "write", unit = "operations" }
)]
pub static BLOCKIO_WRITE_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/write/operations",
    description = "Distribution of blockio write operation rates from sample to sample",
    metadata = { unit = "operations/second" }
)]
pub static BLOCKIO_WRITE_OPS_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/operations",
    description = "The number of completed discard operations for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "discard", unit = "operations" }
)]
pub static BLOCKIO_DISCARD_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/discard/operations",
    description = "Distribution of blockio discard operation rates from sample to sample",
    metadata = { unit = "operations/second" }
)]
pub static BLOCKIO_DISCARD_OPS_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/operations",
    description = "The number of completed flush operations for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "flush", unit = "operations" }
)]
pub static BLOCKIO_FLUSH_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/flush/operations",
    description = "Distribution of blockio flush operation rates from sample to sample",
    metadata = { unit = "operations/second" }
)]
pub static BLOCKIO_FLUSH_OPS_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/bytes",
    description = "The number of bytes read for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "read", unit = "bytes" }
)]
pub static BLOCKIO_READ_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/read/bytes",
    description = "Distribution of blockio read throughput from sample to sample",
    metadata = { unit = "bytes/second" }
)]
pub static BLOCKIO_READ_BYTES_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/bytes",
    description = "The number of bytes written for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "write", unit = "bytes" }
)]
pub static BLOCKIO_WRITE_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/write/bytes",
    description = "Distribution of blockio write throughput from sample to sample",
    metadata = { unit = "bytes/second" }
)]
pub static BLOCKIO_WRITE_BYTES_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/bytes",
    description = "The number of bytes discarded for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "discard", unit = "bytes" }
)]
pub static BLOCKIO_DISCARD_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/discard/bytes",
    description = "Distribution of blockio discard throughput from sample to sample",
    metadata = { unit = "bytes/second" }
)]
pub static BLOCKIO_DISCARD_BYTES_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/bytes",
    description = "The number of bytes flushed for block devices",
    formatter = blockio_metric_formatter,
    metadata = { op = "flush", unit = "bytes" }
)]
pub static BLOCKIO_FLUSH_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/flush/bytes",
    description = "Distribution of blockio flush throughput from sample to sample",
    metadata = { unit = "bytes/second" }
)]
pub static BLOCKIO_FLUSH_BYTES_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

/// A function to format the blockio metrics that allows for export of ops and
/// byte counters by operation type.
///
/// Note: we do not currently support per-device metrics.
///
/// For the `Simple` format, the metrics will be formatted according to the
/// a pattern which depends on the metric metadata:
/// `blockio/{op}/{operations, bytes}/total` eg: `blockio/read/operations/total`
///
/// For the `Prometheus` format, we supply the operation type as metadata. Note:
/// we rely on the exposition logic to convert the `/`s to `_`s in the metric
/// name.
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
