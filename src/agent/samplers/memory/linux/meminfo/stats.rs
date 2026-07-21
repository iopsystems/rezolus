use metriken::*;

#[metric(
    name = "memory_total",
    description = "The total amount of system memory",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_TOTAL: WindowedLazyGauge = WindowedLazyGauge::new(Gauge::default);

#[metric(
    name = "memory_free",
    description = "The amount of system memory that is currently free",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_FREE: WindowedLazyGauge = WindowedLazyGauge::new(Gauge::default);

#[metric(
    name = "memory_available",
    description = "The amount of system memory that is available for allocation",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_AVAILABLE: WindowedLazyGauge = WindowedLazyGauge::new(Gauge::default);

#[metric(
    name = "memory_buffers",
    description = "The amount of system memory used for buffers",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_BUFFERS: WindowedLazyGauge = WindowedLazyGauge::new(Gauge::default);

#[metric(
    name = "memory_cached",
    description = "The amount of system memory used by the page cache",
    metadata = { unit = "bytes" }
)]
pub static MEMORY_CACHED: WindowedLazyGauge = WindowedLazyGauge::new(Gauge::default);
