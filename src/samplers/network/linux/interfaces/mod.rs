use crate::samplers::network::linux::*;

#[distributed_slice(NETWORK_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    let metrics = vec![
        (&NETWORK_RX_CRC_ERRORS, "rx_crc_errors"),
        (&NETWORK_RX_DROPPED, "rx_dropped"),
        (&NETWORK_RX_MISSED_ERRORS, "rx_missed_errors"),
        (&NETWORK_TX_DROPPED, "tx_dropped"),
    ];

    if let Ok(s) = SysfsNetSampler::new(config, NAME, metrics) {
        Box::new(s)
    } else {
        Box::new(Nop::new(config))
    }
}

const NAME: &str = "network_interfaces";
