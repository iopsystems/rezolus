use crate::*;

sampler!(Syscall, "syscall", SYSCALL_SAMPLERS);

mod stats;

#[cfg(all(feature = "bpf", target_os = "linux"))]
mod latency;
