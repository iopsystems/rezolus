use crate::samplers::tcp::stats::*;
use crate::*;
use std::fs::File;
use crate::common::classic::NestedMap;
use crate::samplers::tcp::TCP_SAMPLERS;

#[distributed_slice(TCP_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    Box::new(Snmp::new(config))
}

pub struct Snmp {
	prev: Instant,
	next: Instant,
	interval: Duration,
	file: File,
	tcp_rx_segs: Option<u64>,
	tcp_tx_segs: Option<u64>,
}

impl Snmp {
	pub fn new(_config: &Config) -> Self {
		let now = Instant::now();
		Self {
			prev: now,
			next: now,
			interval: Duration::from_millis(100),
			file: File::open("/proc/net/snmp").expect("file not found"),
			tcp_rx_segs: None,
			tcp_tx_segs: None,
		}
	}
}

impl Sampler for Snmp {
	fn sample(&mut self) {

		let now = Instant::now();

		if now < self.next {
			return;
		}

		SAMPLERS_TCP_CLASSIC_SNMP_SAMPLE.increment();

		let elapsed = (now - self.prev).as_secs_f64();

		let counters = [
			(&mut self.tcp_rx_segs, &TCP_RX_SEGS, &TCP_RX_SEGS_HIST, "Tcp:", "InSegs"),
			(&mut self.tcp_tx_segs, &TCP_TX_SEGS, &TCP_TX_SEGS_HIST, "Tcp:", "OutSegs"),
		];

		if let Ok(nested_map) = NestedMap::try_from_procfs(&mut self.file) {
			for (prev, cnt, hist, pkey, lkey) in counters {
				if let Some(curr) = nested_map.get(pkey, lkey) {
					if let Some(p) = *prev {
						let delta = curr.wrapping_sub(p);
						cnt.add(delta);
						hist.increment(now, (delta as f64 / elapsed) as _, 1);
					}
					*prev = Some(curr);
				}
			}
		} else {
			SAMPLERS_TCP_CLASSIC_SNMP_SAMPLE_EX.increment();
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

impl Display for Snmp {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
		write!(f, "tcp::classic::snmp")
	}
}