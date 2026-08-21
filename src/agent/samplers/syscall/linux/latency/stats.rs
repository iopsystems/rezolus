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
// Linux-only.
//
// ONE group for all 16 syscall-class latency histograms: they are LIKE
// ENTITIES — instances of a single "syscall latency" family distinguished
// by the `op` label — read back-to-back as one sweep, not 16 independent
// read sections. See the `# Granularity rule` on
// `crate::agent::samplers::ACQUISITION_GROUPS` and
// docs/journal/2026-08-17-window-sidecar-cost.md's addendum, which names
// syscall_latency explicitly as the collapse-to-one-group case. Mechanism:
// `BpfBuilder::histogram` batches every call naming this same group into
// one `HistogramBatch`, so the group is stamped once per refresh, not once
// per histogram — see `bpf/histogram.rs`.
pub static LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("syscall_latency"),
    "syscall_latency_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static LATENCIES_ACQ_REG: &'static AcquisitionGroup = &LATENCIES_ACQ;

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
