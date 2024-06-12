use crate::samplers::network::linux::*;

const NAME: &str = "network_traffic";

#[cfg(feature = "bpf")]
mod bpf;

#[cfg(feature = "bpf")]
use bpf::*;

#[cfg(feature = "bpf")]
#[distributed_slice(NETWORK_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    // try to initialize the bpf based sampler
    if let Ok(s) = NetworkTraffic::new(config) {
        Box::new(s)
    } else {
        let metrics = vec![
            (&NETWORK_RX_BYTES, "rx_bytes"),
            (&NETWORK_RX_PACKETS, "rx_packets"),
            (&NETWORK_TX_BYTES, "tx_bytes"),
            (&NETWORK_TX_PACKETS, "tx_packets"),
        ];

        if let Ok(s) = SysfsNetSampler::new(config, NAME, metrics) {
            Box::new(s)
        } else {
            Box::new(Nop::new(config))
        }
    }
}

#[cfg(not(feature = "bpf"))]
#[distributed_slice(NETWORK_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    let metrics = vec![
        (&NETWORK_RX_BYTES, "rx_bytes"),
        (&NETWORK_RX_PACKETS, "rx_packets"),
        (&NETWORK_TX_BYTES, "tx_bytes"),
        (&NETWORK_TX_PACKETS, "tx_packets"),
    ];

    if let Ok(s) = SysfsNetSampler::new(config, NAME, metrics) {
        Box::new(s)
    } else {
        Box::new(Nop::new(config))
    }
}
