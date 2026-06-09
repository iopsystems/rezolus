//! Collects BlockIO Request stats using BPF and traces:
//! * `block_rq_complete`
//! * `block_rq_requeue`
//!
//! And produces these stats:
//! * `blockio_bytes`
//! * `blockio_operations`
//! * `blockio_size`
//! * `blockio_errors`   — labeled by `op` and `error` class
//! * `blockio_requeues` — labeled by `op`

const NAME: &str = "blockio_requests";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/blockio_requests.bpf.rs"));
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

    let counters = vec![
        &BLOCKIO_READ_OPS,
        &BLOCKIO_WRITE_OPS,
        &BLOCKIO_FLUSH_OPS,
        &BLOCKIO_DISCARD_OPS,
        &BLOCKIO_READ_BYTES,
        &BLOCKIO_WRITE_BYTES,
        &BLOCKIO_FLUSH_BYTES,
        &BLOCKIO_DISCARD_BYTES,
    ];

    // Order MUST match the BPF layout:
    // errors[cpu * 32 + op * 7 + cls]
    //   op:  0=read, 1=write, 2=flush, 3=discard
    //   cls: 0=io, 1=timeout, 2=nospc, 3=target, 4=protection,
    //        5=unsupported, 6=other
    let errors = vec![
        // op = read
        &BLOCKIO_READ_ERR_IO,
        &BLOCKIO_READ_ERR_TIMEOUT,
        &BLOCKIO_READ_ERR_NOSPC,
        &BLOCKIO_READ_ERR_TARGET,
        &BLOCKIO_READ_ERR_PROTECTION,
        &BLOCKIO_READ_ERR_UNSUPPORTED,
        &BLOCKIO_READ_ERR_OTHER,
        // op = write
        &BLOCKIO_WRITE_ERR_IO,
        &BLOCKIO_WRITE_ERR_TIMEOUT,
        &BLOCKIO_WRITE_ERR_NOSPC,
        &BLOCKIO_WRITE_ERR_TARGET,
        &BLOCKIO_WRITE_ERR_PROTECTION,
        &BLOCKIO_WRITE_ERR_UNSUPPORTED,
        &BLOCKIO_WRITE_ERR_OTHER,
        // op = flush
        &BLOCKIO_FLUSH_ERR_IO,
        &BLOCKIO_FLUSH_ERR_TIMEOUT,
        &BLOCKIO_FLUSH_ERR_NOSPC,
        &BLOCKIO_FLUSH_ERR_TARGET,
        &BLOCKIO_FLUSH_ERR_PROTECTION,
        &BLOCKIO_FLUSH_ERR_UNSUPPORTED,
        &BLOCKIO_FLUSH_ERR_OTHER,
        // op = discard
        &BLOCKIO_DISCARD_ERR_IO,
        &BLOCKIO_DISCARD_ERR_TIMEOUT,
        &BLOCKIO_DISCARD_ERR_NOSPC,
        &BLOCKIO_DISCARD_ERR_TARGET,
        &BLOCKIO_DISCARD_ERR_PROTECTION,
        &BLOCKIO_DISCARD_ERR_UNSUPPORTED,
        &BLOCKIO_DISCARD_ERR_OTHER,
    ];

    // requeues[cpu * 8 + op]
    let requeues = vec![
        &BLOCKIO_READ_REQUEUE,
        &BLOCKIO_WRITE_REQUEUE,
        &BLOCKIO_FLUSH_REQUEUE,
        &BLOCKIO_DISCARD_REQUEUE,
    ];

    let bpf = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .counters("counters", counters)
    .counters("errors", errors)
    .counters("requeues", requeues)
    .histogram("read_size", &BLOCKIO_READ_SIZE)
    .histogram("write_size", &BLOCKIO_WRITE_SIZE)
    .histogram("flush_size", &BLOCKIO_FLUSH_SIZE)
    .histogram("discard_size", &BLOCKIO_DISCARD_SIZE)
    .disabled_programs(if kernel_has_btf() {
        &["block_rq_complete_raw", "block_rq_requeue_raw"]
    } else {
        &["block_rq_complete_btf", "block_rq_requeue_btf"]
    })
    .build()?;

    Ok(Some(Box::new(bpf)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry =
    crate::agent::samplers::SamplerEntry { name: NAME, init };

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map<'_> {
        match name {
            "counters" => &self.maps.counters,
            "errors" => &self.maps.errors,
            "requeues" => &self.maps.requeues,
            "read_size" => &self.maps.read_size,
            "write_size" => &self.maps.write_size,
            "flush_size" => &self.maps.flush_size,
            "discard_size" => &self.maps.discard_size,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} block_rq_complete_btf() BPF instruction count: {}",
            self.progs.block_rq_complete_btf.insn_cnt()
        );
        debug!(
            "{NAME} block_rq_requeue_btf() BPF instruction count: {}",
            self.progs.block_rq_requeue_btf.insn_cnt()
        );
    }
}
