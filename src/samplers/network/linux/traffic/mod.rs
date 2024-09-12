use crate::common::*;
use crate::samplers::network::linux::*;

const NAME: &str = "network_traffic";

mod bpf;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> Box<dyn Sampler> {
    if let Ok(s) = bpf::NetworkTraffic::new(config.clone()) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}
