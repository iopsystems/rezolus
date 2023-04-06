use crate::*;

sampler!(BlockIO, "block_io", BLOCK_IO_SAMPLERS);

#[cfg(feature = "bpf")]
mod latency;

mod stats;
