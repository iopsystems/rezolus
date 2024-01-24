use crate::*;

sampler!(Memory, "memory", MEMORY_SAMPLERS);

mod stats;

#[cfg(target_os = "linux")]
#[path = "."]
mod linux {
    mod proc_meminfo;
    mod proc_vmstat;
}
