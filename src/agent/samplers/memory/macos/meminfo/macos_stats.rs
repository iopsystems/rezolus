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
    description = "Compressed data the compressor holds, in bytes",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_COMPRESSED: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_compressed_stored",
    description = "Uncompressed size of the memory the compressor holds; its ratio to memory_compressed is the compression ratio",
    metadata = { unit = "bytes", acq_group = "memory_meminfo_read" }
)]
pub static MEMORY_COMPRESSED_STORED: LazyGauge = LazyGauge::new(Gauge::default);
