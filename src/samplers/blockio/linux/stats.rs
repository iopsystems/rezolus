use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

#[metric(
    name = "blockio/latency",
    description = "Distribution of blockio operation latency in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static BLOCKIO_LATENCY: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/read/latency",
    description = "Distribution of blockio read operation latency in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static BLOCKIO_READ_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/write/latency",
    description = "Distribution of blockio write operation latency in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static BLOCKIO_WRITE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/size",
    description = "Distribution of blockio operation sizes in bytes",
    metadata = { unit = "bytes" }
)]
pub static BLOCKIO_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/read/size",
    description = "Distribution of blockio read operation sizes in bytes",
    metadata = { unit = "bytes" }
)]
pub static BLOCKIO_READ_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/write/size",
    description = "Distribution of blockio write operation sizes in bytes",
    metadata = { unit = "bytes" }
)]
pub static BLOCKIO_WRITE_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio/operations/total",
    description = "The number of completed read operations for block devices",
    formatter = blockio_ops_metric_formatter,
    metadata = { op = "read", unit = "operations" }
)]
pub static BLOCKIO_READ_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/operations/total",
    description = "The number of completed write operations for block devices",
    formatter = blockio_ops_metric_formatter,
    metadata = { op = "write", unit = "operations" }
)]
pub static BLOCKIO_WRITE_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/operations/total",
    description = "The number of completed discard operations for block devices",
    formatter = blockio_ops_metric_formatter,
    metadata = { op = "discard", unit = "operations" }
)]
pub static BLOCKIO_DISCARD_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/operations/total",
    description = "The number of completed flush operations for block devices",
    formatter = blockio_ops_metric_formatter,
    metadata = { op = "flush", unit = "operations" }
)]
pub static BLOCKIO_FLUSH_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/bytes/total",
    description = "The number of bytes read for block devices",
    formatter = blockio_bytes_metric_formatter,
    metadata = { op = "read", unit = "bytes" }
)]
pub static BLOCKIO_READ_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/bytes/total",
    description = "The number of bytes written for block devices",
    formatter = blockio_bytes_metric_formatter,
    metadata = { op = "write", unit = "bytes" }
)]
pub static BLOCKIO_WRITE_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/bytes/total",
    description = "The number of bytes discarded for block devices",
    formatter = blockio_bytes_metric_formatter,
    metadata = { op = "discard", unit = "bytes" }
)]
pub static BLOCKIO_DISCARD_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio/bytes/total",
    description = "The number of bytes flushed for block devices",
    formatter = blockio_bytes_metric_formatter,
    metadata = { op = "flush", unit = "bytes" }
)]
pub static BLOCKIO_FLUSH_BYTES: LazyCounter = LazyCounter::new(Counter::default);

// metric formatters

pub fn blockio_bytes_metric_formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            let op = metric
                .metadata()
                .get("op")
                .expect("no `op` for metric formatter");
            format!("blockio/{op}/bytes/total")
        }
        _ => metric.name().to_string(),
    }
}

pub fn blockio_ops_metric_formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            let op = metric
                .metadata()
                .get("op")
                .expect("no `op` for metric formatter");
            format!("blockio/{op}/operations/total")
        }
        _ => metric.name().to_string(),
    }
}
