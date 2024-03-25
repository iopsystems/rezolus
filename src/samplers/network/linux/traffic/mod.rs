use crate::common::Nop;
use crate::samplers::network::*;

const NAME: &str = "network_traffic";

#[cfg(feature = "bpf")]
mod bpf;

#[cfg(feature = "bpf")]
use bpf::*;

#[cfg(feature = "bpf")]
#[distributed_slice(NETWORK_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    // try to initialize the bpf based sampler
    if let Ok(s) = NetworkTraffic::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

#[cfg(not(feature = "bpf"))]
#[distributed_slice(NETWORK_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    Box::new(Nop {})
}
