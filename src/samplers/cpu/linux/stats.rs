use crate::samplers::cpu::stats::*;

use metriken::*;

#[metric(
    name = "cpu/usage/total",
    description = "The amount of CPU time spent waiting for IO to complete",
    formatter = cpu_usage_total_formatter,
    metadata = { state = "io_wait", unit = "nanoseconds" }
)]
pub static CPU_USAGE_IO_WAIT: LazyCounter = LazyCounter::new(Counter::default);

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
    name = "cpu/cycles/total",
    description = "The number of elapsed CPU cycles",
    formatter = simple_formatter,
    metadata = { unit = "cycles" }
)]
pub static CPU_CYCLES: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/instructions/total",
    description = "The number of instructions retired",
    formatter = simple_formatter,
    metadata = { unit = "instructions" }
)]
pub static CPU_INSTRUCTIONS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu/base_frequency/average",
    description = "Average base CPU frequency (MHz)",
    metadata = { unit = "megahertz" }
)]
pub static CPU_BASE_FREQUENCY_AVERAGE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu/frequency/average",
    description = "Average running CPU frequency (MHz): SUM(RUNNING_FREQUENCY_CPU0...N)/N",
    metadata = { unit = "megahertz" }
)]
pub static CPU_FREQUENCY_AVERAGE: LazyGauge = LazyGauge::new(Gauge::default);

pub fn simple_formatter(metric: &MetricEntry, _format: Format) -> String {
    metric.name().to_string()
}

pub fn cpu_metric_percore_formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            let id = metric
                .metadata()
                .get("id")
                .expect("no `id` for metric formatter");
            format!("{}/cpu{id}", metric.name())
        }
        _ => metric.name().to_string(),
    }
}

pub fn cpu_usage_percore_formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            let id = metric
                .metadata()
                .get("id")
                .expect("no `id` for metric formatter");
            let state = metric
                .metadata()
                .get("state")
                .expect("no `state` for metric formatter");
            format!("{}/{state}/cpu{id}", metric.name())
        }
        _ => metric.name().to_string(),
    }
}
