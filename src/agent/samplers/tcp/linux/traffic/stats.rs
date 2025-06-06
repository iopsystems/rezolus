use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

#[metric(
    name = "tcp_bytes",
    description = "The number of bytes received over TCP",
    metadata = { direction = "receive", unit = "bytes" }
)]
pub static TCP_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_packets",
    description = "The number of packets received over TCP",
    metadata = { direction = "receive", unit = "packets" }
)]
pub static TCP_RX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_size",
    description = "Distribution of the size of TCP packets received after reassembly",
    metadata = { direction = "receive", unit = "bytes" }
)]
pub static TCP_RX_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp_bytes",
    description = "The number of bytes transmitted over TCP",
    metadata = { direction = "transmit", unit = "bytes" }
)]
pub static TCP_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_packets",
    description = "The number of packets transmitted over TCP",
    metadata = { direction = "transmit", unit = "packets" }
)]
pub static TCP_TX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_size",
    description = "Distribution of the size of TCP packets transmitted before fragmentation",
    metadata = { direction = "transmit", unit = "bytes" }
)]
pub static TCP_TX_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "tcp_traffic"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "tcp_traffic"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);
