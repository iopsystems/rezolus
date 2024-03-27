use crate::common::HISTOGRAM_GROUPING_POWER;
use crate::*;
use metriken::{metric, Counter, LazyCounter, RwLockHistogram};

#[metric(
    name = "scheduler/runqueue/latency",
    description = "Distribution of the amount of time tasks were waiting in the runqueue",
    metadata = { unit = "nanoseconds" }
)]
pub static SCHEDULER_RUNQUEUE_LATENCY: RwLockHistogram =
    RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler/running",
    description = "Distribution of the amount of time tasks were on-CPU",
    metadata = { unit = "nanoseconds" }
)]
pub static SCHEDULER_RUNNING: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler/context_switch/involuntary",
    description = "The number of involuntary context switches"
)]
pub static SCHEDULER_IVCSW: LazyCounter = LazyCounter::new(Counter::default);
