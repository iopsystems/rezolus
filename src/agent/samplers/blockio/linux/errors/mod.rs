//! Collects BlockIO error and requeue stats using BPF and traces:
//! * `block_rq_complete` (filtered to status != BLK_STS_OK)
//! * `block_rq_requeue`
//!
//! And produces these stats:
//! * `blockio_errors`   — labeled by `op` and `error` class
//! * `blockio_requeues` — labeled by `op`

static NAME: &str = "blockio_errors";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/blockio_errors.bpf.rs"));
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

    // Order MUST match the BPF layout:
    // errors[cpu * 28 + op * 7 + cls]
    //   op: 0=read, 1=write, 2=flush, 3=discard
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

    // requeues[cpu * 4 + op]
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
    .counters("errors", errors)
    .counters("requeues", requeues)
    .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map<'_> {
        match name {
            "errors" => &self.maps.errors,
            "requeues" => &self.maps.requeues,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} block_rq_complete() BPF instruction count: {}",
            self.progs.block_rq_complete.insn_cnt()
        );
        debug!(
            "{NAME} block_rq_requeue() BPF instruction count: {}",
            self.progs.block_rq_requeue.insn_cnt()
        );
    }
}
