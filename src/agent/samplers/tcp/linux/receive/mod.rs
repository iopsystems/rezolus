//! Collects TCP Receive stats using BPF and traces:
//! * `tcp_rcv_established` (fentry when the kernel has BTF, else kprobe)
//!
//! And produces these stats:
//! * `tcp/receive/jitter`
//! * `tcp/receive/srtt`

const NAME: &str = "tcp_receive";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_receive.bpf.rs"));
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
    .histogram("srtt", &TCP_SRTT, &SRTT_ACQ)
    .histogram("jitter", &TCP_JITTER, &JITTER_ACQ)
    // Prefer fentry (cheaper dispatch); fall back to kprobe without BTF.
    .disabled_programs(if kernel_has_btf() {
        &["tcp_rcv_kprobe"]
    } else {
        &["tcp_rcv_fentry"]
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
            "srtt" => &self.maps.srtt,
            "jitter" => &self.maps.jitter,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} tcp_rcv_established_fentry() BPF instruction count: {}",
            self.progs.tcp_rcv_fentry.insn_cnt()
        );
    }
}
