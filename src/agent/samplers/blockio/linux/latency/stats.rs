use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `blockio/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
// ONE group for all 4 op-class latency histograms: LIKE ENTITIES (one
// "blockio latency" family, distinguished by the `op` label) read as a
// single sweep, not 4 independent read sections — see the `# Granularity
// rule` on `crate::agent::samplers::ACQUISITION_GROUPS`.
// `BpfBuilder::histogram` batches every call naming this group into one
// `HistogramBatch`, stamped once per refresh — see `bpf/histogram.rs`.
pub static LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("blockio_latency"),
    "blockio_latency_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static LATENCIES_ACQ_REG: &'static AcquisitionGroup = &LATENCIES_ACQ;

// A SECOND group for the 4 queue-residency histograms. Queue residency and
// service latency are different metric families -- they measure different
// phases of a request's life and move independently under saturation -- and
// principle 18 keeps families in their own groups even when they are read
// back-to-back in the same refresh. Same shape as `LATENCIES_ACQ`: the 4 op
// classes within THIS family are like entities and share one group.
pub static QUEUE_LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("blockio_latency"),
    "blockio_latency_queue_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static QUEUE_LATENCIES_ACQ_REG: &'static AcquisitionGroup = &QUEUE_LATENCIES_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "blockio_latency"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "blockio_latency"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio operation latency in nanoseconds",
    metadata = { op = "read", unit = "nanoseconds", acq_group = "blockio_latency_latencies" }
)]
pub static BLOCKIO_READ_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio operation latency in nanoseconds",
    metadata = { op = "write", unit = "nanoseconds", acq_group = "blockio_latency_latencies" }
)]
pub static BLOCKIO_WRITE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio operation latency in nanoseconds",
    metadata = { op = "flush", unit = "nanoseconds", acq_group = "blockio_latency_latencies" }
)]
pub static BLOCKIO_FLUSH_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_latency",
    description = "Distribution of blockio operation latency in nanoseconds",
    metadata = { op = "discard", unit = "nanoseconds", acq_group = "blockio_latency_latencies" }
)]
pub static BLOCKIO_DISCARD_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_queue_latency",
    description = "Distribution of time block IO requests spent queued before the device began servicing them, in nanoseconds",
    metadata = { op = "read", unit = "nanoseconds", acq_group = "blockio_latency_queue_latencies" }
)]
pub static BLOCKIO_READ_QUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_queue_latency",
    description = "Distribution of time block IO requests spent queued before the device began servicing them, in nanoseconds",
    metadata = { op = "write", unit = "nanoseconds", acq_group = "blockio_latency_queue_latencies" }
)]
pub static BLOCKIO_WRITE_QUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_queue_latency",
    description = "Distribution of time block IO requests spent queued before the device began servicing them, in nanoseconds",
    metadata = { op = "flush", unit = "nanoseconds", acq_group = "blockio_latency_queue_latencies" }
)]
pub static BLOCKIO_FLUSH_QUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_queue_latency",
    description = "Distribution of time block IO requests spent queued before the device began servicing them, in nanoseconds",
    metadata = { op = "discard", unit = "nanoseconds", acq_group = "blockio_latency_queue_latencies" }
)]
pub static BLOCKIO_DISCARD_QUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
