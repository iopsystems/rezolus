use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::{
    metric, AtomicHistogram, Counter, Format, LazyCounter, MetricEntry, RwLockHistogram,
};

#[metric(
    name = "syscall/total",
    description = "The total number of syscalls",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_TOTAL: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall/total",
    description = "Distribution of the total rate of syscalls from sample to sample",
    metadata = { unit = "syscalls/second" }
)]
pub static SYSCALL_TOTAL_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/total/latency",
    description = "Distribution of the latency for all syscalls",
    metadata = { unit = "nanoseconds" }
)]
pub static SYSCALL_TOTAL_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/read",
    description = "The number of read related syscalls (read, recvfrom, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_READ: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall/read",
    description = "Distribution of the rate of read related syscalls from sample to sample",
    metadata = { unit = "syscalls/second" }
)]
pub static SYSCALL_READ_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/read/latency",
    description = "Distribution of the latency for read related syscalls",
    metadata = { unit = "nanoseconds" }
)]
pub static SYSCALL_READ_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/write",
    description = "The number of write related syscalls (write, sendto, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_WRITE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall/write",
    description = "Distribution of the rate of write related syscalls from sample to sample",
    metadata = { unit = "sycalls/second" }
)]
pub static SYSCALL_WRITE_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/write/latency",
    description = "Distribution of the latency for write related syscalls",
    metadata = { unit = "nanoseconds" }
)]
pub static SYSCALL_WRITE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/poll",
    description = "The number of poll related syscalls (poll, select, epoll, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_POLL: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall/poll",
    description = "Distribution of the rate of poll related syscalls from sample to sample",
    metadata = { unit = "sycalls/second" }
)]
pub static SYSCALL_POLL_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/poll/latency",
    description = "Distribution of the latency for poll related syscalls",
    metadata = { unit = "nanoseconds" }
)]
pub static SYSCALL_POLL_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/lock",
    description = "The number of lock related syscalls (futex, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_LOCK: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall/lock",
    description = "Distribution of the rate of lock related syscalls from sample to sample",
    metadata = { unit = "sycalls/second" }
)]
pub static SYSCALL_LOCK_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/lock/latency",
    description = "Distribution of the latency for lock related syscalls",
    metadata = { unit = "nanoseconds" }
)]
pub static SYSCALL_LOCK_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/time",
    description = "The number of time related syscalls (clock_gettime, clock_settime, clock_getres, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_TIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall/time",
    description = "Distribution of the rate of time related syscalls from sample to sample",
    metadata = { unit = "sycalls/second" }
)]
pub static SYSCALL_TIME_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/time/latency",
    description = "Distribution of the latency for time related syscalls",
    metadata = { unit = "nanoseconds" }
)]
pub static SYSCALL_TIME_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/sleep",
    description = "The number of sleep related syscalls (nanosleep, clock_nanosleep, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_SLEEP: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall/sleep",
    description = "Distribution of the rate of sleep related syscalls from sample to sample",
    metadata = { unit = "sycalls/second" }
)]
pub static SYSCALL_SLEEP_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/sleep/latency",
    description = "Distribution of the latency for sleep related syscalls",
    metadata = { unit = "nanoseconds" }
)]
pub static SYSCALL_SLEEP_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/socket",
    description = "The number of socket related syscalls (accept, connect, bind, setsockopt, ...)",
    metadata = { unit = "syscalls" }
)]
pub static SYSCALL_SOCKET: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "syscall/socket",
    description = "Distribution of the rate of socket related syscalls from sample to sample",
    metadata = { unit = "sycalls/second" }
)]
pub static SYSCALL_SOCKET_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "syscall/socket/latency",
    description = "Distribution of the latency for socket related syscalls",
    metadata = { unit = "nanoseconds" }
)]
pub static SYSCALL_SOCKET_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);
