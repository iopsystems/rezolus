/// Collects TCP packet latency stats using BPF and traces:
/// * `tcp_destroy_sock`
/// * `tcp_probe`
/// * `tcp_rcv_space_adjust`
///
/// And produces these stats:
/// * `tcp/receive/packet_latency`

const NAME: &str = "tcp_packet_latency";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_packet_latency.bpf.rs"));
}

use bpf::*;

use crate::common::bpf::*;
use crate::samplers::tcp::stats::*;
use crate::samplers::tcp::*;

#[distributed_slice(ASYNC_SAMPLERS)]
fn spawn(config: Arc<Config>, runtime: &Runtime) {
    // check if sampler should be enabled
    if !(config.enabled(NAME) && config.bpf(NAME)) {
        return;
    }

    let bpf = AsyncBpfBuilder::new(ModSkelBuilder::default)
        .distribution("latency", &TCP_PACKET_LATENCY)
        .collected_at(&METADATA_TCP_PACKET_LATENCY_COLLECTED_AT)
        .runtime(
            &METADATA_TCP_PACKET_LATENCY_RUNTIME,
            &METADATA_TCP_PACKET_LATENCY_RUNTIME_HISTOGRAM,
        )
        .build();

    if bpf.is_err() {
        return;
    }

    runtime.spawn(async move {
        let mut sampler = AsyncBpfSampler::new(bpf.unwrap(), config.async_interval(NAME));

        loop {
            if sampler.is_finished() {
                return;
            }

            sampler.sample().await;
        }
    });
}

impl GetMap for ModSkel<'_> {
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
            "{NAME} tcp_probe() BPF instruction count: {}",
            self.progs.tcp_probe.insn_cnt()
        );
        debug!(
            "{NAME} tcp_rcv_space_adjust() BPF instruction count: {}",
            self.progs.tcp_rcv_space_adjust.insn_cnt()
        );
        debug!(
            "{NAME} tcp_destroy_sock() BPF instruction count: {}",
            self.progs.tcp_destroy_sock.insn_cnt()
        );
    }
}
