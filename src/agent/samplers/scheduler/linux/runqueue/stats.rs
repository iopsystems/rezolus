use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

use crate::agent::*;

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
    metadata = { unit = "nanoseconds" }
)]
pub static SCHEDULER_RUNQUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_running",
    description = "Distribution of the amount of time tasks were on-CPU",
    metadata = { unit = "nanoseconds" }
)]
pub static SCHEDULER_RUNNING: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_offcpu",
    description = "Distribution of the amount of time tasks were off-CPU",
    metadata = { unit = "nanoseconds" }
)]
pub static SCHEDULER_OFFCPU: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_context_switch",
    description = "The number of involuntary context switches",
    metadata = { kind = "involuntary" }
)]
pub static SCHEDULER_IVCSW: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "scheduler_runqueue_wait",
    description = "Tracks time spent in the runqueue on a per-CPU basis",
    metadata = { unit = "nanoseconds" }
)]
pub static SCHEDULER_RUNQUEUE_WAIT: CounterGroup = CounterGroup::new(MAX_CPUS);

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
    description = "The number of involuntary context switches on a per-cgroup basis",
    metadata = { kind = "involuntary" }
)]
pub static CGROUP_SCHEDULER_IVCSW: CounterGroup = CounterGroup::new(MAX_CGROUPS);
