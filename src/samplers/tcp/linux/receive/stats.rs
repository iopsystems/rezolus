use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

#[metric(
    name = "tcp_jitter",
    description = "Distribution of TCP latency jitter",
    metadata = { unit = "nanoseconds" }
)]
pub static TCP_JITTER: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp_srtt",
    description = "Distribution of TCP smoothed round-trip time",
    metadata = { unit = "nanoseconds" }
)]
pub static TCP_SRTT: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
