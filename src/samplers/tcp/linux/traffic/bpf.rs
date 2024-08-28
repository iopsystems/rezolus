#[allow(clippy::module_inception)]
mod bpf {
    include!(concat!(env!("OUT_DIR"), "/tcp_traffic.bpf.rs"));
}

use super::NAME;

use bpf::*;

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::tcp::stats::*;
use crate::samplers::tcp::*;

impl GetMap for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map {
        match name {
            "counters" => &self.maps.counters,
            "rx_size" => &self.maps.rx_size,
            "tx_size" => &self.maps.tx_size,
            _ => unimplemented!(),
        }
    }
}

/// Collects TCP Traffic stats using BPF and traces:
/// * `tcp_sendmsg`
/// * `tcp_cleanup_rbuf`
///
/// And produces these stats:
/// * `tcp/receive/bytes`
/// * `tcp/receive/segments`
/// * `tcp/receive/size`
/// * `tcp/transmit/bytes`
/// * `tcp/transmit/segments`
/// * `tcp/transmit/size`
pub struct TcpTraffic {
    bpf: Bpf<ModSkel<'static>>,
    interval: Interval,
}

impl TcpTraffic {
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
            "{NAME} tcp_sendmsg() BPF instruction count: {}",
            skel.progs.tcp_sendmsg.insn_cnt()
        );
        debug!(
            "{NAME} tcp_cleanup_rbuf() BPF instruction count: {}",
            skel.progs.tcp_cleanup_rbuf.insn_cnt()
        );

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let counters = vec![
            Counter::new(&TCP_RX_BYTES, Some(&TCP_RX_BYTES_HISTOGRAM)),
            Counter::new(&TCP_TX_BYTES, Some(&TCP_TX_BYTES_HISTOGRAM)),
            Counter::new(&TCP_RX_PACKETS, Some(&TCP_RX_PACKETS_HISTOGRAM)),
            Counter::new(&TCP_TX_PACKETS, Some(&TCP_TX_PACKETS_HISTOGRAM)),
        ];

        let bpf = BpfBuilder::new(skel)
            .counters("counters", counters)
            .distribution("rx_size", &TCP_RX_SIZE)
            .distribution("tx_size", &TCP_TX_SIZE)
            .build();

        let now = Instant::now();

        Ok(Self {
            bpf,
            interval: Interval::new(now, config.interval(NAME)),
        })
    }

    pub fn refresh(&mut self, now: Instant) -> Result<(), ()> {
        let elapsed = self.interval.try_wait(now)?;

        METADATA_TCP_TRAFFIC_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

        self.bpf.refresh(elapsed);

        Ok(())
    }
}

impl Sampler for TcpTraffic {
    fn sample(&mut self) {
        let now = Instant::now();

        if self.refresh(now).is_ok() {
            let elapsed = now.elapsed().as_nanos() as u64;
            METADATA_TCP_TRAFFIC_RUNTIME.add(elapsed);
            let _ = METADATA_TCP_TRAFFIC_RUNTIME_HISTOGRAM.increment(elapsed);
        }
    }
}
