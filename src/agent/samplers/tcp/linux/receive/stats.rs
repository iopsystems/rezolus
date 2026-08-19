use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `tcp/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only. One group per histogram map read (each `Histogram` reads
// exactly one BPF map per refresh).
pub static JITTER_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("tcp_receive"),
    "tcp_receive_jitter",
);
pub static SRTT_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("tcp_receive"),
    "tcp_receive_srtt",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static JITTER_ACQ_REG: &'static AcquisitionGroup = &JITTER_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static SRTT_ACQ_REG: &'static AcquisitionGroup = &SRTT_ACQ;

#[metric(
    name = "tcp_jitter",
    description = "Distribution of TCP latency jitter",
    metadata = { unit = "nanoseconds", acq_group = "tcp_receive_jitter" }
)]
pub static TCP_JITTER: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp_srtt",
    description = "Distribution of TCP smoothed round-trip time",
    metadata = { unit = "nanoseconds", acq_group = "tcp_receive_srtt" }
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
