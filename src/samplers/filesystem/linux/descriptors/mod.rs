use crate::common::Nop;
use crate::samplers::filesystem::FILESYSTEM_SAMPLERS;
use crate::{distributed_slice, Config, Sampler};

const NAME: &str = "filesystem_descriptors";

mod procfs;

use procfs::*;

#[distributed_slice(FILESYSTEM_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = Procfs::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}
