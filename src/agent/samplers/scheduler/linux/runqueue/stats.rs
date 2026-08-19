use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CGROUPS, MAX_CPUS};
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `scheduler/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only.
//
/// Brackets the `counters` cpu_counters refresh (single writer: this
/// sampler's own BPF refresh path).
pub static COUNTERS_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("scheduler_runqueue"),
    "scheduler_runqueue_counters",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static COUNTERS_ACQ_REG: &'static AcquisitionGroup = &COUNTERS_ACQ;

/// One group per histogram map read (each `Histogram` reads exactly one
/// BPF map per refresh, so there is no multi-map section to share).
pub static RUNQLAT_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("scheduler_runqueue"),
    "scheduler_runqueue_runqlat",
);
pub static RUNNING_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("scheduler_runqueue"),
    "scheduler_runqueue_running",
);
pub static OFFCPU_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("scheduler_runqueue"),
    "scheduler_runqueue_offcpu",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static RUNQLAT_ACQ_REG: &'static AcquisitionGroup = &RUNQLAT_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static RUNNING_ACQ_REG: &'static AcquisitionGroup = &RUNNING_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static OFFCPU_ACQ_REG: &'static AcquisitionGroup = &OFFCPU_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "scheduler_runqueue"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "scheduler_runqueue"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * system-wide
 */

#[metric(
    name = "scheduler_runqueue_latency",
    description = "Distribution of the amount of time tasks were waiting in the runqueue",
    metadata = { unit = "nanoseconds", acq_group = "scheduler_runqueue_runqlat" }
)]
pub static SCHEDULER_RUNQUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_running",
    description = "Distribution of the amount of time tasks were on-CPU",
    metadata = { unit = "nanoseconds", acq_group = "scheduler_runqueue_running" }
)]
pub static SCHEDULER_RUNNING: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_offcpu",
    description = "Distribution of the amount of time tasks were off-CPU",
    metadata = { unit = "nanoseconds", acq_group = "scheduler_runqueue_offcpu" }
)]
pub static SCHEDULER_OFFCPU: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_context_switch",
    description = "The number of involuntary context switches, where a runnable task was preempted. Switches away from the idle task are excluded, since nothing was competing for the CPU",
    metadata = { kind = "involuntary", acq_group = "scheduler_runqueue_counters" }
)]
pub static SCHEDULER_IVCSW: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "scheduler_context_switch",
    description = "The number of voluntary context switches, where a task left the CPU because it blocked",
    metadata = { kind = "voluntary", acq_group = "scheduler_runqueue_counters" }
)]
pub static SCHEDULER_VCSW: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "scheduler_runqueue_wait",
    description = "Tracks time spent in the runqueue on a per-CPU basis",
    metadata = { unit = "nanoseconds", acq_group = "scheduler_runqueue_counters" }
)]
pub static SCHEDULER_RUNQUEUE_WAIT: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "scheduler_discarded_samples",
    description = "The number of scheduler timing samples discarded because the two timestamps arrived out of order across CPUs, which would otherwise underflow into the top histogram bucket",
    metadata = { acq_group = "scheduler_runqueue_counters" }
)]
pub static SCHEDULER_DISCARDED: CounterGroup = CounterGroup::new(MAX_CPUS);

/*
 * per-cgroup
 */

#[metric(
    name = "cgroup_scheduler_runqueue_wait",
    description = "Tracks time spent in the runqueue on a per-cgroup basis",
    metadata = { state = "wait", unit = "nanoseconds" }
)]
pub static CGROUP_SCHEDULER_RUNQUEUE_WAIT: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_scheduler_offcpu",
    description = "Tracks the time when tasks were off-CPU on a per-cgroup basis",
    metadata = { state = "offcpu", unit = "nanoseconds" }
)]
pub static CGROUP_SCHEDULER_OFFCPU: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_scheduler_context_switch",
    description = "The number of involuntary context switches on a per-cgroup basis, where a runnable task was preempted. Switches away from the idle task are excluded, since nothing was competing for the CPU",
    metadata = { kind = "involuntary" }
)]
pub static CGROUP_SCHEDULER_IVCSW: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_scheduler_context_switch",
    description = "The number of voluntary context switches on a per-cgroup basis, where a task left the CPU because it blocked",
    metadata = { kind = "voluntary" }
)]
pub static CGROUP_SCHEDULER_VCSW: CounterGroup = CounterGroup::new(MAX_CGROUPS);
