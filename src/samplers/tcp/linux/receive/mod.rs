/// Collects TCP Receive stats using BPF and traces:
/// * `tcp_rcv_established`
///
/// And produces these stats:
/// * `tcp/receive/jitter`
/// * `tcp/receive/srtt`

const NAME: &str = "tcp_receive";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_receive.bpf.rs"));
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
        .distribution("srtt", &TCP_SRTT)
        .distribution("jitter", &TCP_JITTER)
        .collected_at(&METADATA_TCP_RECEIVE_COLLECTED_AT)
        .runtime(
            &METADATA_TCP_RECEIVE_RUNTIME,
            &METADATA_TCP_RECEIVE_RUNTIME_HISTOGRAM,
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
            "srtt" => &self.maps.srtt,
            "jitter" => &self.maps.jitter,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} tcp_rcv() BPF instruction count: {}",
            self.progs.tcp_rcv_kprobe.insn_cnt()
        );
    }
}
