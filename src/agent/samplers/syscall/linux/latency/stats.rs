use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

// this is hard-coded still and must match the BPF histograms which are fixed to
// use 2^64-1 as the max value
static LATENCY_HISTOGRAM_MAX: u8 = 64;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "syscall_latency"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "syscall_latency"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "other" }
)]
pub static SYSCALL_OTHER_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "read" }
)]
pub static SYSCALL_READ_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "write" }
)]
pub static SYSCALL_WRITE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "poll" }
)]
pub static SYSCALL_POLL_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "lock" }
)]
pub static SYSCALL_LOCK_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "time" }
)]
pub static SYSCALL_TIME_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "sleep" }
)]
pub static SYSCALL_SLEEP_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "socket" }
)]
pub static SYSCALL_SOCKET_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "yield" }
)]
pub static SYSCALL_YIELD_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "filesystem" }
)]
pub static SYSCALL_FILESYSTEM_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "memory" }
)]
pub static SYSCALL_MEMORY_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "process" }
)]
pub static SYSCALL_PROCESS_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "query" }
)]
pub static SYSCALL_QUERY_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "ipc" }
)]
pub static SYSCALL_IPC_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "timer" }
)]
pub static SYSCALL_TIMER_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "event" }
)]
pub static SYSCALL_EVENT_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);
