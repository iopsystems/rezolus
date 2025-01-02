use metriken::*;

use crate::common::*;

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent busy",
    formatter = formatter,
    metadata = { state = "busy", unit = "nanoseconds" }
)]
pub static CPU_USAGE_BUSY: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
   description = "The amount of CPU time spent executing normal tasks is user mode",
    formatter = formatter,
    metadata = { state = "user", unit = "nanoseconds" }
)]
pub static CPU_USAGE_USER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent executing low priority tasks in user mode",
    formatter = formatter,
    metadata = { state = "nice", unit = "nanoseconds" }
)]
pub static CPU_USAGE_NICE: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent executing tasks in kernel mode",
    formatter = formatter,
    metadata = { state = "system", unit = "nanoseconds" }
)]
pub static CPU_USAGE_SYSTEM: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent servicing softirqs",
    formatter = formatter,
    metadata = { state = "softirq", unit = "nanoseconds" }
)]
pub static CPU_USAGE_SOFTIRQ: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent servicing interrupts",
    formatter = formatter,
    metadata = { state = "irq", unit = "nanoseconds" }
)]
pub static CPU_USAGE_IRQ: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time stolen by the hypervisor",
    formatter = formatter,
    metadata = { state = "steal", unit = "nanoseconds" }
)]
pub static CPU_USAGE_STEAL: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest",
    formatter = formatter,
    metadata = { state = "guest", unit = "nanoseconds" }
)]
pub static CPU_USAGE_GUEST: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest in low priority mode",
    formatter = formatter,
    metadata = { state = "guest_nice", unit = "nanoseconds" }
)]
pub static CPU_USAGE_GUEST_NICE: CounterGroup = CounterGroup::new(MAX_CPUS);

pub fn formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            let state = metric
                .metadata()
                .get("state")
                .expect("no `state` for metric formatter");
            format!("{}/{state}/cpu", metric.name())
        }
        _ => metric.name().to_string(),
    }
}
