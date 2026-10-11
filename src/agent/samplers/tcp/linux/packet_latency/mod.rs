//! Collects TCP packet latency stats using BPF and traces:
//! * `tcp_destroy_sock`
//! * `tcp_probe`
//! * `tcp_rcv_space_adjust`
//!
//! And produces these stats:
//! * `tcp/receive/packet_latency`

const NAME: &str = "tcp_packet_latency";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_packet_latency.bpf.rs"));
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

    // Both flavors run the same slot-array logic; see mod.bpf.c. `tp_btf`
    // needs all three tracepoints in BTF, as any `tp_btf` program does, and
    // `bpf_skc_to_tcp_sock` (5.9+), or the load fails for the whole skeleton.
    let btf =
        kernel_btf_has_tracepoints(&["tcp_probe", "tcp_rcv_space_adjust", "tcp_destroy_sock"])
            && kernel_btf_has_helper("bpf_skc_to_tcp_sock");

    debug!("{NAME}: {}", if btf { "tp_btf" } else { "raw_tp" });

    let bpf = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .histogram("latency", &TCP_PACKET_LATENCY, &LATENCY_ACQ)
    .disabled_programs(if btf {
        &[
            "tcp_probe_raw",
            "tcp_rcv_space_adjust_raw",
            "tcp_destroy_sock_raw",
        ]
    } else {
        &[
            "tcp_probe_btf",
            "tcp_rcv_space_adjust_btf",
            "tcp_destroy_sock_btf",
        ]
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
            "latency" => &self.maps.latency,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        // Both flavors are logged; the debug line at init names the one
        // attached.
        debug!(
            "{NAME} tcp_probe_btf() BPF instruction count: {}",
            self.progs.tcp_probe_btf.insn_cnt()
        );
        debug!(
            "{NAME} tcp_rcv_space_adjust_btf() BPF instruction count: {}",
            self.progs.tcp_rcv_space_adjust_btf.insn_cnt()
        );
        debug!(
            "{NAME} tcp_destroy_sock_btf() BPF instruction count: {}",
            self.progs.tcp_destroy_sock_btf.insn_cnt()
        );
        debug!(
            "{NAME} tcp_probe_raw() BPF instruction count: {}",
            self.progs.tcp_probe_raw.insn_cnt()
        );
        debug!(
            "{NAME} tcp_rcv_space_adjust_raw() BPF instruction count: {}",
            self.progs.tcp_rcv_space_adjust_raw.insn_cnt()
        );
        debug!(
            "{NAME} tcp_destroy_sock_raw() BPF instruction count: {}",
            self.progs.tcp_destroy_sock_raw.insn_cnt()
        );
    }
}
