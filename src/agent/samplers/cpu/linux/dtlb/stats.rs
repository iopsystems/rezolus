use metriken::*;

use crate::agent::*;

// per-CPU metrics

/// DTLB misses without op label - used on AMD/ARM where load and store
/// misses are reported as a single combined event
#[metric(
    name = "cpu_dtlb_miss",
    description = "The number of DTLB misses"
)]
pub static CPU_DTLB_MISS: CounterGroup = CounterGroup::new(MAX_CPUS);

/// DTLB load misses - Intel only
#[metric(
    name = "cpu_dtlb_miss",
    description = "The number of DTLB load misses",
    metadata = { op = "load" }
)]
pub static CPU_DTLB_MISS_LOAD: CounterGroup = CounterGroup::new(MAX_CPUS);

/// DTLB store misses - Intel only
#[metric(
    name = "cpu_dtlb_miss",
    description = "The number of DTLB store misses",
    metadata = { op = "store" }
)]
pub static CPU_DTLB_MISS_STORE: CounterGroup = CounterGroup::new(MAX_CPUS);
