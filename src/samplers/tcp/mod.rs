use crate::*;

sampler!(Tcp, "tcp", TCP_SAMPLERS);

mod stats;

#[cfg(target_os = "linux")]
mod linux;
