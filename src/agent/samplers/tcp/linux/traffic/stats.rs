use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// by the macOS sampler and, on other platforms, by `tcp/mod.rs`'s `mod stats`
// fallback, to keep metric identity stable across platforms.

/// The sampler the groups below belong to: `tcp_traffic` where that sampler
/// exists (Linux, macOS), `unattributed` elsewhere.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const GROUP_SAMPLER: &str = "tcp_traffic";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const GROUP_SAMPLER: &str = crate::agent::samplers::bpf_sampler_name("tcp_traffic");

/// Brackets one read of the counters (the BPF `counters` map on Linux, the
/// `net.inet.tcp.stats` sysctl on macOS); the sampler's `refresh()` is the
/// only writer.
pub static COUNTERS_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "tcp_traffic_counters");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

/// ONE group for both size histograms: LIKE ENTITIES (one "tcp size"
/// family, distinguished by the `direction` label) read as a single sweep
/// — see the `# Granularity rule` on
/// `crate::agent::samplers::ACQUISITION_GROUPS`. Distinct from
/// `COUNTERS_ACQ` above, a different metric family (bytes/packets
/// counters vs. size distribution). `BpfBuilder::histogram` batches every
/// call naming this group into one `HistogramBatch`, stamped once per
/// refresh — see `bpf/histogram.rs`.
pub static SIZES_ACQ: AcquisitionGroup = AcquisitionGroup::new(GROUP_SAMPLER, "tcp_traffic_sizes");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static SIZES_ACQ_REG: &'static AcquisitionGroup = &SIZES_ACQ;

#[metric(
    name = "tcp_bytes",
    description = "The number of bytes transferred over TCP",
    metadata = { direction = "receive", unit = "bytes", acq_group = "tcp_traffic_counters" }
)]
pub static TCP_RX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_packets",
    description = "The number of packets transferred over TCP",
    metadata = { direction = "receive", unit = "packets", acq_group = "tcp_traffic_counters" }
)]
pub static TCP_RX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_size",
    description = "Distribution of the size of TCP packets transferred, ignoring fragmentation",
    metadata = { direction = "receive", unit = "bytes", acq_group = "tcp_traffic_sizes" }
)]
pub static TCP_RX_SIZE: RwLockCounterHistogram =
    RwLockCounterHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "tcp_bytes",
    description = "The number of bytes transferred over TCP",
    metadata = { direction = "transmit", unit = "bytes", acq_group = "tcp_traffic_counters" }
)]
pub static TCP_TX_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_packets",
    description = "The number of packets transferred over TCP",
    metadata = { direction = "transmit", unit = "packets", acq_group = "tcp_traffic_counters" }
)]
pub static TCP_TX_PACKETS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "tcp_size",
    description = "Distribution of the size of TCP packets transferred, ignoring fragmentation",
    metadata = { direction = "transmit", unit = "bytes", acq_group = "tcp_traffic_sizes" }
)]
pub static TCP_TX_SIZE: RwLockCounterHistogram =
    RwLockCounterHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

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
