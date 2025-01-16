use metriken::*;

use crate::common::*;

// per-CPU metrics

#[metric(
    name = "cpu_cycles",
    description = "The number of elapsed CPU cycles",
    metadata = { unit = "cycles" }
)]
pub static CPU_CYCLES: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_instructions",
    description = "The number of instructions retired",
    metadata = { unit = "instructions" }
)]
pub static CPU_INSTRUCTIONS: CounterGroup = CounterGroup::new(MAX_CPUS);

// per-cgroup metrics

#[metric(
    name = "cgroup_cpu_cycles",
    description = "The number of elapsed CPU cycles on a per-cgroup basis",
    metadata = { unit = "cycles" }
)]
pub static CGROUP_CPU_CYCLES: CounterGroup = CounterGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_cpu_instructions",
    description = "The number of elapsed CPU cycles on a per-cgroup basis",
    metadata = { unit = "instructions" }
)]
pub static CGROUP_CPU_INSTRUCTIONS: CounterGroup = CounterGroup::new(MAX_CGROUPS);
