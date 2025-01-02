use metriken::*;

use crate::common::*;

// per-CPU metrics

#[metric(
    name = "cpu/aperf",
    formatter = formatter,
    metadata = { unit = "cycles" }
)]
pub static CPU_APERF: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/mperf",
    formatter = formatter,
    metadata = { unit = "cycles" }
)]
pub static CPU_MPERF: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu/tsc",
    formatter = formatter,
    metadata = { unit = "cycles" }
)]
pub static CPU_TSC: CounterGroup = CounterGroup::new(MAX_CPUS);

// per-cgroup metrics

#[metric(
    name = "cgroup/cpu/aperf",
    formatter = cgroup_formatter,
    metadata = { unit = "cycles" }
)]
pub static CGROUP_CPU_APERF: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup/cpu/mperf",
    formatter = cgroup_formatter,
    metadata = { unit = "cycles" }
)]
pub static CGROUP_CPU_MPERF: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup/cpu/tsc",
    formatter = cgroup_formatter,
    metadata = { unit = "cycles" }
)]
pub static CGROUP_CPU_TSC: CounterGroup = CounterGroup::new(MAX_CGROUPS);

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
