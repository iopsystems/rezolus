#[distributed_slice(BLOCK_IO_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = BlockIORequests::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/block_io_requests.bpf.rs"));
}

static NAME: &str = "block_io_requests";

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::block_io::stats::*;
use crate::samplers::block_io::*;

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        self.obj.map(name).unwrap()
    }
}

/// Collects BlockIO stats using BPF and traces:
/// * `block_rq_complete`
///
/// And produces these stats:
/// * `blockio/*/operations`
/// * `blockio/*/bytes`
/// * `blockio/size`
pub struct BlockIORequests {
    bpf: Bpf<ModSkel<'static>>,
    counter_interval: Interval,
    distribution_interval: Interval,
}

impl BlockIORequests {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !(config.enabled(NAME) && config.bpf(NAME)) {
            return Err(());
        }

        let builder = ModSkelBuilder::default();
        let mut skel = builder
            .open()
            .map_err(|e| error!("failed to open bpf builder: {e}"))?
            .load()
            .map_err(|e| error!("failed to load bpf program: {e}"))?;

        debug!(
            "{NAME} block_rq_complete() BPF instruction count: {}",
            skel.progs().block_rq_complete().insn_cnt()
        );

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

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

        let bpf = BpfBuilder::new(skel)
            .counters("counters", counters)
            .distribution("size", &BLOCKIO_SIZE)
            .build();

        let now = Instant::now();

        Ok(Self {
            bpf,
            counter_interval: Interval::new(now, config.interval(NAME)),
            distribution_interval: Interval::new(now, config.distribution_interval(NAME)),
        })
    }

    pub fn refresh_counters(&mut self, now: Instant) -> Result<(), ()> {
        let elapsed = self.counter_interval.try_wait(now)?;

        METADATA_BLOCKIO_REQUESTS_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

        self.bpf.refresh_counters(elapsed);

        Ok(())
    }

    pub fn refresh_distributions(&mut self, now: Instant) -> Result<(), ()> {
        self.distribution_interval.try_wait(now)?;

        self.bpf.refresh_distributions();

        Ok(())
    }
}

impl Sampler for BlockIORequests {
    fn sample(&mut self) {
        let now = Instant::now();

        if self.refresh_counters(now).is_ok() || self.refresh_distributions(now).is_ok() {
            let elapsed = now.elapsed().as_nanos() as u64;

            METADATA_BLOCKIO_REQUESTS_RUNTIME.add(elapsed);
            let _ = METADATA_BLOCKIO_REQUESTS_RUNTIME_HISTOGRAM.increment(elapsed);
        }
    }
}
