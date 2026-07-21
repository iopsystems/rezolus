use metriken::*;

#[metric(
    name = "memory_numa_hit",
    description = "The number of allocations that succeeded on the intended node"
)]
pub static MEMORY_NUMA_HIT: WindowedLazyCounter = WindowedLazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_miss",
    description = "The number of allocations that did not succeed on the intended node"
)]
pub static MEMORY_NUMA_MISS: WindowedLazyCounter = WindowedLazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_foreign",
    description = "The number of allocations that were not intended for a node that were serviced by this node"
)]
pub static MEMORY_NUMA_FOREIGN: WindowedLazyCounter = WindowedLazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_interleave",
    description = "The number of interleave policy allocations that succeeded on the intended node"
)]
pub static MEMORY_NUMA_INTERLEAVE: WindowedLazyCounter = WindowedLazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_local",
    description = "The number of allocations that succeeded on the local node"
)]
pub static MEMORY_NUMA_LOCAL: WindowedLazyCounter = WindowedLazyCounter::new(Counter::default);

#[metric(
    name = "memory_numa_other",
    description = "The number of allocations that on this node that were allocated by a process on another node"
)]
pub static MEMORY_NUMA_OTHER: WindowedLazyCounter = WindowedLazyCounter::new(Counter::default);
