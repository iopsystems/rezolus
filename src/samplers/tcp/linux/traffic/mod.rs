/// Collects TCP stats using BPF and traces:
/// * `tcp_sendmsg`
/// * `tcp_cleanup_rbuf`
///
/// And produces these stats:
/// * `tcp/receive/bytes`
/// * `tcp/receive/packets`
/// * `tcp/receive/size`
/// * `tcp/transmit/bytes`
/// * `tcp/transmit/packets`
/// * `tcp/transmit/size`

const NAME: &str = "tcp_traffic";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_traffic.bpf.rs"));
}

use bpf::*;

use crate::common::*;
use crate::samplers::tcp::linux::stats::*;
use crate::*;

use std::sync::Arc;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let counters = vec![
        &TCP_RX_BYTES,
        &TCP_TX_BYTES,
        &TCP_RX_PACKETS,
        &TCP_TX_PACKETS,
    ];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .counters("counters", counters)
        .histogram("rx_size", &TCP_RX_SIZE)
        .histogram("tx_size", &TCP_TX_SIZE)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "counters" => &self.maps.counters,
            "rx_size" => &self.maps.rx_size,
            "tx_size" => &self.maps.tx_size,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} tcp_sendmsg() BPF instruction count: {}",
            self.progs.tcp_sendmsg.insn_cnt()
        );
        debug!(
            "{NAME} tcp_cleanup_rbuf() BPF instruction count: {}",
            self.progs.tcp_cleanup_rbuf.insn_cnt()
        );
    }
}
