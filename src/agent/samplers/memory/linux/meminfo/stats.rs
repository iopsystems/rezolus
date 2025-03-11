use metriken::*;

#[metric(
    name = "memory_total",
    description = "The total amount of system memory",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_TOTAL: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_free",
    description = "The amount of system memory that is currently free",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_FREE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_available",
    description = "The amount of system memory that is available for allocation",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_AVAILABLE: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_buffers",
    description = "The amount of system memory used for buffers",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_BUFFERS: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "memory_cached",
    description = "The amount of system memory used by the page cache",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_CACHED: LazyGauge = LazyGauge::new(Gauge::default);
