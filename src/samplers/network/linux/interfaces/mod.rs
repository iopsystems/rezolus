const NAME: &str = "network_interfaces";

use crate::samplers::network::linux::stats::*;
use crate::samplers::network::linux::sysfs::SysfsSampler;
use crate::*;

use tokio::sync::Mutex;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let metrics = vec![
        (&NETWORK_CARRIER_CHANGES, "../carrier_changes"),
        (&NETWORK_RX_CRC_ERRORS, "rx_crc_errors"),
        (&NETWORK_RX_DROPPED, "rx_dropped"),
        (&NETWORK_RX_MISSED_ERRORS, "rx_missed_errors"),
        (&NETWORK_TX_DROPPED, "tx_dropped"),
    ];

    let inner = SysfsSampler::new(metrics)?;

    Ok(Some(Box::new(Interfaces {
        inner: Mutex::new(inner),
    })))
}

struct Interfaces {
    inner: Mutex<SysfsSampler>,
}

#[async_trait]
impl Sampler for Interfaces {
    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        inner.refresh().await;
    }
}
