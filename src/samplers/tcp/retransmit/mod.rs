#[distributed_slice(TCP_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = Retransmit::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_retransmit.bpf.rs"));
}

use bpf::*;

use super::stats::*;
use super::*;
use crate::common::bpf::*;
use crate::common::*;

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        self.obj.map(name).unwrap()
    }
}

/// Collects TCP Retransmit stats using BPF and traces:
/// * `tcp_retransmit_timer`
///
/// And produces these stats:
/// * `tcp/transmit/retransmit`
pub struct Retransmit {
    bpf: Bpf<ModSkel<'static>>,
    counter_interval: Duration,
    counter_next: Instant,
    counter_prev: Instant,
    distribution_interval: Duration,
    distribution_next: Instant,
    distribution_prev: Instant,
}

impl Retransmit {
    pub fn new(_config: &Config) -> Result<Self, ()> {
        let builder = ModSkelBuilder::default();
        let mut skel = builder
            .open()
            .map_err(|e| error!("failed to open bpf builder: {e}"))?
            .load()
            .map_err(|e| error!("failed to load bpf program: {e}"))?;

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let mut bpf = Bpf::from_skel(skel);

        let counters = vec![Counter::new(
            &TCP_TX_RETRANSMIT,
            Some(&TCP_TX_RETRANSMIT_HEATMAP),
        )];

        bpf.add_counters("counters", counters);

        Ok(Self {
            bpf,
            counter_interval: Duration::from_millis(10),
            counter_next: Instant::now(),
            counter_prev: Instant::now(),
            distribution_interval: Duration::from_millis(50),
            distribution_next: Instant::now(),
            distribution_prev: Instant::now(),
        })
    }

    pub fn refresh_counters(&mut self, now: Instant) {
        if now < self.counter_next {
            return;
        }

        let elapsed = (now - self.counter_prev).as_secs_f64();

        self.bpf.refresh_counters(now, elapsed);

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

        self.bpf.refresh_distributions(now);

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

impl Sampler for Retransmit {
    fn sample(&mut self) {
        let now = Instant::now();
        self.refresh_counters(now);
        self.refresh_distributions(now);
    }
}
