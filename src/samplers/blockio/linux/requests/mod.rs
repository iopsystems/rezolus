/// Collects BlockIO Request stats using BPF and traces:
/// * `block_rq_complete`
///
/// And produces these stats:
/// * `blockio/*/operations`
/// * `blockio/*/bytes`
/// * `blockio/size`

static NAME: &str = "blockio_requests";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/blockio_requests.bpf.rs"));
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

    let bpf = BpfBuilder::new(ModSkelBuilder::default)
        .counters("counters", counters)
        .histogram("size", &BLOCKIO_SIZE)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "counters" => &self.maps.counters,
            "size" => &self.maps.size,
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
    }
}
