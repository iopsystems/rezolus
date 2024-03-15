use crate::*;

sampler!(BlockIO, "block_io", BLOCK_IO_SAMPLERS);

#[cfg(target_os = "linux")]
mod stats;

#[cfg(target_os = "linux")]
mod linux;
