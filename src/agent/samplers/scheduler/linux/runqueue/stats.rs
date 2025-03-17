use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

#[metric(
    name = "scheduler_runqueue_latency",
    description = "Distribution of the amount of time tasks were waiting in the runqueue",
    metadata = { unit = "nanoseconds" }
)]
pub static SCHEDULER_RUNQUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_running",
    description = "Distribution of the amount of time tasks were on-CPU",
    metadata = { unit = "nanoseconds" }
)]
pub static SCHEDULER_RUNNING: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_offcpu",
    description = "Distribution of the amount of time tasks were off-CPU",
    metadata = { unit = "nanoseconds" }
)]
pub static SCHEDULER_OFFCPU: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler_context_switch",
    description = "The number of involuntary context switches",
    metadata = { kind = "involuntary" }
)]
pub static SCHEDULER_IVCSW: LazyCounter = LazyCounter::new(Counter::default);
