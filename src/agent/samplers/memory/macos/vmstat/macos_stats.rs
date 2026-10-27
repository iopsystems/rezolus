//! macOS memory counters with no Linux equivalent, in the same acquisition
//! group as the shared ones.

use metriken::*;

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
    description = "Pages read in from files by the pager",
    metadata = { unit = "pages", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_PAGEINS: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "memory_pageouts",
    description = "Pages written out to files by the pager",
    metadata = { unit = "pages", acq_group = "memory_vmstat_read" }
)]
pub static MEMORY_PAGEOUTS: LazyCounter = LazyCounter::new(Counter::default);
