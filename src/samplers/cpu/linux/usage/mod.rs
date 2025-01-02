/// Collects CPU usage stats using BPF and traces:
/// * `cpuacct_account_field`
///
/// And produces these stats:
/// * `cpu_usage/busy`
/// * `cpu_usage/user`
/// * `cpu_usage/nice`
/// * `cpu_usage/system`
/// * `cpu_usage/softirq`
/// * `cpu_usage/irq`
/// * `cpu_usage/steal`
/// * `cpu_usage/guest`
/// * `cpu_usage/guest_nice`

const NAME: &str = "cpu_usage";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_usage.bpf.rs"));
}

use bpf::*;

use crate::common::*;
use crate::*;

use std::sync::Arc;

mod stats;

use stats::*;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let counters = vec![
        &CPU_USAGE_BUSY,
        &CPU_USAGE_USER,
        &CPU_USAGE_NICE,
        &CPU_USAGE_SYSTEM,
        &CPU_USAGE_SOFTIRQ,
        &CPU_USAGE_IRQ,
        &CPU_USAGE_STEAL,
        &CPU_USAGE_GUEST,
        &CPU_USAGE_GUEST_NICE,
    ];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .cpu_counters("counters", counters)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "counters" => &self.maps.counters,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} cpuacct_account_field() BPF instruction count: {}",
            self.progs.cpuacct_account_field_kprobe.insn_cnt()
        );
    }
}
