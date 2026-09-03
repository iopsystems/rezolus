//! Collects BlockIO Latency stats using BPF and traces:
//! * `block_rq_insert`
//! * `block_rq_issue`
//! * `block_rq_complete`
//!
//! And produces these stats, one per phase of a request's life:
//! * `blockio_queue_latency` (insert -> issue: how long the request waited)
//! * `blockio_device_latency` (issue -> complete: how long the device took)
//! * `blockio_total_latency` (insert -> complete: the two together)
//!
//! `blockio_device_latency` was called `blockio_latency` before the phases were
//! split out, and measures exactly what that metric always measured.

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
    // One group per phase — see stats.rs's acquisition-group doc comment.
    .histogram(
        "read_device_latency",
        &BLOCKIO_READ_DEVICE_LATENCY,
        &DEVICE_LATENCIES_ACQ,
    )
    .histogram(
        "write_device_latency",
        &BLOCKIO_WRITE_DEVICE_LATENCY,
        &DEVICE_LATENCIES_ACQ,
    )
    .histogram(
        "flush_device_latency",
        &BLOCKIO_FLUSH_DEVICE_LATENCY,
        &DEVICE_LATENCIES_ACQ,
    )
    .histogram(
        "discard_device_latency",
        &BLOCKIO_DISCARD_DEVICE_LATENCY,
        &DEVICE_LATENCIES_ACQ,
    )
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
    .histogram(
        "read_total_latency",
        &BLOCKIO_READ_TOTAL_LATENCY,
        &TOTAL_LATENCIES_ACQ,
    )
    .histogram(
        "write_total_latency",
        &BLOCKIO_WRITE_TOTAL_LATENCY,
        &TOTAL_LATENCIES_ACQ,
    )
    .histogram(
        "flush_total_latency",
        &BLOCKIO_FLUSH_TOTAL_LATENCY,
        &TOTAL_LATENCIES_ACQ,
    )
    .histogram(
        "discard_total_latency",
        &BLOCKIO_DISCARD_TOTAL_LATENCY,
        &TOTAL_LATENCIES_ACQ,
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
            "read_device_latency" => &self.maps.read_device_latency,
            "write_device_latency" => &self.maps.write_device_latency,
            "flush_device_latency" => &self.maps.flush_device_latency,
            "discard_device_latency" => &self.maps.discard_device_latency,
            "read_queue_latency" => &self.maps.read_queue_latency,
            "write_queue_latency" => &self.maps.write_queue_latency,
            "flush_queue_latency" => &self.maps.flush_queue_latency,
            "discard_queue_latency" => &self.maps.discard_queue_latency,
            "read_total_latency" => &self.maps.read_total_latency,
            "write_total_latency" => &self.maps.write_total_latency,
            "flush_total_latency" => &self.maps.flush_total_latency,
            "discard_total_latency" => &self.maps.discard_total_latency,
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
