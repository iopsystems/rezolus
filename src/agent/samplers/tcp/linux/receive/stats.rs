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

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "tcp_receive"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "tcp_receive"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);
