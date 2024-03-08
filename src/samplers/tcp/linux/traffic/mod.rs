use crate::common::Nop;
use crate::samplers::tcp::*;

const NAME: &str = "tcp_traffic";

#[cfg(feature = "bpf")]
mod bpf;

mod proc;

#[cfg(feature = "bpf")]
use bpf::*;

use proc::*;

#[cfg(feature = "bpf")]
#[distributed_slice(TCP_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    // try to initialize the bpf based sampler
    if let Ok(s) = TcpTraffic::new(config) {
        Box::new(s)
    // try to fallback to the /proc/net/snmp based sampler if there was an error
    } else if let Ok(s) = ProcNetSnmp::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

#[cfg(not(feature = "bpf"))]
#[distributed_slice(TCP_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    // try to use the /proc/net/snmp based sampler since BPF was not enabled for
    // this build
    if let Ok(s) = ProcNetSnmp::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}
