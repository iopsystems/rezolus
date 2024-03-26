use crate::common::Nop;
use crate::samplers::cpu::CPU_SAMPLERS;
use crate::{distributed_slice, Config, Sampler};

const NAME: &str = "cpu_usage";

#[cfg(feature = "bpf")]
mod bpf;

mod proc_stat;

#[cfg(feature = "bpf")]
use bpf::*;

use proc_stat::*;

#[cfg(feature = "bpf")]
#[distributed_slice(CPU_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    // try to initialize the bpf based sampler
    if let Ok(s) = CpuUsage::new(config) {
        Box::new(s)
    // try to fallback to the /proc/stat based sampler if there was an error
    } else if let Ok(s) = ProcStat::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

#[cfg(not(feature = "bpf"))]
#[distributed_slice(CPU_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    // try to use the /proc/stat based sampler since BPF was not enabled for
    // this build
    if let Ok(s) = ProcStat::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}
