use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "blockio_requests"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "blockio_requests"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "blockio_size",
    description = "Distribution of blockio operation sizes in bytes",
    metadata = { op = "read", unit = "bytes" }
)]
pub static BLOCKIO_READ_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_size",
    description = "Distribution of blockio operation sizes in bytes",
    metadata = { op = "write", unit = "bytes" }
)]
pub static BLOCKIO_WRITE_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_size",
    description = "Distribution of blockio operation sizes in bytes",
    metadata = { op = "flush", unit = "bytes" }
)]
pub static BLOCKIO_FLUSH_SIZE: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_size",
    description = "Distribution of blockio operation sizes in bytes",
    metadata = { op = "discard", unit = "bytes" }
)]
pub static BLOCKIO_DISCARD_SIZE: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "blockio_operations",
    description = "The number of completed operations for block devices",
    metadata = { op = "read", unit = "operations" }
)]
pub static BLOCKIO_READ_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_operations",
    description = "The number of completed operations for block devices",
    metadata = { op = "write", unit = "operations" }
)]
pub static BLOCKIO_WRITE_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_operations",
    description = "The number of completed operations for block devices",
    metadata = { op = "discard", unit = "operations" }
)]
pub static BLOCKIO_DISCARD_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_operations",
    description = "The number of completed operations for block devices",
    metadata = { op = "flush", unit = "operations" }
)]
pub static BLOCKIO_FLUSH_OPS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_bytes",
    description = "The number of bytes transferred for block device operations",
    metadata = { op = "read", unit = "bytes" }
)]
pub static BLOCKIO_READ_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_bytes",
    description = "The number of bytes transferred for block device operations",
    metadata = { op = "write", unit = "bytes" }
)]
pub static BLOCKIO_WRITE_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_bytes",
    description = "The number of bytes transferred for block device operations",
    metadata = { op = "discard", unit = "bytes" }
)]
pub static BLOCKIO_DISCARD_BYTES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_bytes",
    description = "The number of bytes transferred for block device operations",
    metadata = { op = "flush", unit = "bytes" }
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
    metadata = { op = "read", error = "io", unit = "operations" }
)]
pub static BLOCKIO_READ_ERR_IO: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "timeout", unit = "operations" }
)]
pub static BLOCKIO_READ_ERR_TIMEOUT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "nospc", unit = "operations" }
)]
pub static BLOCKIO_READ_ERR_NOSPC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "target", unit = "operations" }
)]
pub static BLOCKIO_READ_ERR_TARGET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "protection", unit = "operations" }
)]
pub static BLOCKIO_READ_ERR_PROTECTION: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "unsupported", unit = "operations" }
)]
pub static BLOCKIO_READ_ERR_UNSUPPORTED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "read", error = "other", unit = "operations" }
)]
pub static BLOCKIO_READ_ERR_OTHER: LazyCounter = LazyCounter::new(Counter::default);

// op = write
#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "io", unit = "operations" }
)]
pub static BLOCKIO_WRITE_ERR_IO: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "timeout", unit = "operations" }
)]
pub static BLOCKIO_WRITE_ERR_TIMEOUT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "nospc", unit = "operations" }
)]
pub static BLOCKIO_WRITE_ERR_NOSPC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "target", unit = "operations" }
)]
pub static BLOCKIO_WRITE_ERR_TARGET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "protection", unit = "operations" }
)]
pub static BLOCKIO_WRITE_ERR_PROTECTION: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "unsupported", unit = "operations" }
)]
pub static BLOCKIO_WRITE_ERR_UNSUPPORTED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "write", error = "other", unit = "operations" }
)]
pub static BLOCKIO_WRITE_ERR_OTHER: LazyCounter = LazyCounter::new(Counter::default);

// op = flush
#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "io", unit = "operations" }
)]
pub static BLOCKIO_FLUSH_ERR_IO: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "timeout", unit = "operations" }
)]
pub static BLOCKIO_FLUSH_ERR_TIMEOUT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "nospc", unit = "operations" }
)]
pub static BLOCKIO_FLUSH_ERR_NOSPC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "target", unit = "operations" }
)]
pub static BLOCKIO_FLUSH_ERR_TARGET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "protection", unit = "operations" }
)]
pub static BLOCKIO_FLUSH_ERR_PROTECTION: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "unsupported", unit = "operations" }
)]
pub static BLOCKIO_FLUSH_ERR_UNSUPPORTED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "flush", error = "other", unit = "operations" }
)]
pub static BLOCKIO_FLUSH_ERR_OTHER: LazyCounter = LazyCounter::new(Counter::default);

// op = discard
#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "io", unit = "operations" }
)]
pub static BLOCKIO_DISCARD_ERR_IO: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "timeout", unit = "operations" }
)]
pub static BLOCKIO_DISCARD_ERR_TIMEOUT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "nospc", unit = "operations" }
)]
pub static BLOCKIO_DISCARD_ERR_NOSPC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "target", unit = "operations" }
)]
pub static BLOCKIO_DISCARD_ERR_TARGET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "protection", unit = "operations" }
)]
pub static BLOCKIO_DISCARD_ERR_PROTECTION: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "unsupported", unit = "operations" }
)]
pub static BLOCKIO_DISCARD_ERR_UNSUPPORTED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_errors",
    description = "Terminal block IO failures",
    metadata = { op = "discard", error = "other", unit = "operations" }
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
    metadata = { op = "read", unit = "operations" }
)]
pub static BLOCKIO_READ_REQUEUE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_requeues",
    description = "Block IO requests put back on the queue for retry",
    metadata = { op = "write", unit = "operations" }
)]
pub static BLOCKIO_WRITE_REQUEUE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_requeues",
    description = "Block IO requests put back on the queue for retry",
    metadata = { op = "flush", unit = "operations" }
)]
pub static BLOCKIO_FLUSH_REQUEUE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "blockio_requeues",
    description = "Block IO requests put back on the queue for retry",
    metadata = { op = "discard", unit = "operations" }
)]
pub static BLOCKIO_DISCARD_REQUEUE: LazyCounter = LazyCounter::new(Counter::default);
