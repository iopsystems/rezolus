use metriken::*;

use crate::agent::*;

// per-CPU metrics

#[metric(
    name = "cpu_branch_instructions",
    description = "The number of branch instructions retired"
)]
pub static CPU_BRANCH_INSTRUCTIONS: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_branch_misses",
    description = "The number of branch mispredictions"
)]
pub static CPU_BRANCH_MISSES: CounterGroup = CounterGroup::new(MAX_CPUS);
