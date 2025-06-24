//! Collects Network Traffic stats using BPF and traces:
//!
//! And produces these stats:
//!

const NAME: &str = "network_interfaces";

#[allow(clippy::module_inception)]
mod bpf {
    include!(concat!(env!("OUT_DIR"), "/network_interfaces.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let counters = vec![
        &NETWORK_DROP,
        &NETWORK_TX_BUSY,
        &NETWORK_TX_COMPLETE,
        &NETWORK_TX_TIMEOUT,
    ];

    let bpf = BpfBuilder::new(
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .counters("counters", counters)
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
            "{NAME} kfree_skb() BPF instruction count: {}",
            self.progs.kfree_skb.insn_cnt()
        );
        debug!(
            "{NAME} net_dev_xmit() BPF instruction count: {}",
            self.progs.net_dev_xmit.insn_cnt()
        );
    }
}
