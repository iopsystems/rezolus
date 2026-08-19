use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `network/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
/// Brackets the `counters` map's refresh (single writer: this sampler's
/// own BPF refresh path).
pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("network_traffic"),
    "network_traffic_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "network_traffic"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "network_traffic"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "network_bytes",
    description = "The number of bytes transferred over the network",
    metadata = { direction = "receive", unit = "bytes", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_packets",
    description = "The number of packets transferred over the network",
    metadata = { direction = "receive", unit = "packets", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_RX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_bytes",
    description = "The number of bytes transferred over the network",
    metadata = { direction = "transmit", unit = "bytes", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_packets",
    description = "The number of packets transferred over the network",
    metadata = { direction = "transmit", unit = "packets", acq_group = "network_traffic_counters" }
)]
pub static NETWORK_TX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);
