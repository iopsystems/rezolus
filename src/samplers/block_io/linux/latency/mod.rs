/// Collects BlockIO Latency stats using BPF and traces:
/// * `block_rq_insert`
/// * `block_rq_issue`
/// * `block_rq_complete`
///
/// And produces these stats:
/// * `blockio/latency`

static NAME: &str = "block_io_latency";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/block_io_latency.bpf.rs"));
}

use bpf::*;

use crate::common::bpf::*;
use crate::samplers::block_io::stats::*;
use crate::*;

#[distributed_slice(ASYNC_SAMPLERS)]
fn spawn(config: Arc<Config>, runtime: &Runtime) {
    // check if sampler should be enabled
    if !(config.enabled(NAME) && config.bpf(NAME)) {
        return;
    }

    let bpf = AsyncBpfBuilder::new(ModSkelBuilder::default)
        .distribution("latency", &BLOCKIO_LATENCY)
        .collected_at(&METADATA_BLOCKIO_LATENCY_COLLECTED_AT)
        .runtime(
            &METADATA_BLOCKIO_LATENCY_RUNTIME,
            &METADATA_BLOCKIO_LATENCY_RUNTIME_HISTOGRAM,
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
