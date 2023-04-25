use crate::*;

sampler!(Cpu, "cpu", CPU_SAMPLERS);

mod stats;

#[cfg(target_os = "linux")]
mod cpi;

mod proc_cpuinfo;
mod proc_stat;
