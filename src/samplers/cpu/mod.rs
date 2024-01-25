use crate::*;

sampler!(Cpu, "cpu", CPU_SAMPLERS);

mod stats;

#[cfg(target_os = "linux")]
mod linux;
