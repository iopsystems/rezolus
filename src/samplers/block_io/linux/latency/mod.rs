#[distributed_slice(BLOCK_IO_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = BlockIOLatency::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/block_io_latency.bpf.rs"));
}

static NAME: &str = "block_io_latency";

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::block_io::stats::*;
use crate::samplers::block_io::*;

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "latency" => &self.maps.latency,
            _ => unimplemented!(),
        }
    }
}

/// Collects Scheduler Runqueue Latency stats using BPF and traces:
/// * `block_rq_insert`
/// * `block_rq_issue`
/// * `block_rq_complete`
///
/// And produces these stats:
/// * `blockio/latency`
/// * `blockio/size`
pub struct BlockIOLatency {
    bpf: Bpf<ModSkel<'static>>,
    distribution_interval: Interval,
}

impl BlockIOLatency {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !(config.enabled(NAME) && config.bpf(NAME)) {
            return Err(());
        }

        let open_object: &'static mut MaybeUninit<OpenObject> =
            Box::leak(Box::new(MaybeUninit::uninit()));

        let builder = ModSkelBuilder::default();
        let mut skel = builder
            .open(open_object)
            .map_err(|e| error!("failed to open bpf builder: {e}"))?
            .load()
            .map_err(|e| error!("failed to load bpf program: {e}"))?;

        debug!(
            "{NAME} block_rq_insert() BPF instruction count: {}",
            skel.progs.block_rq_insert.insn_cnt()
        );
        debug!(
            "{NAME} block_rq_issue() BPF instruction count: {}",
            skel.progs.block_rq_issue.insn_cnt()
        );
        debug!(
            "{NAME} block_rq_complete() BPF instruction count: {}",
            skel.progs.block_rq_complete.insn_cnt()
        );

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let bpf = BpfBuilder::new(skel)
            .distribution("latency", &BLOCKIO_LATENCY)
            .build();

        let now = Instant::now();

        Ok(Self {
            bpf,
            distribution_interval: Interval::new(now, config.distribution_interval(NAME)),
        })
    }

    pub fn refresh_distributions(&mut self, now: Instant) -> Result<(), ()> {
        self.distribution_interval.try_wait(now)?;

        self.bpf.refresh_distributions();

        Ok(())
    }
}

impl Sampler for BlockIOLatency {
    fn sample(&mut self) {
        let now = Instant::now();
        let _ = self.refresh_distributions(now);
    }
}
