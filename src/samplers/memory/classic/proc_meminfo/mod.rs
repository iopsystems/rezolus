use crate::common::LazyGauge;
use metriken::MetricInstance;

use std::collections::HashMap;
use std::io::{Read, Seek};
use super::super::stats::*;
use crate::*;
use std::fs::File;

#[cfg(target_os = "linux")]
#[distributed_slice(super::super::MEMORY_CLASSIC_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
	Box::new(ProcMeminfo::new(config))
}

pub struct ProcMeminfo {
	prev: Instant,
	next: Instant,
	interval: Duration,
	file: File,
	gauges: HashMap<&'static str, &'static MetricInstance<LazyGauge>>,
}

impl ProcMeminfo {
	#![allow(dead_code)]
	pub fn new(_config: &Config) -> Self {
		let now = Instant::now();

		let gauges = HashMap::from([
			("MemTotal:", &MEMORY_TOTAL),
			("MemFree:", &MEMORY_FREE),
			("MemAvailable:", &MEMORY_AVAILABLE),
			("Buffers:", &MEMORY_BUFFERS),
			("Cached:", &MEMORY_CACHED),
		]);

		Self {
			file: File::open("/proc/meminfo").expect("file not found"),
			gauges,
			prev: now,
			next: now,
			interval: Duration::from_millis(100),
		}
	}
}

impl Sampler for ProcMeminfo {
	fn sample(&mut self) {
		let now = Instant::now();

		if now < self.next {
			return;
		}

		if self.sample_proc_meminfo(now).is_err() {
			return;
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

impl ProcMeminfo {
	fn sample_proc_meminfo(&mut self, _now: Instant) -> Result<(), std::io::Error> {
		self.file.rewind()?;

		let mut data = String::new();
		self.file.read_to_string(&mut data)?;

		let lines = data.lines();

		for line in lines {
			let parts: Vec<&str> = line.split_whitespace().collect();
			if parts.is_empty() {
				continue;
			}

			if let Some(gauge) = self.gauges.get_mut(*parts.first().unwrap()) {
				if let Some(Ok(v)) = parts.get(1).map(|v| v.parse::<i64>()) {
					gauge.set(v);
				}
			}
		}

		Ok(())
	}
}

impl Display for ProcMeminfo {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
		write!(f, "cpu::classic::proc_meminfo")
	}
}

#[cfg(test)]
mod test {

}