use crate::*;

sampler!(Scheduler, "scheduler", SCHEDULER_SAMPLERS);

mod stats;

#[cfg(feature = "bpf")]
mod runqueue;
