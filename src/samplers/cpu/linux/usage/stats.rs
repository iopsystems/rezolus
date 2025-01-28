use metriken::*;

use crate::common::*;

/*
 * per-cpu metrics
 */

#[metric(
    name = "cpu_usage",
   description = "The amount of CPU time spent executing normal tasks is user mode",
    metadata = { state = "user", unit = "nanoseconds" }
)]
pub static CPU_USAGE_USER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent executing low priority tasks in user mode",
    metadata = { state = "nice", unit = "nanoseconds" }
)]
pub static CPU_USAGE_NICE: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent executing tasks in kernel mode",
    metadata = { state = "system", unit = "nanoseconds" }
)]
pub static CPU_USAGE_SYSTEM: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent servicing softirqs",
    metadata = { state = "softirq", unit = "nanoseconds" }
)]
pub static CPU_USAGE_SOFTIRQ: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent servicing interrupts",
    metadata = { state = "irq", unit = "nanoseconds" }
)]
pub static CPU_USAGE_IRQ: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time stolen by the hypervisor",
    metadata = { state = "steal", unit = "nanoseconds" }
)]
pub static CPU_USAGE_STEAL: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest",
    metadata = { state = "guest", unit = "nanoseconds" }
)]
pub static CPU_USAGE_GUEST: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest in low priority mode",
    metadata = { state = "guest_nice", unit = "nanoseconds" }
)]
pub static CPU_USAGE_GUEST_NICE: CounterGroup = CounterGroup::new(MAX_CPUS);

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
    description = "The amount of CPU time spent executing low priority tasks in user mode on a per-cgroup basis",
    metadata = { state = "nice", unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_NICE: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_usage",
    description = "The amount of CPU time spent executing tasks in kernel mode on a per-cgroup basis",
    metadata = { state = "system", unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_SYSTEM: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_usage",
    description = "The amount of CPU time spent servicing softirqs on a per-cgroup basis",
    metadata = { state = "softirq", unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_SOFTIRQ: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_usage",
    description = "The amount of CPU time spent servicing interrupts on a per-cgroup basis",
    metadata = { state = "irq", unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_IRQ: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_usage",
    description = "The amount of CPU time stolen by the hypervisor on a per-cgroup basis",
    metadata = { state = "steal", unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_STEAL: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest on a per-cgroup basis",
    metadata = { state = "guest", unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_GUEST: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest in low priority mode on a per-cgroup basis",
    metadata = { state = "guest_nice", unit = "nanoseconds" }
)]
pub static CGROUP_CPU_USAGE_GUEST_NICE: CounterGroup = CounterGroup::new(MAX_CGROUPS);
