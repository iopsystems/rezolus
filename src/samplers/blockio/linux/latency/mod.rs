/// Collects BlockIO Latency stats using BPF and traces:
/// * `block_rq_insert`
/// * `block_rq_issue`
/// * `block_rq_complete`
///
/// And produces these stats:
/// * `blockio/latency`

static NAME: &str = "blockio_latency";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/blockio_latency.bpf.rs"));
}

use bpf::*;

use crate::common::*;
use crate::samplers::blockio::linux::stats::*;
use crate::*;

use std::sync::Arc;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .histogram("latency", &BLOCKIO_LATENCY)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
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
            "{NAME} block_rq_insert() BPF instruction count: {}",
            self.progs.block_rq_insert.insn_cnt()
        );
        debug!(
            "{NAME} block_rq_issue() BPF instruction count: {}",
            self.progs.block_rq_issue.insn_cnt()
        );
        debug!(
            "{NAME} block_rq_complete() BPF instruction count: {}",
            self.progs.block_rq_complete.insn_cnt()
        );
    }
}
