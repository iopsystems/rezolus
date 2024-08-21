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

use crate::common::bpf::*;
use crate::common::*;
use crate::samplers::tcp::stats::*;
use crate::samplers::tcp::*;

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
    interval: Interval,
}

impl PacketLatency {
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
            "{NAME} tcp_probe() BPF instruction count: {}",
            skel.progs().tcp_probe().insn_cnt()
        );
        debug!(
            "{NAME} tcp_rcv_space_adjust() BPF instruction count: {}",
            skel.progs().tcp_rcv_space_adjust().insn_cnt()
        );
        debug!(
            "{NAME} tcp_destroy_sock() BPF instruction count: {}",
            skel.progs().tcp_destroy_sock().insn_cnt()
        );

        skel.attach()
            .map_err(|e| error!("failed to attach bpf program: {e}"))?;

        let bpf = BpfBuilder::new(skel)
            .distribution("latency", &TCP_PACKET_LATENCY)
            .build();

        let now = Instant::now();

        Ok(Self {
            bpf,
            interval: Interval::new(now, config.distribution_interval(NAME)),
        })
    }
}

impl Sampler for PacketLatency {
    fn sample(&mut self) {
        let now = Instant::now();

        if self.interval.try_wait(now).is_ok() {
            METADATA_TCP_PACKET_LATENCY_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

            self.bpf.refresh_distributions();

            let elapsed = now.elapsed().as_nanos() as u64;
            METADATA_TCP_PACKET_LATENCY_RUNTIME.add(elapsed);
            let _ = METADATA_TCP_PACKET_LATENCY_RUNTIME_HISTOGRAM.increment(elapsed);
        }
    }
}
