use crate::common::classic::NestedMap;
use crate::common::Counter;
use crate::common::Nop;
use crate::samplers::tcp::stats::*;
use crate::samplers::tcp::*;
use crate::*;
use std::fs::File;

#[distributed_slice(TCP_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    // if we have bpf enabled, we don't need to run this sampler at all
    if config.bpf() {
        Box::new(Nop::new(config))
    } else {
        Box::new(Snmp::new(config))
    }
}

pub struct Snmp {
    prev: Instant,
    next: Instant,
    interval: Duration,
    file: File,
    counters: Vec<(Counter, &'static str, &'static str)>,
}

impl Snmp {
    pub fn new(_config: &Config) -> Self {
        let now = Instant::now();

        let counters = vec![
            (
                Counter::new(&TCP_RX_SEGMENTS, Some(&TCP_RX_SEGMENTS_HEATMAP)),
                "Tcp:",
                "InSegs",
            ),
            (
                Counter::new(&TCP_TX_SEGMENTS, Some(&TCP_TX_SEGMENTS_HEATMAP)),
                "Tcp:",
                "OutSegs",
            ),
        ];

        Self {
            file: File::open("/proc/net/snmp").expect("file not found"),
            counters,
            prev: now,
            next: now,
            interval: Duration::from_millis(100),
        }
    }
}

impl Sampler for Snmp {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let elapsed = (now - self.prev).as_secs_f64();

        if let Ok(nested_map) = NestedMap::try_from_procfs(&mut self.file) {
            for (counter, pkey, lkey) in self.counters.iter_mut() {
                if let Some(curr) = nested_map.get(pkey, lkey) {
                    counter.set(now, elapsed, curr);
                }
            }
        }

        // determine when to sample next
        let next = self.next + self.interval;

        // it's possible we fell behind
        if next > now {
            // if we didn't, sample at the next planned time
            self.next = next;
        } else {
            // if we did, sample after the interval has elapsed
            self.next = now + self.interval;
        }

        // mark when we last sampled
        self.prev = now;
    }
}
