/// Collects TCP Retransmit stats using BPF and traces:
/// * `tcp_retransmit_timer`
///
/// And produces these stats:
/// * `tcp/transmit/retransmit`

const NAME: &str = "tcp_retransmit";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_retransmit.bpf.rs"));
}

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::tcp::stats::*;
use crate::samplers::tcp::*;

#[distributed_slice(ASYNC_SAMPLERS)]
fn spawn(config: Arc<Config>, runtime: &Runtime) {
    // check if sampler should be enabled
    if !config.enabled(NAME) {
        return;
    }

    let counters = vec![Counter::new(
        &TCP_TX_RETRANSMIT,
        Some(&TCP_TX_RETRANSMIT_HISTOGRAM),
    )];

    let bpf = AsyncBpfBuilder::new(ModSkelBuilder::default)
        .counters("counters", counters)
        .collected_at(&METADATA_TCP_RETRANSMIT_COLLECTED_AT)
        .runtime(
            &METADATA_TCP_RETRANSMIT_RUNTIME,
            &METADATA_TCP_RETRANSMIT_RUNTIME_HISTOGRAM,
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
            "counters" => &self.maps.counters,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} tcp_retransmit_skb() BPF instruction count: {}",
            self.progs.tcp_retransmit_skb.insn_cnt()
        );
    }
}
