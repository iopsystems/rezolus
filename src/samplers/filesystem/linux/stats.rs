use metriken::{metric, Gauge, LazyGauge};

#[metric(
    name = "filesystem/descriptors/open",
    description = "The number of file descriptors currently allocated"
)]
pub static FILESYSTEM_DESCRIPTORS_OPEN: LazyGauge = LazyGauge::new(Gauge::default);
