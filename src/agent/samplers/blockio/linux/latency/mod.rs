//! Collects BlockIO Latency stats using BPF and traces:
//! * `block_rq_insert`
//! * `block_rq_issue`
//! * `block_rq_complete`
//!
//! And produces these stats:
//! * `blockio_latency` (issue -> complete: how long the device took)
//! * `blockio_queue_latency` (insert -> issue: how long the request waited)

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
    // All 4 op-class latency histograms share ONE group — see stats.rs's
    // `LATENCIES_ACQ` doc comment.
    .histogram("read_latency", &BLOCKIO_READ_LATENCY, &LATENCIES_ACQ)
    .histogram("write_latency", &BLOCKIO_WRITE_LATENCY, &LATENCIES_ACQ)
    .histogram("flush_latency", &BLOCKIO_FLUSH_LATENCY, &LATENCIES_ACQ)
    .histogram("discard_latency", &BLOCKIO_DISCARD_LATENCY, &LATENCIES_ACQ)
    // Queue residency is its own family, so its own group — see stats.rs's
    // `QUEUE_LATENCIES_ACQ` doc comment.
    .histogram(
        "read_queue_latency",
        &BLOCKIO_READ_QUEUE_LATENCY,
        &QUEUE_LATENCIES_ACQ,
    )
    .histogram(
        "write_queue_latency",
        &BLOCKIO_WRITE_QUEUE_LATENCY,
        &QUEUE_LATENCIES_ACQ,
    )
    .histogram(
        "flush_queue_latency",
        &BLOCKIO_FLUSH_QUEUE_LATENCY,
        &QUEUE_LATENCIES_ACQ,
    )
    .histogram(
        "discard_queue_latency",
        &BLOCKIO_DISCARD_QUEUE_LATENCY,
        &QUEUE_LATENCIES_ACQ,
    )
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
            "read_queue_latency" => &self.maps.read_queue_latency,
            "write_queue_latency" => &self.maps.write_queue_latency,
            "flush_queue_latency" => &self.maps.flush_queue_latency,
            "discard_queue_latency" => &self.maps.discard_queue_latency,
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
