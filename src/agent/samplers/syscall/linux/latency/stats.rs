use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// this is hard-coded still and must match the BPF histograms which are fixed to
// use 2^64-1 as the max value
static LATENCY_HISTOGRAM_MAX: u8 = 64;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `syscall/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only. One group per histogram map read (each `Histogram` reads
// exactly one BPF map per refresh) — 16 syscall-class latency histograms,
// 16 groups.
macro_rules! syscall_latency_group {
    ($static_name:ident, $shortname:literal) => {
        pub static $static_name: AcquisitionGroup = AcquisitionGroup::new(
            crate::agent::samplers::bpf_sampler_name("syscall_latency"),
            concat!("syscall_latency_", $shortname),
        );
    };
}

syscall_latency_group!(OTHER_LATENCY_ACQ, "other_latency");
syscall_latency_group!(READ_LATENCY_ACQ, "read_latency");
syscall_latency_group!(WRITE_LATENCY_ACQ, "write_latency");
syscall_latency_group!(POLL_LATENCY_ACQ, "poll_latency");
syscall_latency_group!(LOCK_LATENCY_ACQ, "lock_latency");
syscall_latency_group!(TIME_LATENCY_ACQ, "time_latency");
syscall_latency_group!(SLEEP_LATENCY_ACQ, "sleep_latency");
syscall_latency_group!(SOCKET_LATENCY_ACQ, "socket_latency");
syscall_latency_group!(YIELD_LATENCY_ACQ, "yield_latency");
syscall_latency_group!(FILESYSTEM_LATENCY_ACQ, "filesystem_latency");
syscall_latency_group!(MEMORY_LATENCY_ACQ, "memory_latency");
syscall_latency_group!(PROCESS_LATENCY_ACQ, "process_latency");
syscall_latency_group!(QUERY_LATENCY_ACQ, "query_latency");
syscall_latency_group!(IPC_LATENCY_ACQ, "ipc_latency");
syscall_latency_group!(TIMER_LATENCY_ACQ, "timer_latency");
syscall_latency_group!(EVENT_LATENCY_ACQ, "event_latency");

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static OTHER_LATENCY_ACQ_REG: &'static AcquisitionGroup = &OTHER_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static READ_LATENCY_ACQ_REG: &'static AcquisitionGroup = &READ_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static WRITE_LATENCY_ACQ_REG: &'static AcquisitionGroup = &WRITE_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static POLL_LATENCY_ACQ_REG: &'static AcquisitionGroup = &POLL_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static LOCK_LATENCY_ACQ_REG: &'static AcquisitionGroup = &LOCK_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static TIME_LATENCY_ACQ_REG: &'static AcquisitionGroup = &TIME_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static SLEEP_LATENCY_ACQ_REG: &'static AcquisitionGroup = &SLEEP_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static SOCKET_LATENCY_ACQ_REG: &'static AcquisitionGroup = &SOCKET_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static YIELD_LATENCY_ACQ_REG: &'static AcquisitionGroup = &YIELD_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static FILESYSTEM_LATENCY_ACQ_REG: &'static AcquisitionGroup = &FILESYSTEM_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static MEMORY_LATENCY_ACQ_REG: &'static AcquisitionGroup = &MEMORY_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static PROCESS_LATENCY_ACQ_REG: &'static AcquisitionGroup = &PROCESS_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static QUERY_LATENCY_ACQ_REG: &'static AcquisitionGroup = &QUERY_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static IPC_LATENCY_ACQ_REG: &'static AcquisitionGroup = &IPC_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static TIMER_LATENCY_ACQ_REG: &'static AcquisitionGroup = &TIMER_LATENCY_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static EVENT_LATENCY_ACQ_REG: &'static AcquisitionGroup = &EVENT_LATENCY_ACQ;

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
    metadata = { unit = "nanoseconds", op = "other", acq_group = "syscall_latency_other_latency" }
)]
pub static SYSCALL_OTHER_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "read", acq_group = "syscall_latency_read_latency" }
)]
pub static SYSCALL_READ_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "write", acq_group = "syscall_latency_write_latency" }
)]
pub static SYSCALL_WRITE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "poll", acq_group = "syscall_latency_poll_latency" }
)]
pub static SYSCALL_POLL_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "lock", acq_group = "syscall_latency_lock_latency" }
)]
pub static SYSCALL_LOCK_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "time", acq_group = "syscall_latency_time_latency" }
)]
pub static SYSCALL_TIME_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "sleep", acq_group = "syscall_latency_sleep_latency" }
)]
pub static SYSCALL_SLEEP_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "socket", acq_group = "syscall_latency_socket_latency" }
)]
pub static SYSCALL_SOCKET_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "yield", acq_group = "syscall_latency_yield_latency" }
)]
pub static SYSCALL_YIELD_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "filesystem", acq_group = "syscall_latency_filesystem_latency" }
)]
pub static SYSCALL_FILESYSTEM_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "memory", acq_group = "syscall_latency_memory_latency" }
)]
pub static SYSCALL_MEMORY_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "process", acq_group = "syscall_latency_process_latency" }
)]
pub static SYSCALL_PROCESS_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "query", acq_group = "syscall_latency_query_latency" }
)]
pub static SYSCALL_QUERY_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "ipc", acq_group = "syscall_latency_ipc_latency" }
)]
pub static SYSCALL_IPC_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "timer", acq_group = "syscall_latency_timer_latency" }
)]
pub static SYSCALL_TIMER_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "syscall_latency",
    description = "Distribution of syscall latencies",
    metadata = { unit = "nanoseconds", op = "event", acq_group = "syscall_latency_event_latency" }
)]
pub static SYSCALL_EVENT_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);
