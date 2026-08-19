use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::{MAX_CGROUPS, MAX_CPUS, MAX_PID};
use linkme::distributed_slice;

// Registered here (not in mod.rs) because this file is also `include!`d
// directly on non-Linux platforms (see `cpu/mod.rs`'s
// `#[cfg(not(target_os = "linux"))] mod stats` fallback) to keep metric
// identity stable across platforms, while `mod.rs`'s BPF sampler code is
// Linux-only. One group per `cpu_counters` map — the map read/sweep each
// brackets, not one group for the whole sampler.
pub static CPU_USAGE_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_usage"),
    "cpu_usage",
);
pub static SOFTIRQ_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_usage"),
    "softirq",
);
pub static SOFTIRQ_TIME_ACQ: AcquisitionGroup = AcquisitionGroup::new(
    crate::agent::samplers::bpf_sampler_name("cpu_usage"),
    "softirq_time",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CPU_USAGE_ACQ_REG: &'static AcquisitionGroup = &CPU_USAGE_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static SOFTIRQ_ACQ_REG: &'static AcquisitionGroup = &SOFTIRQ_ACQ;
#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static SOFTIRQ_TIME_ACQ_REG: &'static AcquisitionGroup = &SOFTIRQ_TIME_ACQ;

/*
 * bpf prog stats
 */

#[metric(
    name = "rezolus_bpf_run_count",
    description = "The number of times Rezolus BPF programs have been run",
    metadata = { sampler = "cpu_usage"}
)]
pub static BPF_RUN_COUNT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_bpf_run_time",
    description = "The amount of time Rezolus BPF programs have been executing",
    metadata = { unit = "nanoseconds", sampler = "cpu_usage"}
)]
pub static BPF_RUN_TIME: LazyCounter = LazyCounter::new(Counter::default);

/*
 * per-cpu metrics
 */

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent in each state",
    metadata = { state = "user", unit = "nanoseconds", acq_group = "cpu_usage" }
)]
pub static CPU_USAGE_USER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent in each state",
    metadata = { state = "system", unit = "nanoseconds", acq_group = "cpu_usage" }
)]
pub static CPU_USAGE_SYSTEM: CounterGroup = CounterGroup::new(MAX_CPUS);

// Deliberately a distinct metric name rather than another `cpu_usage` state:
// this time is a *subset* of user+system, not a disjoint category, so exposing
// it as a third state would double-count any sum across states.
#[metric(
    name = "cpu_usage_exited_tasks",
    description = "The amount of CPU time that was consumed by tasks which have since exited, and so is no longer attributable to any live per-task series",
    metadata = { unit = "nanoseconds", acq_group = "cpu_usage" }
)]
pub static CPU_USAGE_EXITED: CounterGroup = CounterGroup::new(MAX_CPUS);

/*
 * per-cgroup metrics
 */

#[metric(
    name = "cgroup_cpu_usage",
    description = "The amount of CPU time spent in each state on a per-cgroup basis",
    metadata = { state = "user", unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_USER: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_usage",
    description = "The amount of CPU time spent in each state on a per-cgroup basis",
    metadata = { state = "system", unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_SYSTEM: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_usage_exited_tasks",
    description = "The amount of CPU time that was consumed by tasks which have since exited, on a per-cgroup basis",
    metadata = { unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_EXITED: CounterGroup = CounterGroup::new(MAX_CGROUPS);

/*
 * per-task metrics
 */

#[metric(
    name = "task_cpu_usage",
    description = "The amount of CPU time used on a per-task basis",
    metadata = { unit = "nanoseconds" }
)]
pub static TASK_CPU_USAGE: CounterGroup = CounterGroup::new(MAX_PID);

/*
 * softirq metrics
 */

// softirq count by kind

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "hi", acq_group = "softirq" }
)]
pub static SOFTIRQ_HI: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "timer", acq_group = "softirq" }
)]
pub static SOFTIRQ_TIMER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "net_tx", acq_group = "softirq" }
)]
pub static SOFTIRQ_NET_TX: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "net_rx", acq_group = "softirq" }
)]
pub static SOFTIRQ_NET_RX: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "block", acq_group = "softirq" }
)]
pub static SOFTIRQ_BLOCK: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "irq_poll", acq_group = "softirq" }
)]
pub static SOFTIRQ_IRQ_POLL: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "tasklet", acq_group = "softirq" }
)]
pub static SOFTIRQ_TASKLET: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "sched", acq_group = "softirq" }
)]
pub static SOFTIRQ_SCHED: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "hrtimer", acq_group = "softirq" }
)]
pub static SOFTIRQ_HRTIMER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "rcu", acq_group = "softirq" }
)]
pub static SOFTIRQ_RCU: CounterGroup = CounterGroup::new(MAX_CPUS);

// softirq time by kind

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "hi", acq_group = "softirq_time" }
)]
pub static SOFTIRQ_TIME_HI: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "timer", acq_group = "softirq_time" }
)]
pub static SOFTIRQ_TIME_TIMER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "net_tx", acq_group = "softirq_time" }
)]
pub static SOFTIRQ_TIME_NET_TX: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "net_rx", acq_group = "softirq_time" }
)]
pub static SOFTIRQ_TIME_NET_RX: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "block", acq_group = "softirq_time" }
)]
pub static SOFTIRQ_TIME_BLOCK: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "irq_poll", acq_group = "softirq_time" }
)]
pub static SOFTIRQ_TIME_IRQ_POLL: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "tasklet", acq_group = "softirq_time" }
)]
pub static SOFTIRQ_TIME_TASKLET: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "sched", acq_group = "softirq_time" }
)]
pub static SOFTIRQ_TIME_SCHED: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "hrtimer", acq_group = "softirq_time" }
)]
pub static SOFTIRQ_TIME_HRTIMER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "rcu", acq_group = "softirq_time" }
)]
pub static SOFTIRQ_TIME_RCU: CounterGroup = CounterGroup::new(MAX_CPUS);
