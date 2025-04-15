use metriken::*;

use crate::agent::*;

// per-CPU metrics

#[metric(name = "cpu_l3_access", description = "The number of L3 cache access")]
pub static CPU_L3_ACCESS: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(name = "cpu_l3_miss", description = "The number of L3 cache miss")]
pub static CPU_L3_MISS: CounterGroup = CounterGroup::new(MAX_CPUS);
