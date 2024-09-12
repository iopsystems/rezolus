use crate::common::*;
use crate::samplers::tcp::stats::*;
use crate::samplers::tcp::*;

const NAME: &str = "tcp_traffic";

#[cfg(feature = "bpf")]
mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_traffic.bpf.rs"));
}

#[cfg(feature = "bpf")]
use crate::common::bpf::*;
#[cfg(feature = "bpf")]
use bpf::*;

mod proc;

use proc::*;

#[cfg(feature = "bpf")]
impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "counters" => &self.maps.counters,
            "rx_size" => &self.maps.rx_size,
            "tx_size" => &self.maps.tx_size,
            _ => unimplemented!(),
        }
    }
}

#[cfg(feature = "bpf")]
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

#[cfg(feature = "bpf")]
#[distributed_slice(ASYNC_SAMPLERS)]
fn spawn(config: Arc<Config>, runtime: &Runtime) {
    // check if sampler should be enabled
    if !(config.enabled(NAME) && config.bpf(NAME)) {
        return;
    }

    let counters = vec![
        Counter::new(&TCP_RX_BYTES,  None),
        Counter::new(&TCP_TX_BYTES,  None),
        Counter::new(&TCP_RX_PACKETS,  None),
        Counter::new(&TCP_TX_PACKETS,  None),
    ];

    let bpf = AsyncBpfBuilder::new(ModSkelBuilder::default)
        .counters("counters", counters)
        .distribution("rx_size", &TCP_RX_SIZE)
        .distribution("tx_size", &TCP_TX_SIZE)
        .collected_at(&METADATA_TCP_TRAFFIC_COLLECTED_AT)
        .runtime(
            &METADATA_TCP_TRAFFIC_RUNTIME,
            &METADATA_TCP_TRAFFIC_RUNTIME_HISTOGRAM,
        )
        .build();

    if bpf.is_ok() {
        runtime.spawn(async move {
            let mut sampler = AsyncBpfSampler::new(bpf.unwrap(), config.async_interval(NAME));

            loop {
                if sampler.is_finished() {
                    return;
                }

                sampler.sample().await;
            }
        });
    } else {
        runtime.spawn(async move {
            if let Ok(mut sampler) = ProcNetSnmp::new(config.async_interval(NAME)) {
                loop {
                    sampler.sample().await;
                }
            }
        });
    }
}
