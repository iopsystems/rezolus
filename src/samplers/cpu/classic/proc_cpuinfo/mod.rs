// use crate::common::Noop;
use std::io::Seek;
use std::io::Read;
use super::super::stats::*;
use crate::*;
use std::fs::File;
use super::super::*;


#[distributed_slice(CPU_CLASSIC_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
	Box::new(ProcCpuinfo::new(config))
}

pub struct ProcCpuinfo {
	prev: Instant,
	next: Instant,
	interval: Duration,
	file: File,
}

impl ProcCpuinfo {
	pub fn new(_config: &Config) -> Self {
		let now = Instant::now();

		Self {
			file: File::open("/proc/cpuinfo").expect("file not found"),
			prev: now,
			next: now,
			interval: Duration::from_millis(100),
		}
	}
}

impl Sampler for ProcCpuinfo {
	fn sample(&mut self) {
		let now = Instant::now();

		if now < self.next {
			return;
		}

		if self.sample_proc_cpuinfo(now).is_err() {
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

impl ProcCpuinfo {
	fn sample_proc_cpuinfo(&mut self, now: Instant) -> Result<(), std::io::Error> {
		self.file.rewind()?;

		let mut data = String::new();
		self.file.read_to_string(&mut data)?;

		let lines = data.lines();

		for line in lines {
			let parts: Vec<&str> = line.split_whitespace().collect();

			if let (Some(&"cpu"), Some(&"MHz")) = (parts.first(), parts.get(1)) {
				if let Some(Ok(freq)) = parts.get(3).map(|v| v.parse::<f64>().map(|v| v.floor() as u64)) {
					CPU_FREQUENCY.increment(now, freq, 1);
				}
			}
		}

		Ok(())
	}
}

impl Display for ProcCpuinfo {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
		write!(f, "cpu::classic::proc_cpuinfo")
	}
}