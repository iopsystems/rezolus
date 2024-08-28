#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> Box<dyn Sampler> {
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
    interval: Interval,
}

impl BlockIOLatency {
    pub fn new(config: Arc<Config>) -> Result<Self, ()> {
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
            interval: Interval::new(now, config.interval(NAME)),
        })
    }

    pub fn refresh(&mut self, now: Instant) -> Result<(), ()> {
        let elapsed = self.interval.try_wait(now)?;

        METADATA_BLOCKIO_LATENCY_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

        self.bpf.refresh(elapsed);

        Ok(())
    }
}

impl Sampler for BlockIOLatency {
    fn sample(&mut self) {
        let now = Instant::now();

        if self.refresh(now).is_ok() {
            let elapsed = now.elapsed().as_nanos() as u64;

            METADATA_BLOCKIO_LATENCY_RUNTIME.add(elapsed);
            let _ = METADATA_BLOCKIO_LATENCY_RUNTIME_HISTOGRAM.increment(elapsed);
        }
    }
}
