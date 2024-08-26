use metriken::*;

#[metric(
    name = "metadata/memory_meminfo/collected_at",
    description = "The offset from the Unix epoch when memory_meminfo sampler was last run",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_MEMORY_MEMINFO_COLLECTED_AT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/memory_meminfo/runtime",
    description = "The total runtime of the memory_meminfo sampler",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_MEMORY_MEMINFO_RUNTIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/memory_meminfo/runtime",
    description = "Distribution of sampling runtime of the memory_meminfo sampler",
    metadata = { unit = "nanoseconds/second" }
)]
pub static METADATA_MEMORY_MEMINFO_RUNTIME_HISTOGRAM: AtomicHistogram = AtomicHistogram::new(4, 32);

#[metric(
    name = "metadata/memory_vmstat/collected_at",
    description = "The offset from the Unix epoch when memory_vmstat sampler was last run",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_MEMORY_VMSTAT_COLLECTED_AT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/memory_vmstat/runtime",
    description = "The total runtime of the memory_vmstat sampler",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_MEMORY_VMSTAT_RUNTIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/memory_vmstat/runtime",
    description = "Distribution of sampling runtime of the memory_vmstat sampler",
    metadata = { unit = "nanoseconds/second" }
)]
pub static METADATA_MEMORY_VMSTAT_RUNTIME_HISTOGRAM: AtomicHistogram = AtomicHistogram::new(4, 32);

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
