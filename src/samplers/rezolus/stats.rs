use crate::*;
use metriken::metric;
use metriken::Counter;
use metriken::Gauge;
use metriken::LazyCounter;

#[metric(
    name = "rezolus/cpu/usage/user",
    description = "The amount of CPU time Rezolus was executing in user mode"
)]
pub static RU_UTIME: LazyCounter = LazyCounter::new(Counter::default);

histogram!(
    RU_UTIME_HISTOGRAM,
    "rezolus/cpu/usage/user",
    "The amount of CPU time Rezolus was executing in user mode"
);

#[metric(
    name = "rezolus/cpu/usage/system",
    description = "The amount of CPU time Rezolus was executing in system mode"
)]
pub static RU_STIME: LazyCounter = LazyCounter::new(Counter::default);

histogram!(
    RU_STIME_HISTOGRAM,
    "rezolus/cpu/usage/system",
    "The amount of CPU time Rezolus was executing in system mode"
);

#[metric(
    name = "rezolus/memory/usage/resident_set_size",
    description = "The total amount of memory allocated by Rezolus"
)]
pub static RU_MAXRSS: Gauge = Gauge::new();

#[metric(name = "rezolus/memory/usage/shared_memory_size")]
pub static RU_IXRSS: Gauge = Gauge::new();

#[metric(name = "rezolus/memory/usage/data_size")]
pub static RU_IDRSS: Gauge = Gauge::new();

#[metric(name = "rezolus/memory/usage/stack_size")]
pub static RU_ISRSS: Gauge = Gauge::new();

#[metric(name = "rezolus/memory/page/reclaims")]
pub static RU_MINFLT: Counter = Counter::new();

#[metric(name = "rezolus/memory/page/faults")]
pub static RU_MAJFLT: Counter = Counter::new();

#[metric(name = "rezolus/memory/page/swaps")]
pub static RU_NSWAP: Counter = Counter::new();

#[metric(name = "rezolus/io/block/reads")]
pub static RU_INBLOCK: Counter = Counter::new();

#[metric(name = "rezolus/io/block/writes")]
pub static RU_OUBLOCK: Counter = Counter::new();

#[metric(name = "rezolus/messages/sent")]
pub static RU_MSGSND: Counter = Counter::new();

#[metric(name = "rezolus/messages/received")]
pub static RU_MSGRCV: Counter = Counter::new();

#[metric(name = "rezolus/signals/received")]
pub static RU_NSIGNALS: Counter = Counter::new();

#[metric(name = "rezolus/context_switch/voluntary")]
pub static RU_NVCSW: Counter = Counter::new();

#[metric(name = "rezolus/context_switch/involuntary")]
pub static RU_NIVCSW: Counter = Counter::new();
