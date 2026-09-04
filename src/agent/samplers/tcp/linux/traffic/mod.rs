//! Collects TCP stats using BPF and traces `tcp_sendmsg` and `tcp_cleanup_rbuf`,
//! via fentry when the kernel has BTF (cheaper dispatch) and kprobe otherwise
//! (the CO-RE-only fallback).
//!
//! And produces these stats:
//! * `tcp/receive/bytes`
//! * `tcp/receive/packets`
//! * `tcp/receive/size`
//! * `tcp/transmit/bytes`
//! * `tcp/transmit/packets`
//! * `tcp/transmit/size`

const NAME: &str = "tcp_traffic";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_traffic.bpf.rs"));
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
        &TCP_RX_BYTES,
        &TCP_TX_BYTES,
        &TCP_RX_PACKETS,
        &TCP_TX_PACKETS,
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
    .counters("counters", counters, &COUNTERS_ACQ)
    // Both size histograms share ONE group — see stats.rs's `SIZES_ACQ`
    // doc comment.
    .histogram("rx_size", &TCP_RX_SIZE, &SIZES_ACQ)
    .histogram("tx_size", &TCP_TX_SIZE, &SIZES_ACQ)
    // Prefer fentry (cheaper dispatch); fall back to kprobe without BTF.
    .disabled_programs(if kernel_has_btf() {
        &["tcp_sendmsg_kprobe", "tcp_cleanup_rbuf_kprobe"]
    } else {
        &["tcp_sendmsg_fentry", "tcp_cleanup_rbuf_fentry"]
    })
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
            "rx_size" => &self.maps.rx_size,
            "tx_size" => &self.maps.tx_size,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} tcp_sendmsg_fentry() BPF instruction count: {}",
            self.progs.tcp_sendmsg_fentry.insn_cnt()
        );
        debug!(
            "{NAME} tcp_cleanup_rbuf_fentry() BPF instruction count: {}",
            self.progs.tcp_cleanup_rbuf_fentry.insn_cnt()
        );
    }
}
