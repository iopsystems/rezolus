// use crate::common::Noop;
use std::collections::HashMap;
use std::io::Seek;
use std::io::Read;
use super::super::stats::*;
use crate::*;
use std::fs::File;
use crate::common::Counter;


#[cfg(target_os = "linux")]
#[distributed_slice(super::super::MEMORY_CLASSIC_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
	Box::new(ProcVmstat::new(config))
}

pub struct ProcVmstat {
	prev: Instant,
	next: Instant,
	interval: Duration,
	counters: HashMap<&'static str, Counter>,
	file: File,
}

impl ProcVmstat {
	#[allow(dead_code)]
	pub fn new(_config: &Config) -> Self {
		let now = Instant::now();

		let counters = HashMap::from([
			("numa_hit", Counter::new(&MEMORY_NUMA_HIT, None)),
			("numa_miss", Counter::new(&MEMORY_NUMA_MISS, None)),
			("numa_foreign", Counter::new(&MEMORY_NUMA_FOREIGN, None)),
			("numa_interleave", Counter::new(&MEMORY_NUMA_INTERLEAVE, None)),
			("numa_local", Counter::new(&MEMORY_NUMA_LOCAL, None)),
			("numa_other", Counter::new(&MEMORY_NUMA_OTHER, None)),
		]);

		Self {
			file: File::open("/proc/vmstat").expect("file not found"),
			counters,
			prev: now,
			next: now,
			interval: Duration::from_millis(100),
		}
	}
}

impl Sampler for ProcVmstat {
	fn sample(&mut self) {
		let now = Instant::now();

		if now < self.next {
			return;
		}

		let elapsed = (now - self.prev).as_secs_f64();

		if self.sample_proc_vmstat(now, elapsed).is_err() {
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

impl ProcVmstat {
	fn sample_proc_vmstat(&mut self, now: Instant, elapsed: f64) -> Result<(), std::io::Error> {
		self.file.rewind()?;

		let mut data = String::new();
		self.file.read_to_string(&mut data)?;

		let lines = data.lines();

		for line in lines {
			let parts: Vec<&str> = line.split_whitespace().collect();

			if parts.is_empty() {
				continue;
			}

			if let Some(counter) = self.counters.get_mut(*parts.first().unwrap()) {
				if let Some(Ok(v)) = parts.get(1).map(|v| v.parse::<u64>()) {
					counter.set(now, elapsed, v);
				}
			}
		}

		Ok(())
	}
}

impl Display for ProcVmstat {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
		write!(f, "cpu::classic::proc_vmstat")
	}
}