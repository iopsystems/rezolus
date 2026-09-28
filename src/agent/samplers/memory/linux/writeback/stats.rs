use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

// this is hard-coded still and must match the BPF histograms which are fixed to
// use 2^64-1 as the max value
static LATENCY_HISTOGRAM_MAX: u8 = 64;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `memory/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
// Two groups (principle 18): every counter lives in one `counters` map read
// in one sweep, as `blockio_requests` reads ops and bytes from one map; the
// throttle histogram is its own family.
pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("memory_writeback"),
    "memory_writeback_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

pub static THROTTLE_LATENCIES_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("memory_writeback"),
    "memory_writeback_throttle_latencies",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static THROTTLE_LATENCIES_ACQ_REG: &'static AcquisitionGroup = &THROTTLE_LATENCIES_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "memory_writeback"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "memory_writeback"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * throttling: balance_dirty_pages
 */

#[metric(
    name = "writeback_throttle_latency",
    description = "Distribution of the time a task that dirtied pages was made to sleep by the writeback throttle (balance_dirty_pages), in nanoseconds. The kernel reports the pause in whole milliseconds. Only sleeps are counted; a check that did not throttle adds nothing here",
    metadata = { unit = "nanoseconds", acq_group = "memory_writeback_throttle_latencies" }
)]
pub static WRITEBACK_THROTTLE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, LATENCY_HISTOGRAM_MAX);

#[metric(
    name = "writeback_throttle_checks",
    description = "The number of times a task that dirtied pages was checked against the dirty limits (balance_dirty_pages). The kernel runs the check once per ratelimit's worth of pages dirtied, so this rises with write throughput",
    metadata = { unit = "operations", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_THROTTLE_CHECKS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "writeback_throttle_events",
    description = "The number of dirty-limit checks that made the writer sleep. Non-zero means dirty pages are accumulating faster than writeback drains them and writers are paying for it on their own thread",
    metadata = { unit = "operations", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_THROTTLE_EVENTS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "writeback_throttled_time",
    description = "Total time writers have spent asleep in the writeback throttle, in nanoseconds; the sum of writeback_throttle_latency",
    metadata = { unit = "nanoseconds", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_THROTTLED_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * flusher activity
 */

#[metric(
    name = "writeback_pages_written",
    description = "Pages the flusher threads wrote back to storage (writeback_pages_written), summed over writeback passes",
    metadata = { unit = "pages", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_PAGES_WRITTEN: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "writeback_runs",
    description = "Writeback work items started because dirty pages exceeded the background threshold",
    metadata = { unit = "operations", reason = "background", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_RUNS_BACKGROUND: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "writeback_runs",
    description = "Writeback work items started by memory reclaim (vmscan) to free dirty pages",
    metadata = { unit = "operations", reason = "vmscan", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_RUNS_VMSCAN: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "writeback_runs",
    description = "Writeback work items started by sync, syncfs or a filesystem-wide flush",
    metadata = { unit = "operations", reason = "sync", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_RUNS_SYNC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "writeback_runs",
    description = "Writeback work items started by the periodic flusher (dirty_writeback_centisecs), writing pages older than dirty_expire_centisecs",
    metadata = { unit = "operations", reason = "periodic", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_RUNS_PERIODIC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "writeback_runs",
    description = "Writeback work items started by laptop mode's timer",
    metadata = { unit = "operations", reason = "laptop_timer", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_RUNS_LAPTOP_TIMER: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "writeback_runs",
    description = "Writeback work items started by a filesystem to free space",
    metadata = { unit = "operations", reason = "fs_free_space", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_RUNS_FS_FREE_SPACE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "writeback_runs",
    description = "Writeback work items started by the flusher forker thread",
    metadata = { unit = "operations", reason = "forker_thread", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_RUNS_FORKER_THREAD: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "writeback_runs",
    description = "Writeback work items started to flush pages a cgroup dirtied on another cgroup's writeback domain",
    metadata = { unit = "operations", reason = "foreign_flush", acq_group = "memory_writeback_counters" }
)]
pub static WRITEBACK_RUNS_FOREIGN_FLUSH: LazyCounter = LazyCounter::new(Counter::default);
