use std::collections::HashMap;
use std::io::Read;

use metriken::LazyCounter;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub struct SysfsSampler {
    stats: Vec<(&'static LazyCounter, &'static str, HashMap<String, File>)>,
}

impl SysfsSampler {
    pub fn new(
        mut metrics: Vec<(&'static LazyCounter, &'static str)>,
    ) -> Result<Self, std::io::Error> {
        let interfaces = crate::common::linux::network_interfaces()?;

        let mut stats = Vec::new();
        let mut data = String::new();

        for (counter, stat) in metrics.drain(..) {
            let mut if_stats = HashMap::new();

            for interface in &interfaces {
                let mut f =
                    std::fs::File::open(format!("/sys/class/net/{}/statistics/{stat}", interface))?;

                data.clear();

                if f.read_to_string(&mut data).is_ok() && data.trim_end().parse::<u64>().is_ok() {
                    if_stats.insert(interface.to_string(), File::from_std(f));
                }
            }

            stats.push((counter, stat, if_stats));
        }

        Ok(Self { stats })
    }

    pub async fn refresh(&mut self) {
        let mut data = String::new();

        'outer: for (counter, _stat, ref mut if_stats) in &mut self.stats {
            let mut sum = 0;

            for file in if_stats.values_mut() {
                if file.rewind().await.is_ok() {
                    data.clear();

                    if file.read_to_string(&mut data).await.is_err() {
                        continue 'outer;
                    }

                    if let Ok(v) = data.trim_end().parse::<u64>() {
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
