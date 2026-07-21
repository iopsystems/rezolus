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
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .counters("counters", counters)
    // Per-driver tx_timeout kprobes: only the driver(s) bound to present NICs
    // should attach. Keys are the BPF program (C function) names, which differ
    // from the SEC() kprobe targets for virtio/mlx4/mlx5.
    .driver_programs(&[
        ("virtio_tx_timeout", "virtio_net"),
        ("ena_tx_timeout", "ena"),
        ("gve_tx_timeout", "gve"),
        ("mlx4_tx_timeout", "mlx4_en"),
        ("mlx5_tx_timeout", "mlx5_core"),
        ("e1000_tx_timeout", "e1000e"),
        ("igb_tx_timeout", "igb"),
        ("ixgbe_tx_timeout", "ixgbe"),
        ("i40e_tx_timeout", "i40e"),
        ("ice_tx_timeout", "ice"),
        ("bnxt_tx_timeout", "bnxt_en"),
        ("tg3_tx_timeout", "tg3"),
    ])
    .build()?;

    Ok(Some(Box::new(bpf)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map<'_> {
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
