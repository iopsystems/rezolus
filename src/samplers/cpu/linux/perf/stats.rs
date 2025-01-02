use metriken::*;

use crate::common::*;

// per-CPU metrics

#[metric(
    name = "cpu/cycles",
    description = "The number of elapsed CPU cycles",
    formatter = formatter,
    metadata = { unit = "cycles" }
)]
pub static CPU_CYCLES: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/instructions",
    description = "The number of instructions retired",
    formatter = formatter,
    metadata = { unit = "instructions" }
)]
pub static CPU_INSTRUCTIONS: CounterGroup = CounterGroup::new(MAX_CPUS);

// per-cgroup metrics

#[metric(
    name = "cgroup/cpu/cycles",
    description = "The number of elapsed CPU cycles on a per-cgroup basis",
    formatter = cgroup_formatter,
    metadata = { unit = "cycles" }
)]
pub static CGROUP_CPU_CYCLES: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup/cpu/instructions",
    description = "The number of elapsed CPU cycles on a per-cgroup basis",
    formatter = cgroup_formatter,
    metadata = { unit = "instructions" }
)]
pub static CGROUP_CPU_INSTRUCTIONS: CounterGroup = CounterGroup::new(MAX_CGROUPS);

// formatters

pub fn formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            format!("{}/cpu", metric.name())
        }
        _ => metric.name().to_string(),
    }
}

pub fn cgroup_formatter(metric: &MetricEntry, format: Format) -> String {
    match format {
        Format::Simple => {
            format!("{}/cgroup", metric.name())
        }
        _ => metric.name().to_string(),
    }
}
