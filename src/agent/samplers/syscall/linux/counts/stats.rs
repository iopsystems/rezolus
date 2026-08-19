use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::MAX_CGROUPS;
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `syscall/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
/// Brackets the `counters` map's refresh (single writer: this sampler's
/// own BPF refresh path).
pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("syscall_counts"),
    "syscall_counts_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "syscall_counts"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "syscall_counts"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "other", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_OTHER: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "read", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_READ: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "write", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_WRITE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "poll", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_POLL: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "lock", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_LOCK: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "time", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_TIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "sleep", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_SLEEP: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "socket", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_SOCKET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "yield", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_YIELD: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "filesystem", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_FILESYSTEM: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "memory", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_MEMORY: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "process", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_PROCESS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "query", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_QUERY: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "ipc", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_IPC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "timer", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_TIMER: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "event", acq_group = "syscall_counts_counters" }
)]
pub static SYSCALL_EVENT: LazyCounter = LazyCounter::new(Counter::default);

/*
 * per-cgroup
 */

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "other" }
)]
pub static CGROUP_SYSCALL_OTHER: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "read" }
)]
pub static CGROUP_SYSCALL_READ: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "write" }
)]
pub static CGROUP_SYSCALL_WRITE: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "poll" }
)]
pub static CGROUP_SYSCALL_POLL: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "lock" }
)]
pub static CGROUP_SYSCALL_LOCK: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "time" }
)]
pub static CGROUP_SYSCALL_TIME: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "sleep" }
)]
pub static CGROUP_SYSCALL_SLEEP: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "socket" }
)]
pub static CGROUP_SYSCALL_SOCKET: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "yield" }
)]
pub static CGROUP_SYSCALL_YIELD: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "filesystem" }
)]
pub static CGROUP_SYSCALL_FILESYSTEM: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "memory" }
)]
pub static CGROUP_SYSCALL_MEMORY: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "process" }
)]
pub static CGROUP_SYSCALL_PROCESS: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "query" }
)]
pub static CGROUP_SYSCALL_QUERY: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "ipc" }
)]
pub static CGROUP_SYSCALL_IPC: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "timer" }
)]
pub static CGROUP_SYSCALL_TIMER: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "event" }
)]
pub static CGROUP_SYSCALL_EVENT: CounterGroup = CounterGroup::new(MAX_CGROUPS);
