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
// ONE group per phase, three groups total. The 4 op classes within a phase are
// LIKE ENTITIES (one family, distinguished by the `op` label) read as a single
// sweep; the three phases are different families measuring different parts of a
// request's life, and principle 18 keeps families in their own groups even when
// they are read back-to-back in one refresh.
// `BpfBuilder::histogram` batches every call naming a group into one
// `HistogramBatch`, stamped once per refresh — see `bpf/histogram.rs`.
pub static DEVICE_LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("blockio_latency"),
    "blockio_latency_device_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static DEVICE_LATENCIES_ACQ_REG: &'static AcquisitionGroup = &DEVICE_LATENCIES_ACQ;

pub static QUEUE_LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("blockio_latency"),
    "blockio_latency_queue_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static QUEUE_LATENCIES_ACQ_REG: &'static AcquisitionGroup = &QUEUE_LATENCIES_ACQ;

pub static TOTAL_LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("blockio_latency"),
    "blockio_latency_total_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static TOTAL_LATENCIES_ACQ_REG: &'static AcquisitionGroup = &TOTAL_LATENCIES_ACQ;

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
    name = "blockio_device_latency",
    description = "Distribution of block IO device service latency in nanoseconds, from the moment the device began servicing the request until it completed. Excludes time spent waiting in the queue — see blockio_queue_latency. Recordings made before this metric was renamed carry the same measurement under the name blockio_latency",
    metadata = { op = "read", unit = "nanoseconds", acq_group = "blockio_latency_device_latencies" }
)]
pub static BLOCKIO_READ_DEVICE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_device_latency",
    description = "Distribution of block IO device service latency in nanoseconds, from the moment the device began servicing the request until it completed. Excludes time spent waiting in the queue — see blockio_queue_latency. Recordings made before this metric was renamed carry the same measurement under the name blockio_latency",
    metadata = { op = "write", unit = "nanoseconds", acq_group = "blockio_latency_device_latencies" }
)]
pub static BLOCKIO_WRITE_DEVICE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_device_latency",
    description = "Distribution of block IO device service latency in nanoseconds, from the moment the device began servicing the request until it completed. Excludes time spent waiting in the queue — see blockio_queue_latency. Recordings made before this metric was renamed carry the same measurement under the name blockio_latency",
    metadata = { op = "flush", unit = "nanoseconds", acq_group = "blockio_latency_device_latencies" }
)]
pub static BLOCKIO_FLUSH_DEVICE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_device_latency",
    description = "Distribution of block IO device service latency in nanoseconds, from the moment the device began servicing the request until it completed. Excludes time spent waiting in the queue — see blockio_queue_latency. Recordings made before this metric was renamed carry the same measurement under the name blockio_latency",
    metadata = { op = "discard", unit = "nanoseconds", acq_group = "blockio_latency_device_latencies" }
)]
pub static BLOCKIO_DISCARD_DEVICE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_queue_latency",
    description = "Distribution of time block IO requests spent queued before the device began servicing them, in nanoseconds. This is the component that grows under device saturation, where service latency alone stays flat",
    metadata = { op = "read", unit = "nanoseconds", acq_group = "blockio_latency_queue_latencies" }
)]
pub static BLOCKIO_READ_QUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_queue_latency",
    description = "Distribution of time block IO requests spent queued before the device began servicing them, in nanoseconds. This is the component that grows under device saturation, where service latency alone stays flat",
    metadata = { op = "write", unit = "nanoseconds", acq_group = "blockio_latency_queue_latencies" }
)]
pub static BLOCKIO_WRITE_QUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_queue_latency",
    description = "Distribution of time block IO requests spent queued before the device began servicing them, in nanoseconds. This is the component that grows under device saturation, where service latency alone stays flat",
    metadata = { op = "flush", unit = "nanoseconds", acq_group = "blockio_latency_queue_latencies" }
)]
pub static BLOCKIO_FLUSH_QUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_queue_latency",
    description = "Distribution of time block IO requests spent queued before the device began servicing them, in nanoseconds. This is the component that grows under device saturation, where service latency alone stays flat",
    metadata = { op = "discard", unit = "nanoseconds", acq_group = "blockio_latency_queue_latencies" }
)]
pub static BLOCKIO_DISCARD_QUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_total_latency",
    description = "Distribution of end-to-end block IO latency in nanoseconds, from the request entering the queue until it completed — the queue and device phases together. Measured directly rather than summed, because two histograms cannot be added",
    metadata = { op = "read", unit = "nanoseconds", acq_group = "blockio_latency_total_latencies" }
)]
pub static BLOCKIO_READ_TOTAL_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_total_latency",
    description = "Distribution of end-to-end block IO latency in nanoseconds, from the request entering the queue until it completed — the queue and device phases together. Measured directly rather than summed, because two histograms cannot be added",
    metadata = { op = "write", unit = "nanoseconds", acq_group = "blockio_latency_total_latencies" }
)]
pub static BLOCKIO_WRITE_TOTAL_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_total_latency",
    description = "Distribution of end-to-end block IO latency in nanoseconds, from the request entering the queue until it completed — the queue and device phases together. Measured directly rather than summed, because two histograms cannot be added",
    metadata = { op = "flush", unit = "nanoseconds", acq_group = "blockio_latency_total_latencies" }
)]
pub static BLOCKIO_FLUSH_TOTAL_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_total_latency",
    description = "Distribution of end-to-end block IO latency in nanoseconds, from the request entering the queue until it completed — the queue and device phases together. Measured directly rather than summed, because two histograms cannot be added",
    metadata = { op = "discard", unit = "nanoseconds", acq_group = "blockio_latency_total_latencies" }
)]
pub static BLOCKIO_DISCARD_TOTAL_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
