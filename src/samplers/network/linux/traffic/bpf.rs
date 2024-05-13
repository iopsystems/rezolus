#[allow(clippy::module_inception)]
mod bpf {
    include!(concat!(env!("OUT_DIR"), "/network_traffic.bpf.rs"));
}

use super::NAME;

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::network::stats::*;
use crate::samplers::network::*;

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        self.obj.map(name).unwrap()
    }
}

/// Collects Network Traffic stats using BPF and traces:
/// * `netif_receive_skb`
/// * `netdev_start_xmit`
///
/// And produces these stats:
/// * `network/receive/bytes`
/// * `network/receive/frames`
/// * `network/transmit/bytes`
/// * `network/transmit/frames`
pub struct NetworkTraffic {
    bpf: Bpf<ModSkel<'static>>,
    counter_interval: Interval,
    distribution_interval: Interval,
}

impl NetworkTraffic {
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
            "{NAME} netif_receive_skb() BPF instruction count: {}",
            skel.progs().netif_receive_skb().insn_cnt()
        );
        debug!(
            "{NAME} tcp_cleanup_rbuf() BPF instruction count: {}",
            skel.progs().tcp_cleanup_rbuf().insn_cnt()
        );

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let counters = vec![
            Counter::new(&NETWORK_RX_BYTES, Some(&NETWORK_RX_BYTES_HISTOGRAM)),
            Counter::new(&NETWORK_TX_BYTES, Some(&NETWORK_TX_BYTES_HISTOGRAM)),
            Counter::new(&NETWORK_RX_PACKETS, Some(&NETWORK_RX_PACKETS_HISTOGRAM)),
            Counter::new(&NETWORK_TX_PACKETS, Some(&NETWORK_TX_PACKETS_HISTOGRAM)),
        ];

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

impl Sampler for NetworkTraffic {
    fn sample(&mut self) {
        let now = Instant::now();
        let _ = self.refresh_counters(now);
        let _ = self.refresh_distributions(now);
    }
}
