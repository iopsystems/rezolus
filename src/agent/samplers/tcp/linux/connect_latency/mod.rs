//! Collects TCP connect latency stats using BPF and traces:
//! * `tcp_v4_connect`, `tcp_v6_connect` (fentry when the kernel has BTF, else
//!   kprobe)
//! * `inet_sock_set_state` (tp_btf when the tracepoint is in BTF, else raw_tp)
//! * `tcp_destroy_sock`
//!
//! And produces these stats:
//! * `tcp_connect_latency`, one histogram per `outcome`: established,
//!   refused, timeout, error, aborted

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

    let btf = kernel_btf_has_tracepoints(&["inet_sock_set_state"]);
    debug!(
        "{NAME}: inet_sock_set_state {}",
        if btf { "tp_btf" } else { "raw_tp" }
    );

    let mut disabled = unused_twins(&[
        (
            "tcp_v4_connect",
            "tcp_v4_connect_fentry",
            "tcp_v4_connect_kprobe",
        ),
        (
            "tcp_v6_connect",
            "tcp_v6_connect_fentry",
            "tcp_v6_connect_kprobe",
        ),
    ]);
    disabled.push(if btf {
        "inet_sock_set_state_raw"
    } else {
        "inet_sock_set_state_btf"
    });

    let bpf = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .histogram(
        "established",
        &TCP_CONNECT_LATENCY_ESTABLISHED,
        &LATENCY_ACQ,
    )
    .histogram("refused", &TCP_CONNECT_LATENCY_REFUSED, &LATENCY_ACQ)
    .histogram("timeout", &TCP_CONNECT_LATENCY_TIMEOUT, &LATENCY_ACQ)
    .histogram("error", &TCP_CONNECT_LATENCY_ERROR, &LATENCY_ACQ)
    .histogram("aborted", &TCP_CONNECT_LATENCY_ABORTED, &LATENCY_ACQ)
    // Per target, prefer fentry (cheaper dispatch); fall back to its kprobe
    // where fentry cannot attach (`kernel_can_fentry`). tcp_v6_connect is in
    // the ipv6 module on some kernels and may be absent from BTF; its kprobe
    // then misses at attach, which is tolerated.
    .disabled_programs(&disabled)
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
            "established" => &self.maps.established,
            "refused" => &self.maps.refused,
            "timeout" => &self.maps.timeout,
            "error" => &self.maps.error,
            "aborted" => &self.maps.aborted,
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
            "{NAME} inet_sock_set_state_btf() BPF instruction count: {}",
            self.progs.inet_sock_set_state_btf.insn_cnt()
        );
    }
}
