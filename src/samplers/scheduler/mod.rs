use crate::*;

sampler!(Scheduler, "scheduler", SCHEDULER_SAMPLERS);

mod stats;

#[cfg(target_os = "linux")]
mod linux;
