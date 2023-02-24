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
	tcp_rx_segs: u64,
}

impl Snmp {
	pub fn new(_config: &Config) -> Self {
		let now = Instant::now();
		Self {
			prev: now,
			next: now,
			interval: Duration::from_millis(100),
			file: File::open("/proc/net/snmp").expect("file not found"),
			tcp_rx_segs: 0,
		}
	}
}

impl Sampler for Snmp {
	fn sample(&mut self) {

		let now = Instant::now();

		if now < self.next {
			return;
		}

		if now > self.next + self.interval {
			println!("this is not good");
		}

		SAMPLERS_TCP_CLASSIC_SNMP_SAMPLE.increment();

		let first_run = self.prev == self.next;

		let elapsed = (now - self.prev).as_secs_f64();

		if let Ok(nested_map) = NestedMap::try_from_procfs(&mut self.file) {
			if let Some(v) = nested_map.get("Tcp:", "InSegs") {
				if !first_run {
					let delta = v.wrapping_sub(self.tcp_rx_segs);
					TCP_RX_SEGS.add(delta);
					TCP_RX_SEGS_HIST.increment(now, (delta as f64 / elapsed) as _, 1);
					self.tcp_rx_segs = v;
				} else {
					self.tcp_rx_segs = v;
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