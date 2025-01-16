use metriken::*;

#[metric(
    name = "rezolus_cpu_usage",
    description = "The amount of CPU time Rezolus was executing in user mode",
    metadata = { mode = "user", unit = "nanoseconds" }
)]
pub static RU_UTIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_cpu_usage",
    description = "The amount of CPU time Rezolus was executing in system mode",
    metadata = { mode = "system", unit = "nanoseconds" }
)]
pub static RU_STIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_memory_usage_resident_set_size",
    description = "The total amount of memory allocated by Rezolus",
    metadata = { unit = "bytes" }
)]
pub static RU_MAXRSS: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "rezolus_memory_page_reclaims",
    description = "The number of page faults which were serviced by reclaiming a page"
)]
pub static RU_MINFLT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_memory_page_faults",
    description = "The number of page faults which required an I/O operation"
)]
pub static RU_MAJFLT: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_blockio_operations",
    description = "The number of reads from the filesystem",
    metadata = { op = "read", unit = "operations" }
)]
pub static RU_INBLOCK: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_blockio_operations",
    description = "The number of writes to the filesystem",
    metadata = { op = "write", unit = "operations" }
)]
pub static RU_OUBLOCK: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_context_switch",
    description = "The number of voluntary context switches",
    metadata = { kind = "voluntary" }
)]
pub static RU_NVCSW: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "rezolus_context_switch",
    description = "The number of involuntary context switches",
    metadata = { kind = "involuntary" }
)]
pub static RU_NIVCSW: LazyCounter = LazyCounter::new(Counter::default);
