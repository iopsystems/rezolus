use crate::*;

sampler!(Syscall, "syscall", SYSCALL_SAMPLERS);

#[cfg(target_os = "linux")]
mod stats;

#[cfg(target_os = "linux")]
mod linux;
