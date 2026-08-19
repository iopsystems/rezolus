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
    crate::agent::samplers::bpf_sampler_name("network_interfaces"),
    "network_interfaces_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "network_interfaces"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "network_interfaces"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "network_drop",
    description = "Packets dropped anywhere in the network stack due to errors, resource exhaustion, or policy enforcement.",
    metadata = { unit = "packets", acq_group = "network_interfaces_counters" }
)]
pub static NETWORK_DROP: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_transmit_busy",
    description = "Packets encountering retryable device busy status. High rates indicate transmit path backpressure.",
    metadata = { unit = "packets", acq_group = "network_interfaces_counters" }
)]
pub static NETWORK_TX_BUSY: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_transmit_complete",
    description = "Packets successfully transmitted by the driver. Compare with network_transmit_packets to detect transmission issues.",
    metadata = { unit = "packets", acq_group = "network_interfaces_counters" }
)]
pub static NETWORK_TX_COMPLETE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_transmit_timeout",
    description = "Transmit timeout events indicating hardware lockup or severe transmission delays. These are serious issues requiring investigation.",
    metadata = { unit = "events", acq_group = "network_interfaces_counters" }
)]
pub static NETWORK_TX_TIMEOUT: LazyCounter = LazyCounter::new(Counter::default);
