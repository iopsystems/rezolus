//! macOS memory gauges with no Linux equivalent, in the same acquisition
//! group as the shared ones.

use metriken::*;

#[metric(
    name = "memory_purgeable",
    description = "Unwired anonymous memory in objects marked volatile, which the kernel may discard without saving; counted in memory_anon and memory_available",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_PURGEABLE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_speculative",
    description = "File pages read ahead and not yet used; counted in memory_cached and memory_available",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_SPECULATIVE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_compressed",
    description = "Compressed data the compressor holds in memory, in bytes; excludes segments swapped out and unused space within segments",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_COMPRESSED: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_compressed_stored",
    description = "Uncompressed size of every page the compressor holds, including pages in segments swapped out (vm_stat's Pages stored in compressor)",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_COMPRESSED_STORED: LazyGauge = LazyGauge::new(Gauge::default);
