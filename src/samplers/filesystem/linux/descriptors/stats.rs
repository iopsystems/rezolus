#[metric(
    name = "filesystem_descriptors_open",
    description = "The number of file descriptors currently allocated"
)]
pub static FILESYSTEM_DESCRIPTORS_OPEN: LazyGauge = LazyGauge::new(Gauge::default);
