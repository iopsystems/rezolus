use crate::*;

sampler!(BlockIO, "block_io", BLOCK_IO_SAMPLERS);

mod stats;

#[cfg(target_os = "linux")]
mod linux;
