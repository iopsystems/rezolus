use crate::*;

sampler!(BlockIO, "block_io", BLOCK_IO_SAMPLERS);

#[cfg(all(feature = "bpf", target_os = "linux"))]
mod latency;

mod stats;
