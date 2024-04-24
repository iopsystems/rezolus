use crate::common::HISTOGRAM_GROUPING_POWER;
use crate::*;
use metriken::{metric, AtomicHistogram, Counter, Gauge, LazyCounter, LazyGauge};

#[metric(
    name = "filesystem/descriptors/open",
    description = "The number of file descriptors currently allocated"
)]
pub static FILESYSTEM_DESCRIPTORS_OPEN: LazyGauge = LazyGauge::new(Gauge::default);
