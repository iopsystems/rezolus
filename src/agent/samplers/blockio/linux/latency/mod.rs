//! Collects BlockIO Latency stats using BPF and traces:
//! * `block_rq_insert`
//! * `block_rq_issue`
//! * `block_rq_complete`
//!
//! And produces these stats:
//! * `blockio_latency`

const NAME: &str = "blockio_latency";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/blockio_latency.bpf.rs"));
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
    .histogram("read_latency", &BLOCKIO_READ_LATENCY)
    .histogram("write_latency", &BLOCKIO_WRITE_LATENCY)
    .histogram("flush_latency", &BLOCKIO_FLUSH_LATENCY)
    .histogram("discard_latency", &BLOCKIO_DISCARD_LATENCY)
    .disabled_programs(if kernel_has_btf() {
        &[
            "block_rq_insert_raw",
            "block_rq_issue_raw",
            "block_rq_complete_raw",
        ]
    } else {
        &[
            "block_rq_insert_btf",
            "block_rq_issue_btf",
            "block_rq_complete_btf",
        ]
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
            "{NAME} block_rq_insert_btf() BPF instruction count: {}",
            self.progs.block_rq_insert_btf.insn_cnt()
        );
        debug!(
            "{NAME} block_rq_issue_btf() BPF instruction count: {}",
            self.progs.block_rq_issue_btf.insn_cnt()
        );
        debug!(
            "{NAME} block_rq_complete_btf() BPF instruction count: {}",
            self.progs.block_rq_complete_btf.insn_cnt()
        );
    }
}
