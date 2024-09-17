/// Collects TCP packet latency stats using BPF and traces:
/// * `tcp_v4_connect`
/// * `tcp_v6_connect`
/// * `tcp_rcv_state_process`
/// * `tcp_destroy_sock`
///
/// And produces these stats:
/// * `tcp/connect_latency`

const NAME: &str = "tcp_connect_latency";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_connect_latency.bpf.rs"));
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

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .histogram("latency", &TCP_CONNECT_LATENCY)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "latency" => &self.maps.latency,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} tcp_v4_connect() BPF instruction count: {}",
            self.progs.tcp_v4_connect.insn_cnt()
        );
        debug!(
            "{NAME} tcp_rcv_state_process() BPF instruction count: {}",
            self.progs.tcp_rcv_state_process.insn_cnt()
        );
    }
}
