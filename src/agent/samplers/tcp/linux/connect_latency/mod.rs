//! Collects TCP packet latency stats using BPF and traces:
//! * `tcp_v4_connect`, `tcp_v6_connect`, `tcp_rcv_state_process`
//!   (fentry when the kernel has BTF, else kprobe)
//! * `tcp_destroy_sock`
//!
//! And produces these stats:
//! * `tcp/connect_latency`

const NAME: &str = "tcp_connect_latency";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_connect_latency.bpf.rs"));
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

    let bpf = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .histogram("latency", &TCP_CONNECT_LATENCY, &LATENCY_ACQ)
    // Prefer fentry (cheaper dispatch); fall back to kprobe without BTF.
    .disabled_programs(if kernel_has_btf() {
        &[
            "tcp_v4_connect_kprobe",
            "tcp_v6_connect_kprobe",
            "tcp_rcv_state_process_kprobe",
        ]
    } else {
        &[
            "tcp_v4_connect_fentry",
            "tcp_v6_connect_fentry",
            "tcp_rcv_state_process_fentry",
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
        debug!(
            "{NAME} tcp_v4_connect_fentry() BPF instruction count: {}",
            self.progs.tcp_v4_connect_fentry.insn_cnt()
        );
        debug!(
            "{NAME} tcp_rcv_state_process_fentry() BPF instruction count: {}",
            self.progs.tcp_rcv_state_process_fentry.insn_cnt()
        );
    }
}
