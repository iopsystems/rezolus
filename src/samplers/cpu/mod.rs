use crate::*;

sampler!(Cpu, "cpu", CPU_SAMPLERS);

#[cfg(target_os = "linux")]
mod stats;

#[cfg(target_os = "linux")]
mod linux;
