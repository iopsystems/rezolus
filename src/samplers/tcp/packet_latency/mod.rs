#[distributed_slice(TCP_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = PacketLatency::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop {})
    }
}

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_packet_latency.bpf.rs"));
}

const NAME: &str = "tcp_packet_latency";

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

/// Collects TCP packet latency stats using BPF and traces:
/// * `tcp_destroy_sock`
/// * `tcp_probe`
/// * `tcp_rcv_space_adjust`
///
/// And produces these stats:
/// * `tcp/receive/packet_latency`
pub struct PacketLatency {
    bpf: Bpf<ModSkel<'static>>,
    counter_interval: Duration,
    counter_next: Instant,
    counter_prev: Instant,
    distribution_interval: Duration,
    distribution_next: Instant,
    distribution_prev: Instant,
}

impl PacketLatency {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let builder = ModSkelBuilder::default();
        let mut skel = builder
            .open()
            .map_err(|e| error!("failed to open bpf builder: {e}"))?
            .load()
            .map_err(|e| error!("failed to load bpf program: {e}"))?;

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let mut bpf = Bpf::from_skel(skel);

        let mut distributions = vec![("latency", &TCP_PACKET_LATENCY)];

        for (name, histogram) in distributions.drain(..) {
            bpf.add_distribution(name, histogram);
        }

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

        let elapsed = (now - self.counter_prev).as_secs_f64();

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

impl Sampler for PacketLatency {
    fn sample(&mut self) {
        let now = Instant::now();
        self.refresh_counters(now);
        self.refresh_distributions(now);
    }
}
