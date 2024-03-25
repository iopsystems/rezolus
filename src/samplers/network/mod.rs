use crate::*;

sampler!(Network, "network", NETWORK_SAMPLERS);

#[cfg(target_os = "linux")]
pub(crate) mod stats;

#[cfg(target_os = "linux")]
mod linux;
