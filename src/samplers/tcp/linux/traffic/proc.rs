use crate::common::classic::NestedMap;
use crate::common::{Counter, Interval};
use crate::samplers::tcp::stats::*;
use crate::samplers::tcp::*;
use std::fs::File;

use super::NAME;

pub struct ProcNetSnmp {
    interval: Interval,
    file: File,
    counters: Vec<(Counter, &'static str, &'static str)>,
}

impl ProcNetSnmp {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let counters = vec![
            (
                Counter::new(&TCP_RX_PACKETS, Some(&TCP_RX_PACKETS_HISTOGRAM)),
                "Tcp:",
                "InSegs",
            ),
            (
                Counter::new(&TCP_TX_PACKETS, Some(&TCP_TX_PACKETS_HISTOGRAM)),
                "Tcp:",
                "OutSegs",
            ),
        ];

        Ok(Self {
            file: File::open("/proc/net/snmp").expect("file not found"),
            counters,
            interval: Interval::new(Instant::now(), config.interval(NAME)),
        })
    }
}

impl Sampler for ProcNetSnmp {
    fn sample(&mut self) {
        if let Ok(elapsed) = self.interval.try_wait(Instant::now()) {
            if let Ok(nested_map) = NestedMap::try_from_procfs(&mut self.file) {
                for (counter, pkey, lkey) in self.counters.iter_mut() {
                    if let Some(curr) = nested_map.get(pkey, lkey) {
                        counter.set(elapsed.as_secs_f64(), curr);
                    }
                }
            }
        }
    }
}
