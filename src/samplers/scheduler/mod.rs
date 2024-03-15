use crate::*;

sampler!(Scheduler, "scheduler", SCHEDULER_SAMPLERS);

#[cfg(target_os = "linux")]
mod stats;

#[cfg(target_os = "linux")]
mod linux;
