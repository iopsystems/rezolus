use metriken::{metric, Counter, Gauge, LazyCounter, LazyGauge};

#[metric(
    name = "memory/total",
    description = "The total amount of system memory",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_TOTAL: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory/free",
    description = "The amount of system memory that is currently free",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_FREE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory/available",
    description = "The amount of system memory that is available for allocation",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_AVAILABLE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory/buffers",
    description = "The amount of system memory used for buffers",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_BUFFERS: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory/cached",
    description = "The amount of system memory used by the page cache",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_CACHED: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory/numa/hit",
    description = "The number of allocations that succeeded on the intended node"
)]
pub static MEMORY_NUMA_HIT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory/numa/miss",
    description = "The number of allocations that did not succeed on the intended node"
)]
pub static MEMORY_NUMA_MISS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory/numa/foreign",
    description = "The number of allocations that were not intended for a node that were serviced by this node"
)]
pub static MEMORY_NUMA_FOREIGN: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory/numa/interleave",
    description = "The number of interleave policy allocations that succeeded on the intended node"
)]
pub static MEMORY_NUMA_INTERLEAVE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory/numa/local",
    description = "The number of allocations that succeeded on the local node"
)]
pub static MEMORY_NUMA_LOCAL: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory/numa/other",
    description = "The number of allocations that on this node that were allocated by a process on another node"
)]
pub static MEMORY_NUMA_OTHER: LazyCounter = LazyCounter::new(Counter::default);
