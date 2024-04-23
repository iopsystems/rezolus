use crate::*;

sampler!(Filesystem, "filesystem", FILESYSTEM_SAMPLERS);

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::stats::*;
