//! Collects BlockIO Request stats using BPF and traces:
//! * `block_rq_complete`
//!
//! And produces these stats:
//! * `blockio_bytes`
//! * `blockio_operations`
//! * `blockio_size`

static NAME: &str = "blockio_requests";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/blockio_requests.bpf.rs"));
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
        &BLOCKIO_READ_OPS,
        &BLOCKIO_WRITE_OPS,
        &BLOCKIO_FLUSH_OPS,
        &BLOCKIO_DISCARD_OPS,
        &BLOCKIO_READ_BYTES,
        &BLOCKIO_WRITE_BYTES,
        &BLOCKIO_FLUSH_BYTES,
        &BLOCKIO_DISCARD_BYTES,
    ];

    let bpf = BpfBuilder::new(NAME, ModSkelBuilder::default)
        .counters("counters", counters)
        .histogram("read_size", &BLOCKIO_READ_SIZE)
        .histogram("write_size", &BLOCKIO_WRITE_SIZE)
        .histogram("flush_size", &BLOCKIO_FLUSH_SIZE)
        .histogram("discard_size", &BLOCKIO_DISCARD_SIZE)
        .build()?;

    Ok(Some(Box::new(bpf)))
}

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "counters" => &self.maps.counters,
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
            "{NAME} block_rq_complete() BPF instruction count: {}",
            self.progs.block_rq_complete.insn_cnt()
        );
    }
}
