use crate::common::CounterGroup;
use crate::samplers::cpu::stats::*;

use metriken::*;

pub static MAX_CPUS: usize = 1024;

#[metric(
    name = "cpu/usage/total",
    description = "The amount of CPU time spent servicing interrupts",
    formatter = cpu_usage_total_formatter,
    metadata = { state = "irq", unit = "nanoseconds" }
)]
pub static CPU_USAGE_IRQ: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage/total",
    description = "The amount of CPU time spent servicing softirqs",
    formatter = cpu_usage_total_formatter,
    metadata = { state = "softirq", unit = "nanoseconds" }
)]
pub static CPU_USAGE_SOFTIRQ: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage/total",
    description = "The amount of CPU time stolen by the hypervisor",
    formatter = cpu_usage_total_formatter,
    metadata = { state = "steal", unit = "nanoseconds" }
)]
pub static CPU_USAGE_STEAL: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage/total",
    description = "The amount of CPU time spent running a virtual CPU for a guest",
    formatter = cpu_usage_total_formatter,
    metadata = { state = "guest", unit = "nanoseconds" }
)]
pub static CPU_USAGE_GUEST: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage/total",
    description = "The amount of CPU time spent running a virtual CPU for a guest in low priority mode",
    formatter = cpu_usage_total_formatter,
    metadata = { state = "guest_nice", unit = "nanoseconds" }
)]
pub static CPU_USAGE_GUEST_NICE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent busy",
    formatter = cpu_usage_percore_formatter,
    metadata = { state = "busy", unit = "nanoseconds" }
)]
pub static CPU_USAGE_PERCORE_BUSY: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
   description = "The amount of CPU time spent executing normal tasks is user mode",
    formatter = cpu_usage_percore_formatter,
    metadata = { state = "user", unit = "nanoseconds" }
)]
pub static CPU_USAGE_PERCORE_USER: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent executing low priority tasks in user mode",
    formatter = cpu_usage_percore_formatter,
    metadata = { state = "nice", unit = "nanoseconds" }
)]
pub static CPU_USAGE_PERCORE_NICE: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent executing tasks in kernel mode",
    formatter = cpu_usage_percore_formatter,
    metadata = { state = "system", unit = "nanoseconds" }
)]
pub static CPU_USAGE_PERCORE_SYSTEM: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent servicing softirqs",
    formatter = cpu_usage_percore_formatter,
    metadata = { state = "softirq", unit = "nanoseconds" }
)]
pub static CPU_USAGE_PERCORE_SOFTIRQ: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent servicing interrupts",
    formatter = cpu_usage_percore_formatter,
    metadata = { state = "irq", unit = "nanoseconds" }
)]
pub static CPU_USAGE_PERCORE_IRQ: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time stolen by the hypervisor",
    formatter = cpu_usage_total_formatter,
    metadata = { state = "steal", unit = "nanoseconds" }
)]
pub static CPU_USAGE_PERCORE_STEAL: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest",
    formatter = cpu_usage_percore_formatter,
    metadata = { state = "guest", unit = "nanoseconds" }
)]
pub static CPU_USAGE_PERCORE_GUEST: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/usage",
    description = "The amount of CPU time spent running a virtual CPU for a guest in low priority mode",
    formatter = cpu_usage_percore_formatter,
    metadata = { state = "guest_nice", unit = "nanoseconds" }
)]
pub static CPU_USAGE_PERCORE_GUEST_NICE: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/cycles/total",
    description = "The number of elapsed CPU cycles",
    metadata = { unit = "cycles" }
)]
pub static CPU_CYCLES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/instructions/total",
    description = "The number of instructions retired",
    metadata = { unit = "instructions" }
)]
pub static CPU_INSTRUCTIONS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(name = "cpu/aperf/total")]
pub static CPU_APERF: LazyCounter = LazyCounter::new(Counter::default);

#[metric(name = "cpu/mperf/total")]
pub static CPU_MPERF: LazyCounter = LazyCounter::new(Counter::default);

#[metric(name = "cpu/tsc/total")]
pub static CPU_TSC: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/cycles",
    description = "The number of elapsed CPU cycles",
    formatter = cpu_metric_percore_formatter,
    metadata = { unit = "cycles" }
)]
pub static CPU_CYCLES_PERCORE: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/instructions",
    description = "The number of instructions retired",
    formatter = cpu_metric_percore_formatter,
    metadata = { unit = "instructions" }
)]
pub static CPU_INSTRUCTIONS_PERCORE: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/aperf",
    formatter = cpu_metric_percore_formatter
)]
pub static CPU_APERF_PERCORE: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/mperf",
    formatter = cpu_metric_percore_formatter
)]
pub static CPU_MPERF_PERCORE: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/tsc",
    formatter = cpu_metric_percore_formatter
)]
pub static CPU_TSC_PERCORE: CounterGroup = CounterGroup::new(MAX_CPUS);

pub fn cpu_metric_percore_formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            format!("{}/cpu", metric.name())
        }
        _ => metric.name().to_string(),
    }
}

pub fn cpu_usage_percore_formatter(metric: &MetricEntry, format: Format) -> String {
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
