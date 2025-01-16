use metriken::*;

#[metric(
    name = "cpu_cores",
    description = "The total number of logical cores that are currently online"
)]
pub static CPU_CORES: LazyGauge = LazyGauge::new(Gauge::default);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent busy",
    metadata = { state = "busy", unit = "nanoseconds" }
)]
pub static CPU_USAGE_BUSY: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent executing normal tasks is user mode",
    metadata = { state = "user", unit = "nanoseconds" }
)]
pub static CPU_USAGE_USER: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent executing low priority tasks in user mode",
    metadata = { state = "nice", unit = "nanoseconds" }
)]
pub static CPU_USAGE_NICE: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "cpu_usage",
    description = "The amount of CPU time spent executing tasks in kernel mode",
    metadata = { state = "system", unit = "nanoseconds" }
)]
pub static CPU_USAGE_SYSTEM: LazyCounter = LazyCounter::new(Counter::default);
