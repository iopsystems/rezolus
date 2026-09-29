use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CGROUPS, MAX_FILESYSTEMS};
use linkme::distributed_slice;

// this is hard-coded still and must match the BPF histograms which are fixed to
// use 2^64-1 as the max value
static LATENCY_HISTOGRAM_MAX: u8 = 64;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `xfs/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
// Three groups (principle 18): the two latency histograms are one family
// distinguished by `wait` and share a group; every per-filesystem counter
// lives in one `counters` map read in one sweep; the per-cgroup counters are
// mmap-attached and reader-stamped, as `ext4_ops`'s are.
pub static LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("xfs_log"),
    "xfs_log_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static LATENCIES_ACQ_REG: &'static AcquisitionGroup = &LATENCIES_ACQ;

pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("xfs_log"),
    "xfs_log_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

pub static CGROUP_ACQ: AcquisitionGroup = AcquisitionGroup::new_reader_stamped(
    crate::agent::samplers::bpf_sampler_name("xfs_log"),
    "xfs_log_cgroup",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_ACQ_REG: &'static AcquisitionGroup = &CGROUP_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "xfs_log"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "xfs_log"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * latency, host-wide: how long a thread was blocked on the log, per wait.
 */

#[metric(
    name = "xfs_log_wait_latency",
    description = "Distribution of the time a transaction slept waiting for XFS log space (one xfs_log_grant_sleep to its xfs_log_grant_wake), in nanoseconds",
    metadata = { unit = "nanoseconds", wait = "space", acq_group = "xfs_log_latencies" }
)]
pub static XFS_LOG_WAIT_LATENCY_SPACE: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "xfs_log_wait_latency",
    description = "Distribution of the time an XFS log force took (xfs_log_force or xfs_log_force_seq, entry to return): the log write an fsync waits for, in nanoseconds",
    metadata = { unit = "nanoseconds", wait = "force", acq_group = "xfs_log_latencies" }
)]
pub static XFS_LOG_WAIT_LATENCY_FORCE: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

/*
 * per filesystem: one `CounterGroup` per metric, one entry per filesystem slot
 * (`bpf/filesystems.rs`): slot 0 is `mount="other"`, the rest carry the
 * mount's labels. Order here is documentation; the `counters` vec in mod.rs
 * must match the C_* indices in mod.bpf.c.
 */

#[metric(
    name = "xfs_log_waits",
    description = "Times a transaction slept waiting for XFS log space; equals xfs_log_space_sleeps per mount",
    metadata = { unit = "operations", wait = "space", acq_group = "xfs_log_counters" }
)]
pub static XFS_LOG_WAITS_SPACE: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_log_waits",
    description = "XFS log forces completed (xfs_log_force and xfs_log_force_seq); equals xfs_log_forces per mount",
    metadata = { unit = "operations", wait = "force", acq_group = "xfs_log_counters" }
)]
pub static XFS_LOG_WAITS_FORCE: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_log_waits",
    description = "Times a committing transaction found the XFS CIL over its hard limit and waited for a push (xfs_log_cil_wait); a count only, the wake is not traced",
    metadata = { unit = "operations", wait = "cil", acq_group = "xfs_log_counters" }
)]
pub static XFS_LOG_WAITS_CIL: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_log_wait_time",
    description = "Nanoseconds threads slept waiting for XFS log space, summed; divided by xfs_log_waits{wait=\"space\"} it is the mean wait per mount",
    metadata = { unit = "nanoseconds", wait = "space", acq_group = "xfs_log_counters" }
)]
pub static XFS_LOG_WAIT_TIME_SPACE: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

#[metric(
    name = "xfs_log_wait_time",
    description = "Nanoseconds threads spent inside XFS log forces, summed; divided by xfs_log_waits{wait=\"force\"} it is the mean force latency per mount",
    metadata = { unit = "nanoseconds", wait = "force", acq_group = "xfs_log_counters" }
)]
pub static XFS_LOG_WAIT_TIME_FORCE: CounterGroup = CounterGroup::new(MAX_FILESYSTEMS);

/*
 * per cgroup: waits and time blocked, by the cgroup of the waiting thread
 */

#[metric(
    name = "cgroup_xfs_log_waits",
    description = "Times a cgroup's threads slept waiting for XFS log space",
    metadata = { unit = "operations", wait = "space", acq_group = "xfs_log_cgroup" }
)]
pub static CGROUP_XFS_LOG_WAITS_SPACE: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_xfs_log_waits",
    description = "XFS log forces a cgroup's threads performed",
    metadata = { unit = "operations", wait = "force", acq_group = "xfs_log_cgroup" }
)]
pub static CGROUP_XFS_LOG_WAITS_FORCE: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_xfs_log_wait_time",
    description = "Nanoseconds a cgroup's threads slept waiting for XFS log space, summed: time request threads were held by a full log",
    metadata = { unit = "nanoseconds", wait = "space", acq_group = "xfs_log_cgroup" }
)]
pub static CGROUP_XFS_LOG_WAIT_TIME_SPACE: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_xfs_log_wait_time",
    description = "Nanoseconds a cgroup's threads spent inside XFS log forces, summed: time request threads were held by durability",
    metadata = { unit = "nanoseconds", wait = "force", acq_group = "xfs_log_cgroup" }
)]
pub static CGROUP_XFS_LOG_WAIT_TIME_FORCE: CounterGroup = CounterGroup::new(MAX_CGROUPS);
