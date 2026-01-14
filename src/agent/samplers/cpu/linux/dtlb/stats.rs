use metriken::*;

use crate::agent::*;

// per-CPU metrics

#[metric(
    name = "cpu_dtlb_load_miss",
    description = "The number of DTLB load misses"
)]
pub static CPU_DTLB_LOAD_MISS: CounterGroup = CounterGroup::new(MAX_CPUS);

#[metric(
    name = "cpu_dtlb_store_miss",
    description = "The number of DTLB store misses"
)]
pub static CPU_DTLB_STORE_MISS: CounterGroup = CounterGroup::new(MAX_CPUS);
