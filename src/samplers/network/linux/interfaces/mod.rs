use crate::common::{Interval, Nop};
use crate::samplers::hwinfo::hardware_info;
use crate::samplers::network::stats::*;
use crate::samplers::network::*;
use metriken::Counter;
use std::fs::File;
use std::io::Read;
use std::io::Seek;

#[distributed_slice(NETWORK_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = Interfaces::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop::new(config))
    }
}

const NAME: &str = "network_interfaces";

pub struct Interfaces {
    interval: Interval,
    stats: Vec<(&'static Lazy<Counter>, &'static str, HashMap<String, File>)>,
}

impl Interfaces {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let hwinfo = hardware_info().map_err(|e| {
            error!("failed to load hardware info: {e}");
        })?;

        let mut metrics = vec![
            (&NETWORK_RX_CRC_ERRORS, "rx_crc_errors"),
            (&NETWORK_RX_DROPPED, "rx_dropped"),
            (&NETWORK_RX_MISSED_ERRORS, "rx_missed_errors"),
            (&NETWORK_TX_DROPPED, "tx_dropped"),
        ];

        let mut stats = Vec::new();
        let mut d = String::new();

        for (counter, stat) in metrics.drain(..) {
            let mut if_stats = HashMap::new();

            for interface in &hwinfo.network {
                if interface.driver.is_none() {
                    continue;
                }

                if let Ok(mut f) = std::fs::File::open(&format!(
                    "/sys/class/net/{}/statistics/{stat}",
                    interface.name
                )) {
                    if f.read_to_string(&mut d).is_ok() && d.parse::<u64>().is_ok() {
                        if_stats.insert(interface.name.to_string(), f);
                    }
                }
            }

            stats.push((counter, stat, if_stats));
        }

        Ok(Self {
            stats,
            interval: Interval::new(Instant::now(), config.interval(NAME)),
        })
    }
}

impl Sampler for Interfaces {
    fn sample(&mut self) {
        if self.interval.try_wait(Instant::now()).is_err() {
            return;
        }

        let mut data = String::new();

        'outer: for (counter, _stat, ref mut if_stats) in &mut self.stats {
            let mut sum = 0;

            for file in if_stats.values_mut() {
                if file.rewind().is_ok() {
                    if let Err(e) = file.read_to_string(&mut data) {
                        error!("error reading: {e}");
                        continue 'outer;
                    }

                    if let Ok(v) = data.parse::<u64>() {
                        sum += v;
                    } else {
                        continue 'outer;
                    }
                }
            }

            counter.set(sum);
        }
    }
}
