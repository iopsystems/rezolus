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

