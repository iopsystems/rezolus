use crate::common::classic::NestedMap;
use crate::common::*;
use crate::samplers::tcp::stats::*;
use crate::samplers::tcp::*;

pub struct ProcNetSnmp {
    interval: AsyncInterval,
    counters: Vec<(Counter, &'static str, &'static str)>,
    file: tokio::fs::File,
}

impl ProcNetSnmp {
    pub fn new(interval: AsyncInterval) -> Result<Self, ()> {
        let counters = vec![
            (
                Counter::new(&TCP_RX_PACKETS,  None),
                "Tcp:",
                "InSegs",
            ),
            (
                Counter::new(&TCP_TX_PACKETS,  None),
                "Tcp:",
                "OutSegs",
            ),
        ];

        let file = std::fs::File::open("/proc/net/snmp")
            .map(|f| tokio::fs::File::from_std(f))
            .map_err(|e| {
                error!("failed to open: /proc/net/snmp error: {e}");
            })?;

        Ok(Self {
            interval,
            counters,
            file,
        })
    }
}

#[async_trait]
impl AsyncSampler for ProcNetSnmp {
    async fn sample(&mut self) {
        let (now, elapsed) = self.interval.tick().await;

        METADATA_TCP_TRAFFIC_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

        if let Ok(nested_map) = NestedMap::try_from_procfs(&mut self.file).await {
            for (counter, pkey, lkey) in self.counters.iter_mut() {
                if let Some(curr) = nested_map.get(pkey, lkey) {
                    counter.set2(elapsed, curr);
                }
            }
        }

        let elapsed = now.elapsed().as_nanos() as u64;
        METADATA_TCP_TRAFFIC_RUNTIME.add(elapsed);
        let _ = METADATA_TCP_TRAFFIC_RUNTIME_HISTOGRAM.increment(elapsed);
    }
}
