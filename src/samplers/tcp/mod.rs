use crate::*;

sampler!(Tcp, "tcp", TCP_SAMPLERS);

#[cfg(target_os = "linux")]
mod stats;

#[cfg(target_os = "linux")]
mod linux;
