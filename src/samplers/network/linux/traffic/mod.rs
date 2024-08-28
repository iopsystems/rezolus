use crate::common::*;
use crate::samplers::network::linux::*;

const NAME: &str = "network_traffic";

#[cfg(feature = "bpf")]
mod bpf;

#[cfg(feature = "bpf")]
#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> Box<dyn Sampler> {
    // try to initialize the bpf based sampler
    if let Ok(s) = bpf::NetworkTraffic::new(config.clone()) {
        Box::new(s)
    } else {
        if let Ok(s) = NetworkTraffic::new(config) {
            Box::new(s)
        } else {
            Box::new(Nop { })
        }
    }
}

#[cfg(not(feature = "bpf"))]
#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> Box<dyn Sampler> {
    if let Ok(s) = NetworkTraffic::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop { })
    }
}

struct NetworkTraffic {
    inner: SysfsNetSampler,
    interval: Interval,
}

impl NetworkTraffic {
    pub fn new(config: Arc<Config>) -> Result<Self, ()> {
        let metrics = vec![
            (&NETWORK_RX_BYTES, "rx_bytes"),
            (&NETWORK_RX_PACKETS, "rx_packets"),
            (&NETWORK_TX_BYTES, "tx_bytes"),
            (&NETWORK_TX_PACKETS, "tx_packets"),
        ];

        Ok(Self {
            inner: SysfsNetSampler::new(config.clone(), NAME, metrics)?,
            interval: Interval::new(Instant::now(), config.interval(NAME)),
        })
    }
}

impl Sampler for NetworkTraffic {
    fn sample(&mut self) {
        let now = Instant::now();

        if let Ok(_) = self.interval.try_wait(now) {
            METADATA_NETWORK_TRAFFIC_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

            let _ = self.inner.sample_now();

            let elapsed = now.elapsed().as_nanos() as u64;
            METADATA_NETWORK_TRAFFIC_RUNTIME.add(elapsed);
            let _ = METADATA_NETWORK_TRAFFIC_RUNTIME_HISTOGRAM.increment(elapsed);
        }
    }
}
