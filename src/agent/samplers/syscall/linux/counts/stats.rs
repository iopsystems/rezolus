use metriken::*;

use crate::agent::*;

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
    metadata = { unit = "syscalls", op = "other" }
)]
pub static SYSCALL_OTHER: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "read" }
)]
pub static SYSCALL_READ: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "write" }
)]
pub static SYSCALL_WRITE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "poll" }
)]
pub static SYSCALL_POLL: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "lock" }
)]
pub static SYSCALL_LOCK: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "time" }
)]
pub static SYSCALL_TIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "sleep" }
)]
pub static SYSCALL_SLEEP: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "socket" }
)]
pub static SYSCALL_SOCKET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "yield" }
)]
pub static SYSCALL_YIELD: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "filesystem" }
)]
pub static SYSCALL_FILESYSTEM: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "memory" }
)]
pub static SYSCALL_MEMORY: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "process" }
)]
pub static SYSCALL_PROCESS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "query" }
)]
pub static SYSCALL_QUERY: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "ipc" }
)]
pub static SYSCALL_IPC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "timer" }
)]
pub static SYSCALL_TIMER: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of syscalls",
    metadata = { unit = "syscalls", op = "event" }
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
