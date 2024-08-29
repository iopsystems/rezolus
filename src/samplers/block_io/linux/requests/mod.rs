/// Collects BlockIO Request stats using BPF and traces:
/// * `block_rq_complete`
///
/// And produces these stats:
/// * `blockio/*/operations`
/// * `blockio/*/bytes`
/// * `blockio/size`

static NAME: &str = "block_io_requests";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/block_io_requests.bpf.rs"));
}

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::block_io::stats::*;
use crate::*;

#[distributed_slice(ASYNC_SAMPLERS)]
fn spawn(config: Arc<Config>, runtime: &Runtime) {
    // check if sampler should be enabled
    if !(config.enabled(NAME) && config.bpf(NAME)) {
        return;
    }

    let counters = vec![
        Counter::new(&BLOCKIO_READ_OPS, Some(&BLOCKIO_READ_OPS_HISTOGRAM)),
        Counter::new(&BLOCKIO_WRITE_OPS, Some(&BLOCKIO_WRITE_OPS_HISTOGRAM)),
        Counter::new(&BLOCKIO_FLUSH_OPS, Some(&BLOCKIO_FLUSH_OPS_HISTOGRAM)),
        Counter::new(&BLOCKIO_DISCARD_OPS, Some(&BLOCKIO_DISCARD_OPS_HISTOGRAM)),
        Counter::new(&BLOCKIO_READ_BYTES, Some(&BLOCKIO_READ_BYTES_HISTOGRAM)),
        Counter::new(&BLOCKIO_WRITE_BYTES, Some(&BLOCKIO_WRITE_BYTES_HISTOGRAM)),
        Counter::new(&BLOCKIO_FLUSH_BYTES, Some(&BLOCKIO_FLUSH_BYTES_HISTOGRAM)),
        Counter::new(
            &BLOCKIO_DISCARD_BYTES,
            Some(&BLOCKIO_DISCARD_BYTES_HISTOGRAM),
        ),
    ];

    let bpf = AsyncBpfBuilder::new(ModSkelBuilder::default)
        .counters("counters", counters)
        .distribution("size", &BLOCKIO_SIZE)
        .collected_at(&METADATA_BLOCKIO_REQUESTS_COLLECTED_AT)
        .runtime(
            &METADATA_BLOCKIO_REQUESTS_RUNTIME,
            &METADATA_BLOCKIO_REQUESTS_RUNTIME_HISTOGRAM,
        )
        .build();

    if bpf.is_err() {
        return;
    }

    runtime.spawn(async move {
        let mut sampler = AsyncBpfSampler::new(bpf.unwrap(), config.async_interval(NAME));

        loop {
            if sampler.is_finished() {
                return;
            }

            sampler.sample().await;
        }
    });
}

impl GetMap for ModSkel<'_> {
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
