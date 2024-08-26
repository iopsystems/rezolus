use metriken::*;

#[metric(
    name = "metadata/filesystem_descriptors/collected_at",
    description = "The offset from the Unix epoch when filesystem_descriptors sampler was last run",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_FILESYSTEM_DESCRIPTORS_COLLECTED_AT: LazyCounter =
    LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/filesystem_descriptors/runtime",
    description = "The total runtime of the filesystem_descriptors sampler",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_FILESYSTEM_DESCRIPTORS_RUNTIME: LazyCounter =
    LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/filesystem_descriptors/runtime",
    description = "Distribution of sampling runtime of the filesystem_descriptors sampler",
    metadata = { unit = "nanoseconds/second" }
)]
pub static METADATA_FILESYSTEM_DESCRIPTORS_RUNTIME_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(4, 32);

#[metric(
    name = "filesystem/descriptors/open",
    description = "The number of file descriptors currently allocated"
)]
pub static FILESYSTEM_DESCRIPTORS_OPEN: LazyGauge = LazyGauge::new(Gauge::default);
