use metriken::*;

use crate::common::*;

#[metric(
    name = "syscall_total",
    description = "The total number of syscalls",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_TOTAL: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall_read",
    description = "The number of read related syscalls (read, recvfrom, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_READ: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall_write",
    description = "The number of write related syscalls (write, sendto, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_WRITE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall_poll",
    description = "The number of poll related syscalls (poll, select, epoll, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_POLL: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall_lock",
    description = "The number of lock related syscalls (futex, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_LOCK: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall_time",
    description = "The number of time related syscalls (clock_gettime, clock_settime, clock_getres, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_TIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall_sleep",
    description = "The number of sleep related syscalls (nanosleep, clock_nanosleep, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_SLEEP: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall_socket",
    description = "The number of socket related syscalls (accept, connect, bind, setsockopt, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_SOCKET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall_yield",
    description = "The number of socket related syscalls (sched_yield, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_YIELD: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cgroup_syscall_total",
    description = "The total number of syscalls on a per-cgroup basis",
    metadata = { unit = "syscalls" }
)]
pub static CGROUP_SYSCALL_TOTAL: CounterGroup = CounterGroup::new(MAX_CGROUPS);
