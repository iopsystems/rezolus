use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio read operation latency in nanoseconds",
    metadata = { op = "read", unit = "nanoseconds" }
)]
pub static BLOCKIO_READ_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio write operation latency in nanoseconds",
    metadata = { op = "write", unit = "nanoseconds" }
)]
pub static BLOCKIO_WRITE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio flush operation latency in nanoseconds",
    metadata = { op = "flush", unit = "nanoseconds" }
)]
pub static BLOCKIO_FLUSH_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio discard operation latency in nanoseconds",
    metadata = { op = "discard", unit = "nanoseconds" }
)]
pub static BLOCKIO_DISCARD_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
