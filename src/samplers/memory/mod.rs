use crate::*;

sampler!(Memory, "memory", MEMORY_SAMPLERS);

mod stats;

#[cfg(target_os = "linux")]
mod proc_meminfo;
#[cfg(target_os = "linux")]
mod proc_vmstat;
