//! Collects BlockIO Latency stats using BPF and traces:
//! * `block_rq_insert`
//! * `block_rq_issue`
//! * `block_rq_complete`
//!
//! And produces these stats:
//! * `blockio_latency`

static NAME: &str = "blockio_latency";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/blockio_latency.bpf.rs"));
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

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .histogram("read_latency", &BLOCKIO_READ_LATENCY)
        .histogram("write_latency", &BLOCKIO_WRITE_LATENCY)
        .histogram("flush_latency", &BLOCKIO_FLUSH_LATENCY)
        .histogram("discard_latency", &BLOCKIO_DISCARD_LATENCY)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "read_latency" => &self.maps.read_latency,
            "write_latency" => &self.maps.write_latency,
            "flush_latency" => &self.maps.flush_latency,
            "discard_latency" => &self.maps.discard_latency,
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
