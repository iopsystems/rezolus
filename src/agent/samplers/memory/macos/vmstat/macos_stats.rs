//! macOS memory metrics with no Linux equivalent that come from
//! `host_statistics64`, in the same acquisition group as the shared counters.
//! `memory_wired` is a gauge; it is here because no sysctl reports it.

use metriken::*;

#[metric(
    name = "memory_wired",
    description = "Memory wired down: it cannot be paged out or compressed (wire_count, which on macOS also counts throttled pages and free low-memory pages)",
    metadata = { unit = "bytes", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_WIRED: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_compressions",
    description = "Pages compressed into the compressor",
    metadata = { unit = "pages", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_COMPRESSIONS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_decompressions",
    description = "Pages decompressed out of the compressor",
    metadata = { unit = "pages", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_DECOMPRESSIONS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_pageins",
    description = "File pages read from disk on a fault or by read-ahead; pages a read() call fetches directly are not counted",
    metadata = { unit = "pages", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_PAGEINS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_pageouts",
    description = "Dirty file pages the pageout daemon wrote back to free memory; write(), fsync and msync writeback are not counted",
    metadata = { unit = "pages", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_PAGEOUTS: LazyCounter = LazyCounter::new(Counter::default);
