use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

#[metric(
    name = "tcp_connect_latency",
    description = "Distribution of latency for establishing outbound connections (active open)",
    metadata = { unit = "nanoseconds" }
)]
pub static TCP_CONNECT_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
