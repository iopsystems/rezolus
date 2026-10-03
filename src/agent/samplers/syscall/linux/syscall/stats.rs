use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CGROUPS, MAX_CPUS};
use linkme::distributed_slice;

// this is hard-coded still and must match the BPF histograms which are fixed to
// use 2^64-1 as the max value
static LATENCY_HISTOGRAM_MAX: u8 = 64;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `syscall/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
// The `syscall` sampler replaced `syscall_counts` and `syscall_latency`; the
// acquisition groups keep the names they had under those samplers.
//
/// Brackets the `counters` map's refresh (single writer: this sampler's
/// own BPF refresh path). The map holds one bank of the 17 op counters per
/// CPU (padded to 24, three cachelines), read with `cpu_counters`, so each `syscall{op}` is a per-CPU group.
pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("syscall"),
    "syscall_counts_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

// Reader-stamped (mmap-direct `PackedCounters`) group for the per-cgroup
// syscall-class breakdown — see `docs/principles.md` principle 18 and
// `crate::agent::timing::AcquisitionGroup::set_reader_stamped`. All 17
// `CGROUP_SYSCALL_*` counters below share this ONE group: they are all the
// `cgroup_syscall` metric family (distinguished by the `op` label, backed
// by 17 separate BPF maps) — the exact like-entities shape principle 18
// cites `syscall_latency`'s 16 op-class histograms for (see that sampler's
// stats.rs), applied here to counters instead of histograms.
pub static CGROUP_COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new_reader_stamped(
    crate::agent::samplers::bpf_sampler_name("syscall"),
    "syscall_counts_cgroup",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_COUNTERS_ACQ_REG: &'static AcquisitionGroup = &CGROUP_COUNTERS_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "syscall"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "syscall"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * per-CPU: one `CounterGroup` per op, one entry per CPU. The totals the
 * dashboards show are `sum(...)` over CPUs (docs/principles.md principle 9).
 */

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "other", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_OTHER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "read", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_READ: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "write", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_WRITE: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "poll", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_POLL: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "lock", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_LOCK: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "time", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_TIME: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "sleep", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_SLEEP: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "socket", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_SOCKET: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "yield", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_YIELD: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "filesystem", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_FILESYSTEM: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "memory", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_MEMORY: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "process", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_PROCESS: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "query", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_QUERY: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "ipc", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_IPC: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "timer", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_TIMER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "event", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_EVENT: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "sync", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_SYNC: CounterGroup = CounterGroup::new(MAX_CPUS);

/*
 * per-cgroup
 */

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "other", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_OTHER: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "read", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_READ: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "write", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_WRITE: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "poll", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_POLL: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "lock", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_LOCK: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "time", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_TIME: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "sleep", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_SLEEP: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "socket", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_SOCKET: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "yield", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_YIELD: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "filesystem", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_FILESYSTEM: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "memory", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_MEMORY: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "process", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_PROCESS: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "query", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_QUERY: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "ipc", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_IPC: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "timer", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_TIMER: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "event", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_EVENT: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "sync", acq_group = "syscall_counts_cgroup" }
)]
pub static CGROUP_SYSCALL_SYNC: CounterGroup = CounterGroup::new(MAX_CGROUPS);

/*
 * latency
 */

// ONE group for all 17 syscall-class latency histograms: they are LIKE
// ENTITIES — instances of a single "syscall latency" family distinguished
// by the `op` label — read back-to-back as one sweep, not 17 independent
// read sections. See the `# Granularity rule` on
// `crate::agent::samplers::ACQUISITION_GROUPS` and
// docs/journal/2026-08-17-window-sidecar-cost.md's addendum, which names
// syscall_latency explicitly as the collapse-to-one-group case. Mechanism:
// `BpfBuilder::histogram` batches every call naming this same group into
// one `HistogramBatch`, so the group is stamped once per refresh, not once
// per histogram — see `bpf/histogram.rs`.
pub static LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("syscall"),
    "syscall_latency_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static LATENCIES_ACQ_REG: &'static AcquisitionGroup = &LATENCIES_ACQ;

/*
 * system-wide
 */

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "other", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_OTHER_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "read", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_READ_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "write", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_WRITE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "poll", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_POLL_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "lock", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_LOCK_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "time", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_TIME_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "sleep", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_SLEEP_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "socket", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_SOCKET_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "yield", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_YIELD_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "filesystem", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_FILESYSTEM_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "memory", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_MEMORY_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "process", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_PROCESS_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "query", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_QUERY_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "ipc", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_IPC_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "timer", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_TIMER_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "event", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_EVENT_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "sync", acq_group = "syscall_latency_latencies" }
)]
pub static SYSCALL_SYNC_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);
