use std::io::BufRead;
use std::io::BufReader;
use std::fs::File;
use std::io::Result;

// use serde_derive::{Deserialize, Serialize};
// use serde_json::Result;

use serde::Serialize;

#[derive(Serialize)]
pub struct Hwinfo {
	memory: Memory,
}

impl Hwinfo {
	pub fn new() -> Result<Self> {
		Ok(Self {
			memory: Memory::new()?,
		})
	}
}

#[derive(Serialize)]
pub struct Memory {
	total_bytes: u64,
}

impl Memory {
	pub fn total_bytes(&self) -> u64 {
		self.total_bytes
	}
}

impl Memory {
	pub fn new() -> Result<Self> {
		let file = File::open("/proc/meminfo")?;
		let reader = BufReader::new(file);

		let mut ret = Self {
			total_bytes: 0,
		};

		for line in reader.lines() {
			let line = line.unwrap();
			if line.starts_with("MemTotal:") {
				let parts: Vec<&str> = line.split_whitespace().collect();
				if parts.len() == 3 {
					ret.total_bytes = parts[1].parse::<u64>().map(|v| v * 1024).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad value"))?;
				}
			}
		}

		Ok(ret)
	}
}