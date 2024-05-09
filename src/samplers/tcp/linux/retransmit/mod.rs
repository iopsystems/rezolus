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

const NAME: &str = "tcp_retransmit";

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::tcp::stats::*;
use crate::samplers::tcp::*;

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
    counter_interval: Interval,
    distribution_interval: Interval,
}

impl Retransmit {
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
            "{NAME} tcp_retransmit_skb() BPF instruction count: {}",
            skel.progs().tcp_retransmit_skb().insn_cnt()
        );

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let counters = vec![Counter::new(
            &TCP_TX_RETRANSMIT,
            Some(&TCP_TX_RETRANSMIT_HISTOGRAM),
        )];

        let bpf = BpfBuilder::new(skel).counters("counters", counters).build();

        let now = Instant::now();

        Ok(Self {
            bpf,
            counter_interval: Interval::new(now, config.interval(NAME)),
            distribution_interval: Interval::new(now, config.distribution_interval(NAME)),
        })
    }

    pub fn refresh_counters(&mut self, now: Instant) -> Result<(), ()> {
        let elapsed = self.counter_interval.try_wait(now)?;

        self.bpf.refresh_counters(elapsed);

        Ok(())
    }

    pub fn refresh_distributions(&mut self, now: Instant) -> Result<(), ()> {
        self.distribution_interval.try_wait(now)?;

        self.bpf.refresh_distributions();

        Ok(())
    }
}

impl Sampler for Retransmit {
    fn sample(&mut self) {
        let now = Instant::now();
        let _ = self.refresh_counters(now);
        let _ = self.refresh_distributions(now);
    }
}
