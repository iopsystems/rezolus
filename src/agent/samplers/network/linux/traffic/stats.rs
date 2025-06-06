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
    name = "network_bytes",
    description = "The number of bytes received over the network",
    metadata = { direction = "receive", unit = "bytes" }
)]
pub static NETWORK_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_packets",
    description = "The number of packets received over the network",
    metadata = { direction = "receive", unit = "packets" }
)]
pub static NETWORK_RX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_bytes",
    description = "The number of bytes transmitted over the network",
    metadata = { direction = "transmit", unit = "bytes" }
)]
pub static NETWORK_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "network_packets",
    description = "The number of packets transmitted over the network",
    metadata = { direction = "transmit", unit = "packets" }
)]
pub static NETWORK_TX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);
