use metriken::*;

use crate::agent::*;

/*
 * system-wide
 */

#[metric(
    name = "syscall",
    description = "The total number of syscalls",
    metadata = { unit = "syscalls", op = "other" }
)]
pub static SYSCALL_OTHER: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of read related syscalls (read, recvfrom, ...)",
    metadata = { unit = "syscalls", op = "read" }
)]
pub static SYSCALL_READ: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of write related syscalls (write, sendto, ...)",
    metadata = { unit = "syscalls", op = "write" }
)]
pub static SYSCALL_WRITE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of poll related syscalls (poll, select, epoll, ...)",
    metadata = { unit = "syscalls", op = "poll" }
)]
pub static SYSCALL_POLL: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of lock related syscalls (futex, ...)",
    metadata = { unit = "syscalls", op = "lock" }
)]
pub static SYSCALL_LOCK: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of time related syscalls (clock_gettime, clock_settime, clock_getres, ...)",
    metadata = { unit = "syscalls", op = "time" }
)]
pub static SYSCALL_TIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of sleep related syscalls (nanosleep, clock_nanosleep, ...)",
    metadata = { unit = "syscalls", op = "sleep" }
)]
pub static SYSCALL_SLEEP: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of socket related syscalls (accept, connect, bind, setsockopt, ...)",
    metadata = { unit = "syscalls", op = "socket" }
)]
pub static SYSCALL_SOCKET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of socket related syscalls (sched_yield, ...)",
    metadata = { unit = "syscalls", op = "yield" }
)]
pub static SYSCALL_YIELD: LazyCounter = LazyCounter::new(Counter::default);

/*
 * per-cgroup
 */

#[metric(
    name = "cgroup_syscall",
    description = "The total number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls", op = "other" }
)]
pub static CGROUP_SYSCALL_OTHER: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of read related syscalls on a per-cgroup basis (read, recvfrom, ...)",
    metadata = { unit = "syscalls", op = "read" }
)]
pub static CGROUP_SYSCALL_READ: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of write related syscalls on a per-cgroup basis (write, sendto, ...)",
    metadata = { unit = "syscalls", op = "write" }
)]
pub static CGROUP_SYSCALL_WRITE: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of poll related syscalls on a per-cgroup basis (poll, select, epoll, ...)",
    metadata = { unit = "syscalls", op = "poll" }
)]
pub static CGROUP_SYSCALL_POLL: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of lock related syscalls on a per-cgroup basis (futex, ...)",
    metadata = { unit = "syscalls", op = "lock" }
)]
pub static CGROUP_SYSCALL_LOCK: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of time related syscalls on a per-cgroup basis (clock_gettime, clock_settime, clock_getres, ...)",
    metadata = { unit = "syscalls", op = "time" }
)]
pub static CGROUP_SYSCALL_TIME: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of sleep related syscalls on a per-cgroup basis (nanosleep, clock_nanosleep, ...)",
    metadata = { unit = "syscalls", op = "sleep" }
)]
pub static CGROUP_SYSCALL_SLEEP: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of socket related syscalls on a per-cgroup basis (accept, connect, bind, setsockopt, ...)",
    metadata = { unit = "syscalls", op = "socket" }
)]
pub static CGROUP_SYSCALL_SOCKET: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of socket related syscalls on a per-cgroup basis (sched_yield, ...)",
    metadata = { unit = "syscalls", op = "yield" }
)]
pub static CGROUP_SYSCALL_YIELD: CounterGroup = CounterGroup::new(MAX_CGROUPS);
