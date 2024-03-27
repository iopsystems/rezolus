use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::{metric, AtomicHistogram, Counter, Gauge, LazyCounter, LazyGauge};

#[metric(
    name = "rezolus/cpu/usage/user",
    description = "The amount of CPU time Rezolus was executing in user mode",
    metadata = { unit = "nanoseconds" }
)]
pub static RU_UTIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus/cpu/usage/user",
    description = "Distribution of the rate of CPU usage for Rezolus executing in user mode",
    metadata = { unit = "nanoseconds/second" }
)]
pub static RU_UTIME_HISTOGRAM: AtomicHistogram = AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "rezolus/cpu/usage/system",
    description = "The amount of CPU time Rezolus was executing in system mode",
    metadata = { unit = "nanoseconds" }
)]
pub static RU_STIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus/cpu/usage/system",
    description = "Distribution of the rate of CPU usage for Rezolus executing in system mode",
    metadata = { unit = "nanoseconds/second" }
)]
pub static RU_STIME_HISTOGRAM: AtomicHistogram = AtomicHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "rezolus/memory/usage/resident_set_size",
    description = "The total amount of memory allocated by Rezolus",
    metadata = { unit = "bytes" }
)]
pub static RU_MAXRSS: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "rezolus/memory/page/reclaims",
    description = "The number of page faults which were serviced by reclaiming a page"
)]
pub static RU_MINFLT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus/memory/page/faults",
    description = "The number of page faults which required an I/O operation"
)]
pub static RU_MAJFLT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus/blockio/read",
    description = "The number of reads from the filesystem",
    metadata = { unit = "operations" }
)]
pub static RU_INBLOCK: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus/blockio/write",
    description = "The number of writes to the filesystem",
    metadata = { unit = "operations" }
)]
pub static RU_OUBLOCK: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus/context_switch/voluntary",
    description = "The number of voluntary context switches"
)]
pub static RU_NVCSW: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus/context_switch/involuntary",
    description = "The number of involuntary context switches"
)]
pub static RU_NIVCSW: LazyCounter = LazyCounter::new(Counter::default);
