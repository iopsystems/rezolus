use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// The `blockio` sampler replaced `blockio_requests` and `blockio_latency`;
// the acquisition groups keep the names they had under those samplers.
//
// Registered here (not in mod.rs) because this file is compiled three ways:
// as the Linux BPF sampler's `stats` module, inside the macOS sampler
// (`blockio/macos/mod.rs`), and on every other platform under
// `blockio/mod.rs`'s fallback `mod stats`, which keeps metric identity stable
// across platforms. One group per `.counters()` map.

/// The sampler these groups belong to. Where a `blockio` sampler exists
/// (Linux, macOS) the metrics below attribute to it, so the groups must name
/// it; elsewhere the metrics attribute to `unattributed`, and so do the
/// groups (see `crate::agent::samplers::bpf_sampler_name`).
#[cfg(any(target_os = "linux", target_os = "macos"))]
const GROUP_SAMPLER: &str = "blockio";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const GROUP_SAMPLER: &str = crate::agent::samplers::bpf_sampler_name("blockio");

pub static COUNTERS_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "blockio_requests_counters");
pub static ERRORS_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "blockio_requests_errors");
pub static REQUEUES_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "blockio_requests_requeues");

// ONE group for all 4 op-class size histograms: LIKE ENTITIES (one
// "blockio size" family, distinguished by the `op` label) read as a single
// sweep — see the `# Granularity rule` on
// `crate::agent::samplers::ACQUISITION_GROUPS`. Distinct from the 3
// counter groups above, which bracket separate `.counters()` maps (a
// different metric family each). `BpfBuilder::histogram` batches every
// call naming this group into one `HistogramBatch`, stamped once per
// refresh — see `bpf/histogram.rs`.
pub static SIZES_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "blockio_requests_sizes");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static ERRORS_ACQ_REG: &'static AcquisitionGroup = &ERRORS_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static REQUEUES_ACQ_REG: &'static AcquisitionGroup = &REQUEUES_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static SIZES_ACQ_REG: &'static AcquisitionGroup = &SIZES_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "blockio"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "blockio"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "blockio_size",
    description = "Distribution of blockio operation sizes in bytes",
    metadata = { op = "read", unit = "bytes", acq_group = "blockio_requests_sizes" }
)]
pub static BLOCKIO_READ_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_size",
    description = "Distribution of blockio operation sizes in bytes",
    metadata = { op = "write", unit = "bytes", acq_group = "blockio_requests_sizes" }
)]
pub static BLOCKIO_WRITE_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_size",
    description = "Distribution of blockio operation sizes in bytes",
    metadata = { op = "flush", unit = "bytes", acq_group = "blockio_requests_sizes" }
)]
pub static BLOCKIO_FLUSH_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_size",
    description = "Distribution of blockio operation sizes in bytes",
    metadata = { op = "discard", unit = "bytes", acq_group = "blockio_requests_sizes" }
)]
pub static BLOCKIO_DISCARD_SIZE: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_operations",
    description = "The number of completed operations for block devices",
    metadata = { op = "read", unit = "operations", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_READ_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_operations",
    description = "The number of completed operations for block devices",
    metadata = { op = "write", unit = "operations", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_WRITE_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_operations",
    description = "The number of completed operations for block devices",
    metadata = { op = "discard", unit = "operations", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_DISCARD_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_operations",
    description = "The number of completed operations for block devices",
    metadata = { op = "flush", unit = "operations", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_FLUSH_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_bytes",
    description = "The number of bytes transferred for block device operations",
    metadata = { op = "read", unit = "bytes", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_READ_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_bytes",
    description = "The number of bytes transferred for block device operations",
    metadata = { op = "write", unit = "bytes", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_WRITE_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_bytes",
    description = "The number of bytes transferred for block device operations",
    metadata = { op = "discard", unit = "bytes", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_DISCARD_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_bytes",
    description = "The number of bytes transferred for block device operations",
    metadata = { op = "flush", unit = "bytes", acq_group = "blockio_requests_counters" }
)]
pub static BLOCKIO_FLUSH_BYTES: LazyCounter = LazyCounter::new(Counter::default);

/*
 * blockio_errors — terminal block IO failures bucketed by op and
 * error class. The error classes correspond to coarse blk_status_t
 * groupings:
 *   io          — generic IO error / medium error
 *   timeout     — block layer per-request timer fired
 *   nospc       — thin-provisioned storage out of physical capacity
 *   target      — target rejected (illegal request, namespace, reservation)
 *   protection  — T10 PI / DIF/DIX or NVMe end-to-end check failed
 *   unsupported — operation not supported by the device
 *   other       — anything else (transport, resource, zone, offline, …)
 */

// op = read
#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "io", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_READ_ERR_IO: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "timeout", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_READ_ERR_TIMEOUT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "nospc", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_READ_ERR_NOSPC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "target", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_READ_ERR_TARGET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "protection", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_READ_ERR_PROTECTION: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "unsupported", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_READ_ERR_UNSUPPORTED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "other", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_READ_ERR_OTHER: LazyCounter = LazyCounter::new(Counter::default);

// op = write
#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "io", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_WRITE_ERR_IO: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "timeout", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_WRITE_ERR_TIMEOUT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "nospc", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_WRITE_ERR_NOSPC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "target", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_WRITE_ERR_TARGET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "protection", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_WRITE_ERR_PROTECTION: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "unsupported", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_WRITE_ERR_UNSUPPORTED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "other", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_WRITE_ERR_OTHER: LazyCounter = LazyCounter::new(Counter::default);

// op = flush
#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "io", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_FLUSH_ERR_IO: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "timeout", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_FLUSH_ERR_TIMEOUT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "nospc", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_FLUSH_ERR_NOSPC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "target", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_FLUSH_ERR_TARGET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "protection", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_FLUSH_ERR_PROTECTION: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "unsupported", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_FLUSH_ERR_UNSUPPORTED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "other", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_FLUSH_ERR_OTHER: LazyCounter = LazyCounter::new(Counter::default);

// op = discard
#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "io", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_DISCARD_ERR_IO: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "timeout", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_DISCARD_ERR_TIMEOUT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "nospc", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_DISCARD_ERR_NOSPC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "target", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_DISCARD_ERR_TARGET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "protection", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_DISCARD_ERR_PROTECTION: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "unsupported", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_DISCARD_ERR_UNSUPPORTED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "other", unit = "operations", acq_group = "blockio_requests_errors" }
)]
pub static BLOCKIO_DISCARD_ERR_OTHER: LazyCounter = LazyCounter::new(Counter::default);

/*
 * blockio_requeues — block layer put a request back on the queue
 * because the driver couldn't complete it (SCSI EH, NVMe controller
 * reset, multipath path failover). Recovered events, distinct from
 * terminal errors.
 */

#[metric(
    name = "blockio_requeues",
    description = "Block IO requests put back on the queue for retry",
    metadata = { op = "read", unit = "operations", acq_group = "blockio_requests_requeues" }
)]
pub static BLOCKIO_READ_REQUEUE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_requeues",
    description = "Block IO requests put back on the queue for retry",
    metadata = { op = "write", unit = "operations", acq_group = "blockio_requests_requeues" }
)]
pub static BLOCKIO_WRITE_REQUEUE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_requeues",
    description = "Block IO requests put back on the queue for retry",
    metadata = { op = "flush", unit = "operations", acq_group = "blockio_requests_requeues" }
)]
pub static BLOCKIO_FLUSH_REQUEUE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_requeues",
    description = "Block IO requests put back on the queue for retry",
    metadata = { op = "discard", unit = "operations", acq_group = "blockio_requests_requeues" }
)]
pub static BLOCKIO_DISCARD_REQUEUE: LazyCounter = LazyCounter::new(Counter::default);

/*
 * latency
 */

// ONE group per phase, three groups total. The 4 op classes within a phase are
// LIKE ENTITIES (one family, distinguished by the `op` label) read as a single
// sweep; the three phases are different families measuring different parts of a
// request's life, and principle 18 keeps families in their own groups even when
// they are read back-to-back in one refresh.
// `BpfBuilder::histogram` batches every call naming a group into one
// `HistogramBatch`, stamped once per refresh — see `bpf/histogram.rs`.
pub static DEVICE_LATENCIES_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "blockio_latency_device_latencies");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static DEVICE_LATENCIES_ACQ_REG: &'static AcquisitionGroup = &DEVICE_LATENCIES_ACQ;

pub static QUEUE_LATENCIES_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "blockio_latency_queue_latencies");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static QUEUE_LATENCIES_ACQ_REG: &'static AcquisitionGroup = &QUEUE_LATENCIES_ACQ;

pub static TOTAL_LATENCIES_ACQ: AcquisitionGroup =
    AcquisitionGroup::new(GROUP_SAMPLER, "blockio_latency_total_latencies");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static TOTAL_LATENCIES_ACQ_REG: &'static AcquisitionGroup = &TOTAL_LATENCIES_ACQ;

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
