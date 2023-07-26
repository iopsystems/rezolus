use crate::*;

sampler!(Scheduler, "scheduler", SCHEDULER_SAMPLERS);

mod stats;

#[cfg(all(feature = "bpf", target_os = "linux"))]
mod runqueue;
