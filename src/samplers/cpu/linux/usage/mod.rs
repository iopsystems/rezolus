use crate::common::Nop;
use crate::*;

const NAME: &str = "cpu_usage";

mod bpf;

use bpf::*;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> Box<dyn Sampler> {
    if let Ok(s) = CpuUsage::new(config.clone()) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}
