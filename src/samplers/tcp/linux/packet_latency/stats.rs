use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

#[metric(
    name = "tcp/packet_latency",
    description = "Distribution of latency from a socket becoming readable until a userspace read",
    metadata = { unit = "nanoseconds" }
)]
pub static TCP_PACKET_LATENCY: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
