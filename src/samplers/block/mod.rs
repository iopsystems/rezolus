use crate::*;

sampler!(Block, "block", BLOCK_SAMPLERS);

#[cfg(target_os = "linux")]
mod stats;

#[cfg(target_os = "linux")]
mod linux;
