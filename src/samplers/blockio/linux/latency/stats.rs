use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio operation latency in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static BLOCKIO_LATENCY: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_read_latency",
    description = "Distribution of blockio read operation latency in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static BLOCKIO_READ_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_write_latency",
    description = "Distribution of blockio write operation latency in nanoseconds",
    metadata = { unit = "nanoseconds" }
)]
pub static BLOCKIO_WRITE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
