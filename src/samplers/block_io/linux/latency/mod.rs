#[distributed_slice(BLOCK_IO_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = Biolat::new(config) {
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
        self.obj.map(name).unwrap()
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
pub struct Biolat {
    bpf: Bpf<ModSkel<'static>>,
    counter_interval: Duration,
    counter_next: Instant,
    counter_prev: Instant,
    distribution_interval: Duration,
    distribution_next: Instant,
    distribution_prev: Instant,
}

impl Biolat {
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
            "{NAME} block_rq_insert() BPF instruction count: {}",
            skel.progs().block_rq_insert().insn_cnt()
        );
        debug!(
            "{NAME} block_rq_issue() BPF instruction count: {}",
            skel.progs().block_rq_issue().insn_cnt()
        );
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
            .distribution("latency", &BLOCKIO_LATENCY)
            .distribution("size", &BLOCKIO_SIZE)
            .build();

        Ok(Self {
            bpf,
            counter_interval: config.interval(NAME),
            counter_next: Instant::now(),
            counter_prev: Instant::now(),
            distribution_interval: config.distribution_interval(NAME),
            distribution_next: Instant::now(),
            distribution_prev: Instant::now(),
        })
    }

    pub fn refresh_counters(&mut self, now: Instant) {
        if now < self.counter_next {
            return;
        }

        let elapsed = now - self.counter_prev;

        self.bpf.refresh_counters(elapsed);

        // determine when to sample next
        let next = self.counter_next + self.counter_interval;

        // check that next sample time is in the future
        if next > now {
            self.counter_next = next;
        } else {
            self.counter_next = now + self.counter_interval;
        }

        // mark when we last sampled
        self.counter_prev = now;
    }

    pub fn refresh_distributions(&mut self, now: Instant) {
        if now < self.distribution_next {
            return;
        }

        self.bpf.refresh_distributions();

        // determine when to sample next
        let next = self.distribution_next + self.distribution_interval;

        // check that next sample time is in the future
        if next > now {
            self.distribution_next = next;
        } else {
            self.distribution_next = now + self.distribution_interval;
        }

        // mark when we last sampled
        self.distribution_prev = now;
    }
}

impl Sampler for Biolat {
    fn sample(&mut self) {
        let now = Instant::now();
        self.refresh_counters(now);
        self.refresh_distributions(now);
    }
}
