use crate::*;

sampler!(Cpu, "cpu", CPU_SAMPLERS);

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod stats;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use stats::*;

#[cfg(target_os = "linux")]
pub use linux::stats::*;
