//! Collects Softnet stats using BPF and traces:
//! * `net_rx_action`
//!
//! And produces these stats:
//! * `softnet_time_squeezed`
//! * `softnet_budget_exhausted`
//! * `softnet_processed`
//! * `softnet_poll`

const NAME: &str = "network_softnet";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/network_softnet.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let counters = vec![
        &SOFTNET_TIME_SQUEEZED,
        &SOFTNET_BUDGET_EXHAUSTED, 
        &SOFTNET_PROCESSED,
        &SOFTNET_POLL,
    ];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .cpu_counters("counters", counters)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
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
            "{NAME} net_rx_action_enter() BPF instruction count: {}",
            self.progs.net_rx_action_enter.insn_cnt()
        );
        debug!(
            "{NAME} net_rx_action_exit() BPF instruction count: {}",
            self.progs.net_rx_action_exit.insn_cnt()
        );
        debug!(
            "{NAME} napi_poll_enter_fn() BPF instruction count: {}",
            self.progs.napi_poll_enter_fn.insn_cnt()
        );
        debug!(
            "{NAME} napi_poll_exit_fn() BPF instruction count: {}",
            self.progs.napi_poll_exit_fn.insn_cnt()
        );
        debug!(
            "{NAME} napi_gro_receive_kprobe() BPF instruction count: {}",
            self.progs.napi_gro_receive_kprobe.insn_cnt()
        );
    }
}
