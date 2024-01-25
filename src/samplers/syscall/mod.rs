use crate::*;

sampler!(Syscall, "syscall", SYSCALL_SAMPLERS);

mod stats;

#[cfg(target_os = "linux")]
mod linux;
