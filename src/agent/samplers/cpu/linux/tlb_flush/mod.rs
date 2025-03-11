//! Collects tlb flush event information using BPF and traces:
//! * `tlb_flush`
//!
//! And produces these stats:
//! * `cpu_tlb_flush`
//!
//! These stats can be used to understand the reason for TLB flushes.

const NAME: &str = "cpu_tlb_flush";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/cpu_tlb_flush.bpf.rs"));
}

use bpf::*;

use crate::agent::*;

use std::sync::Arc;

mod stats;

use stats::*;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let events = vec![
        &TLB_FLUSH_TASK_SWITCH,
        &TLB_FLUSH_REMOTE_SHOOTDOWN,
        &TLB_FLUSH_LOCAL_SHOOTDOWN,
        &TLB_FLUSH_LOCAL_MM_SHOOTDOWN,
        &TLB_FLUSH_REMOTE_SEND_IPI,
    ];

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .cpu_counters("events", events)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "events" => &self.maps.events,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} tlb_flush() BPF instruction count: {}",
            self.progs.tlb_flush.insn_cnt()
        );
    }
}
