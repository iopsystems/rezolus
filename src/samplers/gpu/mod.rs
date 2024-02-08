use crate::*;

sampler!(Gpu, "gpu", GPU_SAMPLERS);

#[cfg(target_os = "linux")]
mod stats;

#[cfg(target_os = "linux")]
mod nvidia;
