use crate::common::HISTOGRAM_GROUPING_POWER;
use metriken::*;

#[metric(
    name = "metadata/scheduler_runqueue/collected_at",
    description = "The offset from the Unix epoch when scheduler_runqueue sampler was last run",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_SCHEDULER_RUNQUEUE_COLLECTED_AT: LazyCounter =
    LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/scheduler_runqueue/runtime",
    description = "The total runtime of the scheduler_runqueue sampler",
    metadata = { unit = "nanoseconds" }
)]
pub static METADATA_SCHEDULER_RUNQUEUE_RUNTIME: LazyCounter = LazyCounter::new(Counter::default);

#[metric(
    name = "metadata/scheduler_runqueue/runtime",
    description = "Distribution of sampling runtime of the scheduler_runqueue sampler",
    metadata = { unit = "nanoseconds/second" }
)]
pub static METADATA_SCHEDULER_RUNQUEUE_RUNTIME_HISTOGRAM: AtomicHistogram =
    AtomicHistogram::new(4, 32);

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
    name = "scheduler/offcpu",
    description = "Distribution of the amount of time tasks were off-CPU",
    metadata = { unit = "nanoseconds" }
)]
pub static SCHEDULER_OFFCPU: RwLockHistogram = RwLockHistogram::new(HISTOGRAM_GROUPING_POWER, 64);

#[metric(
    name = "scheduler/context_switch/involuntary",
    description = "The number of involuntary context switches"
)]
pub static SCHEDULER_IVCSW: LazyCounter = LazyCounter::new(Counter::default);
