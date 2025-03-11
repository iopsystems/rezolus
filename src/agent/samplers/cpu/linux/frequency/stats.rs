use metriken::*;

use crate::agent::*;

// per-CPU metrics

#[metric(
    name = "cpu_aperf",
    metadata = { unit = "cycles" }
)]
pub static CPU_APERF: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_mperf",
    metadata = { unit = "cycles" }
)]
pub static CPU_MPERF: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_tsc",
    metadata = { unit = "cycles" }
)]
pub static CPU_TSC: CounterGroup = CounterGroup::new(MAX_CPUS);

// per-cgroup metrics

#[metric(
    name = "cgroup_cpu_aperf",
    metadata = { unit = "cycles" }
)]
pub static CGROUP_CPU_APERF: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_mperf",
    metadata = { unit = "cycles" }
)]
pub static CGROUP_CPU_MPERF: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_tsc",
    metadata = { unit = "cycles" }
)]
pub static CGROUP_CPU_TSC: CounterGroup = CounterGroup::new(MAX_CGROUPS);
