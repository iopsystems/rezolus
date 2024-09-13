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
        match name {
            "counters" => &self.maps.counters,
            _ => unimplemented!(),
        }
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
    interval: Interval,
}

impl NetworkTraffic {
    pub fn new(config: Arc<Config>) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
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
            "{NAME} netif_receive_skb() BPF instruction count: {}",
            skel.progs.netif_receive_skb.insn_cnt()
        );
        debug!(
            "{NAME} tcp_cleanup_rbuf() BPF instruction count: {}",
            skel.progs.tcp_cleanup_rbuf.insn_cnt()
        );

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let counters = vec![
            Counter::new(&NETWORK_RX_BYTES, None),
            Counter::new(&NETWORK_TX_BYTES, None),
            Counter::new(&NETWORK_RX_PACKETS, None),
            Counter::new(&NETWORK_TX_PACKETS, None),
        ];

        let bpf = BpfBuilder::new(skel).counters("counters", counters).build();

        let now = Instant::now();

        Ok(Self {
            bpf,
            interval: Interval::new(now, config.interval(NAME)),
        })
    }

    pub fn refresh(&mut self, now: Instant) -> Result<(), ()> {
        let elapsed = self.interval.try_wait(now)?;

        METADATA_NETWORK_TRAFFIC_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

        self.bpf.refresh(elapsed);

        Ok(())
    }
}

impl Sampler for NetworkTraffic {
    fn sample(&mut self) {
        let now = Instant::now();

        if self.refresh(now).is_ok() {
            let elapsed = now.elapsed().as_nanos() as u64;
            METADATA_NETWORK_TRAFFIC_RUNTIME.add(elapsed);
            let _ = METADATA_NETWORK_TRAFFIC_RUNTIME_HISTOGRAM.increment(elapsed);
        }
    }
}
