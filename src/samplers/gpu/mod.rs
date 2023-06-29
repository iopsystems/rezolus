use crate::*;

sampler!(Gpu, "gpu", GPU_SAMPLERS);

mod stats;

#[cfg(target_os = "linux")]
mod nvidia;
