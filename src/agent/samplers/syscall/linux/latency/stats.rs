use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

// this is hard-coded still and must match the BPF histograms which are fixed to
// use 2^64-1 as the max value
static LATENCY_HISTOGRAM_MAX: u8 = 64;

#[metric(
    name = "syscall_latency",
    description = "Distribution of the latency for all other syscalls",
    metadata = { unit = "nanoseconds", op = "other" }
)]
pub static SYSCALL_OTHER_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of the latency for read related syscalls",
    metadata = { unit = "nanoseconds", op = "read" }
)]
pub static SYSCALL_READ_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of the latency for write related syscalls",
    metadata = { unit = "nanoseconds", op = "write" }
)]
pub static SYSCALL_WRITE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of the latency for poll related syscalls",
    metadata = { unit = "nanoseconds", op = "poll" }
)]
pub static SYSCALL_POLL_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of the latency for lock related syscalls",
    metadata = { unit = "nanoseconds", op = "lock" }
)]
pub static SYSCALL_LOCK_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of the latency for time related syscalls",
    metadata = { unit = "nanoseconds", op = "time" }
)]
pub static SYSCALL_TIME_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of the latency for sleep related syscalls",
    metadata = { unit = "nanoseconds", op = "sleep" }
)]
pub static SYSCALL_SLEEP_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of the latency for socket related syscalls",
    metadata = { unit = "nanoseconds", op = "socket" }
)]
pub static SYSCALL_SOCKET_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of the latency for yield related syscalls",
    metadata = { unit = "nanoseconds", op = "yield" }
)]
pub static SYSCALL_YIELD_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);
