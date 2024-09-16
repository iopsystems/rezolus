/// Collects Network Traffic stats using BPF and traces:
/// * `netif_receive_skb`
/// * `netdev_start_xmit`
///
/// And produces these stats:
/// * `network/receive/bytes`
/// * `network/receive/frames`
/// * `network/transmit/bytes`
/// * `network/transmit/frames`

const NAME: &str = "network_traffic";

#[allow(clippy::module_inception)]
mod bpf {
    include!(concat!(env!("OUT_DIR"), "/network_traffic.bpf.rs"));
}

use bpf::*;

use crate::common::*;
use crate::samplers::network::linux::stats::*;
use crate::*;

use std::sync::Arc;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let counters = vec![
        &NETWORK_RX_BYTES,
        &NETWORK_TX_BYTES,
        &NETWORK_RX_PACKETS,
        &NETWORK_TX_PACKETS,
    ];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
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
            "{NAME} netif_receive_skb() BPF instruction count: {}",
            self.progs.netif_receive_skb.insn_cnt()
        );
        debug!(
            "{NAME} tcp_cleanup_rbuf() BPF instruction count: {}",
            self.progs.tcp_cleanup_rbuf.insn_cnt()
        );
    }
}
