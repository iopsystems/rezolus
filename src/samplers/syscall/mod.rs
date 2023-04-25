use crate::*;

sampler!(Syscall, "syscall", SYSCALL_SAMPLERS);

mod stats;

#[cfg(feature = "bpf")]
mod latency;
