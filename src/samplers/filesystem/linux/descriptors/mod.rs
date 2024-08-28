use crate::common::Nop;
use crate::*;

const NAME: &str = "filesystem_descriptors";

mod procfs;

use procfs::*;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> Box<dyn Sampler> {
    if let Ok(s) = Procfs::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}
