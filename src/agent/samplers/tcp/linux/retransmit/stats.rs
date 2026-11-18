use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// by the macOS sampler and, on other platforms, by `tcp/mod.rs`'s `mod stats`
// fallback, to keep metric identity stable across platforms.

/// The sampler the groups below belong to: `tcp_retransmit` where that sampler
/// exists (Linux, macOS), `unattributed` elsewhere.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const GROUP_SAMPLER: &str = "tcp_retransmit";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const GROUP_SAMPLER: &str = crate::agent::samplers::bpf_sampler_name("tcp_retransmit");

/// Brackets one read of the counters (the BPF `counters` map on Linux, the
/// `net.inet.tcp.stats` sysctl on macOS); the sampler's `refresh()` is the
/// only writer.
pub static COUNTERS_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "tcp_retransmit_counters");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

#[metric(
    name = "tcp_retransmit",
    description = "The number of TCP packets that were re-transmitted",
    metadata = { unit = "packets", acq_group = "tcp_retransmit_counters" }
)]
pub static TCP_RETRANSMIT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "tcp_retransmit"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "tcp_retransmit"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);
