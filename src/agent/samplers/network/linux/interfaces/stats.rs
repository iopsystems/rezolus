use metriken::*;

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
    name = "network_drop",
    description = "Packets dropped anywhere in the network stack due to errors, resource exhaustion, or policy enforcement.",
    metadata = { unit = "packets" }
)]
pub static NETWORK_DROP: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_transmit_busy",
    description = "Packets encountering retryable device busy status. High rates indicate transmit path backpressure.",
    metadata = { unit = "packets" }
)]
pub static NETWORK_TX_BUSY: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_transmit_complete",
    description = "Packets successfully transmitted by the driver. Compare with network_transmit_packets to detect transmission issues.",
    metadata = { unit = "packets" }
)]
pub static NETWORK_TX_COMPLETE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_transmit_timeout",
    description = "Transmit timeout events indicating hardware lockup or severe transmission delays. These are serious issues requiring investigation.",
    metadata = { unit = "events" }
)]
pub static NETWORK_TX_TIMEOUT: LazyCounter = LazyCounter::new(Counter::default);
