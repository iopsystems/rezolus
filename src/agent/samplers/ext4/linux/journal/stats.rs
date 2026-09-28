use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// this is hard-coded still and must match the BPF histograms which are fixed to
// use 2^64-1 as the max value
static LATENCY_HISTOGRAM_MAX: u8 = 64;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `ext4/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
// Groups follow principle 18's like-entities rule as `blockio_latency` applies
// it: the six commit-phase histograms are one family distinguished by the
// `phase` label and share a group; the checkpoint and lock-stall histograms
// are each their own family; every counter lives in one `counters` map read
// in one sweep, as `blockio_requests` reads ops and bytes from one map.
pub static COMMIT_LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("ext4_journal"),
    "ext4_journal_commit_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COMMIT_LATENCIES_ACQ_REG: &'static AcquisitionGroup = &COMMIT_LATENCIES_ACQ;

pub static CHECKPOINT_LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("ext4_journal"),
    "ext4_journal_checkpoint_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CHECKPOINT_LATENCIES_ACQ_REG: &'static AcquisitionGroup = &CHECKPOINT_LATENCIES_ACQ;

pub static STALL_LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("ext4_journal"),
    "ext4_journal_stall_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static STALL_LATENCIES_ACQ_REG: &'static AcquisitionGroup = &STALL_LATENCIES_ACQ;

pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("ext4_journal"),
    "ext4_journal_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "ext4_journal"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "ext4_journal"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * journal commit phases
 *
 * jbd2 reports each phase in jiffies; the BPF program converts to nanoseconds
 * with the tick length userspace measured, so every value below is a whole
 * multiple of one jiffy (1–10 ms depending on CONFIG_HZ).
 */

#[metric(
    name = "ext4_journal_commit_latency",
    description = "Distribution of the time, per journal commit, that a handle had to wait to join the transaction (the longest such wait in the transaction). In nanoseconds, at one-jiffy resolution",
    metadata = { unit = "nanoseconds", phase = "wait", acq_group = "ext4_journal_commit_latencies" }
)]
pub static EXT4_JOURNAL_COMMIT_WAIT: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "ext4_journal_commit_latency",
    description = "Distribution of the time, per journal commit, between the commit being requested and the commit thread starting it. In nanoseconds, at one-jiffy resolution",
    metadata = { unit = "nanoseconds", phase = "request_delay", acq_group = "ext4_journal_commit_latencies" }
)]
pub static EXT4_JOURNAL_COMMIT_REQUEST_DELAY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "ext4_journal_commit_latency",
    description = "Distribution of the time, per journal commit, that the transaction was open and accumulating handles before the commit began. Bounded above by the journal's commit interval (5 s by default) and cut short by every fsync. In nanoseconds, at one-jiffy resolution",
    metadata = { unit = "nanoseconds", phase = "running", acq_group = "ext4_journal_commit_latencies" }
)]
pub static EXT4_JOURNAL_COMMIT_RUNNING: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "ext4_journal_commit_latency",
    description = "Distribution of the time, per journal commit, that the transaction spent locked, waiting for its outstanding handles to finish before its buffers could be written. In nanoseconds, at one-jiffy resolution",
    metadata = { unit = "nanoseconds", phase = "locked", acq_group = "ext4_journal_commit_latencies" }
)]
pub static EXT4_JOURNAL_COMMIT_LOCKED: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "ext4_journal_commit_latency",
    description = "Distribution of the time, per journal commit, spent flushing the transaction's data blocks to disk (ordered mode). This is where fsync waits on the device. In nanoseconds, at one-jiffy resolution",
    metadata = { unit = "nanoseconds", phase = "flushing", acq_group = "ext4_journal_commit_latencies" }
)]
pub static EXT4_JOURNAL_COMMIT_FLUSHING: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "ext4_journal_commit_latency",
    description = "Distribution of the time, per journal commit, spent writing the transaction's metadata blocks and commit record to the journal. In nanoseconds, at one-jiffy resolution",
    metadata = { unit = "nanoseconds", phase = "logging", acq_group = "ext4_journal_commit_latencies" }
)]
pub static EXT4_JOURNAL_COMMIT_LOGGING: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

/*
 * checkpoint and lock stalls
 */

#[metric(
    name = "ext4_journal_checkpoint_latency",
    description = "Distribution of the time each journal checkpoint took to write a committed transaction's buffers to their final location and free the journal space. In nanoseconds, at one-jiffy resolution",
    metadata = { unit = "nanoseconds", acq_group = "ext4_journal_checkpoint_latencies" }
)]
pub static EXT4_JOURNAL_CHECKPOINT_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "ext4_journal_lock_buffer_stall_latency",
    description = "Distribution of the time the journal spent stalled on a locked buffer, as reported by jbd2 in whole milliseconds. In nanoseconds",
    metadata = { unit = "nanoseconds", acq_group = "ext4_journal_stall_latencies" }
)]
pub static EXT4_JOURNAL_LOCK_BUFFER_STALL_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

/*
 * counters: one `counters` map, one bank per CPU, summed in userspace. Order
 * here is documentation; the order that matters is the `counters` vec in
 * mod.rs, which must match the C_* indices in mod.bpf.c.
 */

#[metric(
    name = "ext4_journal_commits",
    description = "The number of journal (jbd2) transaction commits",
    metadata = { unit = "commits", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_JOURNAL_COMMITS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_journal_commit_handles",
    description = "The number of handles (metadata operations) committed to the journal, summed over commits",
    metadata = { unit = "handles", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_JOURNAL_COMMIT_HANDLES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_journal_commit_blocks",
    description = "The number of metadata blocks dirtied by committed transactions, summed over commits",
    metadata = { unit = "blocks", kind = "dirtied", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_JOURNAL_COMMIT_BLOCKS_DIRTIED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_journal_commit_blocks",
    description = "The number of blocks written to the journal by committed transactions (metadata plus descriptor and commit blocks), summed over commits",
    metadata = { unit = "blocks", kind = "logged", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_JOURNAL_COMMIT_BLOCKS_LOGGED: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_journal_checkpoints",
    description = "The number of journal checkpoints",
    metadata = { unit = "checkpoints", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_JOURNAL_CHECKPOINTS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_journal_checkpoint_buffers",
    description = "The number of buffers journal checkpoints wrote to their final location",
    metadata = { unit = "buffers", outcome = "written", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_JOURNAL_CHECKPOINT_BUFFERS_WRITTEN: LazyCounter =
    LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_journal_checkpoint_buffers",
    description = "The number of buffers journal checkpoints found already written and dropped without I/O",
    metadata = { unit = "buffers", outcome = "dropped", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_JOURNAL_CHECKPOINT_BUFFERS_DROPPED: LazyCounter =
    LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_journal_checkpoint_forced_to_close",
    description = "The number of transactions a checkpoint had to force closed to free journal space. A rising rate means the journal is too small for the write rate",
    metadata = { unit = "transactions", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_JOURNAL_CHECKPOINT_FORCED_TO_CLOSE: LazyCounter =
    LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_sync_file",
    description = "The number of fsync calls that reached ext4",
    metadata = { unit = "operations", op = "fsync", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_SYNC_FILE_FSYNC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_sync_file",
    description = "The number of fdatasync calls that reached ext4",
    metadata = { unit = "operations", op = "fdatasync", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_SYNC_FILE_FDATASYNC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_sync_file_errors",
    description = "The number of fsync and fdatasync calls ext4 completed with an error",
    metadata = { unit = "operations", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_SYNC_FILE_ERRORS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_errors",
    description = "The number of errors ext4 reported (ext4_error and its variants), counted as they occur and before any errors=remount-ro takes effect. Absent on kernels without the ext4_error tracepoint",
    metadata = { unit = "errors", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_ERRORS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_shutdowns",
    description = "The number of forced ext4 filesystem shutdowns",
    metadata = { unit = "shutdowns", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_SHUTDOWNS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "ext4_journal_lock_buffer_stalls",
    description = "The number of times the journal stalled on a locked buffer",
    metadata = { unit = "stalls", acq_group = "ext4_journal_counters" }
)]
pub static EXT4_JOURNAL_LOCK_BUFFER_STALLS: LazyCounter = LazyCounter::new(Counter::default);
