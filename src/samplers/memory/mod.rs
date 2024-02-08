use crate::*;

sampler!(Memory, "memory", MEMORY_SAMPLERS);

#[cfg(target_os = "linux")]
mod stats;

#[cfg(target_os = "linux")]
mod linux;
