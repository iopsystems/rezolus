use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CGROUPS, MAX_FILESYSTEMS};
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
// Three groups (principle 18): the four latency histograms are one family
// distinguished by `op` and share a group; every per-filesystem counter lives
// in one `counters` map read in one sweep; the per-cgroup counters are
// mmap-attached and reader-stamped, as `cgroup_syscall`'s are.
pub static LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("ext4_ops"),
    "ext4_ops_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static LATENCIES_ACQ_REG: &'static AcquisitionGroup = &LATENCIES_ACQ;

pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("ext4_ops"),
    "ext4_ops_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

pub static CGROUP_ACQ: AcquisitionGroup = AcquisitionGroup::new_reader_stamped(
    crate::agent::samplers::bpf_sampler_name("ext4_ops"),
    "ext4_ops_cgroup",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_ACQ_REG: &'static AcquisitionGroup = &CGROUP_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "ext4_ops"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "ext4_ops"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * latency, host-wide: how long each call held the calling thread, from the
 * ext4 entry point to its return.
 */

#[metric(
    name = "ext4_op_latency",
    description = "Distribution of the time an fsync or fdatasync held the calling thread inside ext4, in nanoseconds",
    metadata = { unit = "nanoseconds", op = "fsync", acq_group = "ext4_ops_latencies" }
)]
pub static EXT4_OP_LATENCY_FSYNC: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "ext4_op_latency",
    description = "Distribution of the time an unlink held the calling thread inside ext4, in nanoseconds",
    metadata = { unit = "nanoseconds", op = "unlink", acq_group = "ext4_ops_latencies" }
)]
pub static EXT4_OP_LATENCY_UNLINK: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "ext4_op_latency",
    description = "Distribution of the time a write held the calling thread inside ext4 (ext4_file_write_iter, buffered or direct), in nanoseconds",
    metadata = { unit = "nanoseconds", op = "write", acq_group = "ext4_ops_latencies" }
)]
pub static EXT4_OP_LATENCY_WRITE: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "ext4_op_latency",
    description = "Distribution of the time a rename held the calling thread inside ext4 (ext4_rename2), in nanoseconds",
    metadata = { unit = "nanoseconds", op = "rename", acq_group = "ext4_ops_latencies" }
)]
pub static EXT4_OP_LATENCY_RENAME: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

/*
 * per filesystem: one `CounterGroup` per metric, one entry per filesystem slot
 * (`bpf/filesystems.rs`): slot 0 is `mount="other"`, the rest carry the
 * mount's labels. Order here is documentation; the `counters` vec in mod.rs
 * must match the C_* indices in mod.bpf.c.
 */

#[metric(
    name = "ext4_ops",
    description = "fsync and fdatasync calls that completed in ext4",
    metadata = { unit = "operations", op = "fsync", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OPS_FSYNC: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_ops",
    description = "unlink calls that completed in ext4",
    metadata = { unit = "operations", op = "unlink", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OPS_UNLINK: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_ops",
    description = "write calls that completed in ext4 (ext4_file_write_iter)",
    metadata = { unit = "operations", op = "write", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OPS_WRITE: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_ops",
    description = "rename calls that completed in ext4 (ext4_rename2)",
    metadata = { unit = "operations", op = "rename", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OPS_RENAME: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_op_time",
    description = "Nanoseconds calling threads spent inside ext4's fsync, summed; divided by ext4_ops it is the mean fsync latency per filesystem",
    metadata = { unit = "nanoseconds", op = "fsync", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OP_TIME_FSYNC: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_op_time",
    description = "Nanoseconds calling threads spent inside ext4's unlink, summed",
    metadata = { unit = "nanoseconds", op = "unlink", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OP_TIME_UNLINK: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_op_time",
    description = "Nanoseconds calling threads spent inside ext4's write path, summed",
    metadata = { unit = "nanoseconds", op = "write", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OP_TIME_WRITE: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_op_time",
    description = "Nanoseconds calling threads spent inside ext4's rename, summed",
    metadata = { unit = "nanoseconds", op = "rename", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OP_TIME_RENAME: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_op_errors",
    description = "fsync and fdatasync calls ext4 completed with an error",
    metadata = { unit = "operations", op = "fsync", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OP_ERRORS_FSYNC: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_op_errors",
    description = "unlink calls ext4 completed with an error",
    metadata = { unit = "operations", op = "unlink", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OP_ERRORS_UNLINK: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_op_errors",
    description = "write calls ext4 completed with an error",
    metadata = { unit = "operations", op = "write", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OP_ERRORS_WRITE: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_op_errors",
    description = "rename calls ext4 completed with an error",
    metadata = { unit = "operations", op = "rename", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_OP_ERRORS_RENAME: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "ext4_write_bytes",
    description = "Bytes applications wrote into ext4 (the return values of ext4_file_write_iter), summed: the first term of write amplification, against pages written back, journal blocks logged and device bytes",
    metadata = { unit = "bytes", acq_group = "ext4_ops_counters" }
)]
pub static EXT4_WRITE_BYTES: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * per cgroup: calls and time held, by the cgroup of the calling thread
 */

#[metric(
    name = "cgroup_ext4_ops",
    description = "fsync and fdatasync calls that completed in ext4, by the calling thread's cgroup",
    metadata = { unit = "operations", op = "fsync", acq_group = "ext4_ops_cgroup" }
)]
pub static CGROUP_EXT4_OPS_FSYNC: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_ext4_ops",
    description = "unlink calls that completed in ext4, by the calling thread's cgroup",
    metadata = { unit = "operations", op = "unlink", acq_group = "ext4_ops_cgroup" }
)]
pub static CGROUP_EXT4_OPS_UNLINK: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_ext4_ops",
    description = "write calls that completed in ext4, by the calling thread's cgroup",
    metadata = { unit = "operations", op = "write", acq_group = "ext4_ops_cgroup" }
)]
pub static CGROUP_EXT4_OPS_WRITE: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_ext4_ops",
    description = "rename calls that completed in ext4, by the calling thread's cgroup",
    metadata = { unit = "operations", op = "rename", acq_group = "ext4_ops_cgroup" }
)]
pub static CGROUP_EXT4_OPS_RENAME: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_ext4_op_time",
    description = "Nanoseconds a cgroup's threads spent inside ext4's fsync, summed: time request threads were held by durability",
    metadata = { unit = "nanoseconds", op = "fsync", acq_group = "ext4_ops_cgroup" }
)]
pub static CGROUP_EXT4_OP_TIME_FSYNC: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_ext4_op_time",
    description = "Nanoseconds a cgroup's threads spent inside ext4's unlink, summed",
    metadata = { unit = "nanoseconds", op = "unlink", acq_group = "ext4_ops_cgroup" }
)]
pub static CGROUP_EXT4_OP_TIME_UNLINK: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_ext4_op_time",
    description = "Nanoseconds a cgroup's threads spent inside ext4's write path, summed",
    metadata = { unit = "nanoseconds", op = "write", acq_group = "ext4_ops_cgroup" }
)]
pub static CGROUP_EXT4_OP_TIME_WRITE: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_ext4_op_time",
    description = "Nanoseconds a cgroup's threads spent inside ext4's rename, summed",
    metadata = { unit = "nanoseconds", op = "rename", acq_group = "ext4_ops_cgroup" }
)]
pub static CGROUP_EXT4_OP_TIME_RENAME: CounterGroup = CounterGroup::new(MAX_CGROUPS);
