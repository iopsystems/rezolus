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

#[metric(
    name = "syscall",
    description = "The number of filesystem operations (open, close, stat, chmod, mkdir, ...)",
    metadata = { unit = "syscalls", op = "filesystem" }
)]
pub static SYSCALL_FILESYSTEM: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of memory management syscalls (mmap, munmap, mprotect, brk, ...)",
    metadata = { unit = "syscalls", op = "memory" }
)]
pub static SYSCALL_MEMORY: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of process control syscalls (fork, clone, exec, wait, kill, ...)",
    metadata = { unit = "syscalls", op = "process" }
)]
pub static SYSCALL_PROCESS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of resource query syscalls (getrusage, getrlimit, getpid, ...)",
    metadata = { unit = "syscalls", op = "query" }
)]
pub static SYSCALL_QUERY: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of IPC syscalls (pipe, msgget, semop, shmat, mq_open, ...)",
    metadata = { unit = "syscalls", op = "ipc" }
)]
pub static SYSCALL_IPC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of timer syscalls (alarm, setitimer, timer_create, ...)",
    metadata = { unit = "syscalls", op = "timer" }
)]
pub static SYSCALL_TIMER: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall",
    description = "The number of event notification syscalls (eventfd, inotify, io_uring, ...)",
    metadata = { unit = "syscalls", op = "event" }
)]
pub static SYSCALL_EVENT: LazyCounter = LazyCounter::new(Counter::default);

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

#[metric(
    name = "cgroup_syscall",
    description = "The number of filesystem operations on a per-cgroup basis (open, stat, mkdir, ...)",
    metadata = { unit = "syscalls", op = "filesystem" }
)]
pub static CGROUP_SYSCALL_FILESYSTEM: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of memory management syscalls on a per-cgroup basis (mmap, brk, ...)",
    metadata = { unit = "syscalls", op = "memory" }
)]
pub static CGROUP_SYSCALL_MEMORY: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of process control syscalls on a per-cgroup basis (fork, exec, ...)",
    metadata = { unit = "syscalls", op = "process" }
)]
pub static CGROUP_SYSCALL_PROCESS: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of resource query syscalls on a per-cgroup basis (getrusage, ...)",
    metadata = { unit = "syscalls", op = "query" }
)]
pub static CGROUP_SYSCALL_QUERY: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of IPC syscalls on a per-cgroup basis (pipe, semop, shmat, ...)",
    metadata = { unit = "syscalls", op = "ipc" }
)]
pub static CGROUP_SYSCALL_IPC: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of timer syscalls on a per-cgroup basis (setitimer, timer_create, ...)",
    metadata = { unit = "syscalls", op = "timer" }
)]
pub static CGROUP_SYSCALL_TIMER: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_syscall",
    description = "The number of event notification syscalls on a per-cgroup basis (inotify, io_uring, ...)",
    metadata = { unit = "syscalls", op = "event" }
)]
pub static CGROUP_SYSCALL_EVENT: CounterGroup = CounterGroup::new(MAX_CGROUPS);
