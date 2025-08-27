use metriken::*;

use crate::agent::*;

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
   description = "The amount of CPU time spent in the given state",
    metadata = { state = "user", unit = "nanoseconds" }
)]
pub static CPU_USAGE_USER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent executing tasks in kernel mode",
    metadata = { state = "system", unit = "nanoseconds" }
)]
pub static CPU_USAGE_SYSTEM: CounterGroup = CounterGroup::new(MAX_CPUS);

/*
 * per-cgroup metrics
 */

#[metric(
    name = "cgroup_cpu_usage",
    description = "The amount of CPU time spent busy on a per-cgroup basis",
    metadata = { state = "user", unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_USER: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_usage",
    description = "The amount of CPU time spent executing tasks in kernel mode on a per-cgroup basis",
    metadata = { state = "system", unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_SYSTEM: CounterGroup = CounterGroup::new(MAX_CGROUPS);

/*
 * softirq metrics
 */

// softirq count by kind

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "hi" }
)]
pub static SOFTIRQ_HI: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "timer" }
)]
pub static SOFTIRQ_TIMER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "net_tx" }
)]
pub static SOFTIRQ_NET_TX: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "net_rx" }
)]
pub static SOFTIRQ_NET_RX: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "block" }
)]
pub static SOFTIRQ_BLOCK: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "irq_poll" }
)]
pub static SOFTIRQ_IRQ_POLL: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "tasklet" }
)]
pub static SOFTIRQ_TASKLET: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "sched" }
)]
pub static SOFTIRQ_SCHED: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "hrtimer" }
)]
pub static SOFTIRQ_HRTIMER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq",
    description = "The count of softirqs",
    metadata = { unit = "interrupts", kind = "rcu" }
)]
pub static SOFTIRQ_RCU: CounterGroup = CounterGroup::new(MAX_CPUS);

// softirq time by kind

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "hi" }
)]
pub static SOFTIRQ_TIME_HI: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "timer" }
)]
pub static SOFTIRQ_TIME_TIMER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "net_tx" }
)]
pub static SOFTIRQ_TIME_NET_TX: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "net_rx" }
)]
pub static SOFTIRQ_TIME_NET_RX: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "block" }
)]
pub static SOFTIRQ_TIME_BLOCK: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "irq_poll" }
)]
pub static SOFTIRQ_TIME_IRQ_POLL: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "tasklet" }
)]
pub static SOFTIRQ_TIME_TASKLET: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "sched" }
)]
pub static SOFTIRQ_TIME_SCHED: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "hrtimer" }
)]
pub static SOFTIRQ_TIME_HRTIMER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "softirq_time",
    description = "The time spent in softirq handlers",
    metadata = { unit = "nanoseconds", kind = "rcu" }
)]
pub static SOFTIRQ_TIME_RCU: CounterGroup = CounterGroup::new(MAX_CPUS);
