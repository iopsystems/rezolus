use std::path::Path;
use std::io::BufRead;
use std::io::BufReader;
use std::fs::File;
use std::io::Result;

// use serde_derive::{Deserialize, Serialize};
// use serde_json::Result;

use serde::Serialize;

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheType {
	Data,
	Instruction,
	Unified,
}

#[derive(Clone, Serialize)]
pub struct Cache {
	coherency_line_size: usize,
	number_of_sets: usize,
	shared_cpus: Vec<usize>,
	size: String,
	r#type: CacheType,
	ways_of_associativity: usize,
}

impl Cache {
	pub fn new(cpu: usize, index: usize) -> Result<Self> {
		let coherency_line_size = read_usize(format!("/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/coherency_line_size"))?;
		let number_of_sets = read_usize(format!("/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/number_of_sets"))?;
		let shared_cpus = read_list(format!("/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/shared_cpu_list"))?;
		let size = read_string(format!("/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/size"))?;
		let r#type = read_cache_type(format!("/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/type"))?;
		let ways_of_associativity = read_usize(format!("/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/ways_of_associativity"))?;
		
		Ok(Cache {
			coherency_line_size,
			number_of_sets,
			shared_cpus,
			size,
			r#type,
			ways_of_associativity,
		})
	}
}

#[derive(Serialize)]
pub struct Hwinfo {
	caches: Vec<Vec<Cache>>,
	cpus: Vec<Cpu>,
	memory: Memory,
	nodes: Vec<Node>,
}

impl Hwinfo {
	pub fn new() -> Result<Self> {
		Ok(Self {
			caches: get_caches()?,
			cpus: get_cpus()?,
			memory: Memory::new()?,
			nodes: get_nodes()?,
		})
	}
}

fn read_usize(path: impl AsRef<Path>) -> Result<usize> {
	let raw = std::fs::read_to_string(path)?;
	let raw = raw.trim();

	raw.parse().map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not a number"))
}

fn read_string(path: impl AsRef<Path>) -> Result<String> {
	let raw = std::fs::read_to_string(path)?;
	let raw = raw.trim();

	Ok(raw.to_string())
}

fn read_cache_type(path: impl AsRef<Path>) -> Result<CacheType> {
	let raw = std::fs::read_to_string(path)?;
	let raw = raw.trim();

	match raw {
		"Data" | "data" => Ok(CacheType::Data),
		"Instruction" | "instruction" => Ok(CacheType::Instruction),
		"Unified" | "unified" => Ok(CacheType::Unified),
		_ => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "unexpected cache type")),
	}
}

fn read_list(path: impl AsRef<Path>) -> Result<Vec<usize>> {
	let raw = std::fs::read_to_string(path)?;
	parse_list(raw)
}

fn parse_list(raw: String) -> Result<Vec<usize>> {
	let raw = raw.trim();
	let mut ret = Vec::new();
	let ranges: Vec<&str> = raw.split(',').collect();
	for range in ranges {
		let parts: Vec<&str> = range.split('-').collect();
		if parts.len() == 1 {
			ret.push(parts[0].parse().map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not a number"))?);
		} else if parts.len() == 2 {
			let start: usize = parts[0].parse().map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not a number"))?;
			let stop: usize = parts[1].parse().map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not a number"))?;
			for i in start..=stop {
				ret.push(i);
			}
		}
	}

	Ok(ret)
}

fn get_nodes() -> Result<Vec<Node>> {
	let mut ret = Vec::new();

	let ids = read_list("/sys/devices/system/node/online")?;

	for id in ids {
		let memory = Memory::node(id)?;
		let cpus = read_list(format!("/sys/devices/system/node/node{id}/cpulist"))?;
		ret.push(Node { id, cpus, memory });
	}

	Ok(ret)
}

fn get_caches() -> Result<Vec<Vec<Cache>>> {
	// This is sufficient for up to four caches: L1i, L1d, L2, L3
	let max_cache_index = 3; // inclusive

	let mut ret = vec![vec![]; max_cache_index];

	let cpu_ids = read_list("/sys/devices/system/cpu/online")?;
	
	for (index, caches) in ret.iter_mut().enumerate() {
		for cpu_id in &cpu_ids {
			let cache = Cache::new(*cpu_id, index)?;

			if cache.shared_cpus[0] != *cpu_id {
				continue;
			}

			caches.push(cache);
		}
	}

	Ok(ret)
}

fn get_cpus() -> Result<Vec<Cpu>> {
	let mut ret = Vec::new();

	let ids = read_list("/sys/devices/system/cpu/online")?;

	for id in ids {
		let core_id = read_usize(format!("/sys/devices/system/cpu/cpu{id}/topology/core_id"))?;
		let die_id = read_usize(format!("/sys/devices/system/cpu/cpu{id}/topology/die_id"))?;
		let package_id = read_usize(format!("/sys/devices/system/cpu/cpu{id}/topology/physical_package_id"))?;

		let core_cpus = read_list(format!("/sys/devices/system/cpu/cpu{id}/topology/core_cpus_list"))?;
		let die_cpus = read_list(format!("/sys/devices/system/cpu/cpu{id}/topology/die_cpus_list"))?;
		let package_cpus = read_list(format!("/sys/devices/system/cpu/cpu{id}/topology/package_cpus_list"))?;

		let core_siblings = read_list(format!("/sys/devices/system/cpu/cpu{id}/topology/core_siblings_list"))?;
		let thread_siblings = read_list(format!("/sys/devices/system/cpu/cpu{id}/topology/thread_siblings_list"))?;

		let microcode = match read_string(format!("/sys/devices/system/cpu/cpu{id}/microcode/version")) {
			Ok(s) => {
				let parts: Vec<&str> = s.split('x').collect();
				match u64::from_str_radix(parts[parts.len() - 1], 16) {
					Ok(v) => Some(v),
					Err(_) => None,
				}
			}
			Err(_) => None
		};

		let mut caches = Vec::new();

		for index in 0..4 {
			if let Ok(cache) = Cache::new(id, index) {
				caches.push(cache);
			}
		}
		
		ret.push(Cpu {
			id,
			core_id,
			die_id,
			package_id,
			core_cpus,
			die_cpus,
			package_cpus,
			core_siblings,
			thread_siblings,
			microcode,
			caches,
		});
	}

	Ok(ret)
}

#[derive(Serialize)]
pub struct Node {
	id: usize,
	memory: Memory,
	cpus: Vec<usize>,
}

#[derive(Serialize)]
pub struct Cpu {
	id: usize,

	core_id: usize,
	die_id: usize,
	package_id: usize,

	core_cpus: Vec<usize>,
	die_cpus: Vec<usize>,
	package_cpus: Vec<usize>,

	core_siblings: Vec<usize>,
	thread_siblings: Vec<usize>,

	microcode: Option<u64>,

	caches: Vec<Cache>,
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

	pub fn node(id: usize) -> Result<Self> {
		let file = File::open(format!("/sys/devices/system/node/node{id}/meminfo"))?;
		let reader = BufReader::new(file);

		let mut ret = Self {
			total_bytes: 0,
		};

		for line in reader.lines() {
			let line = line.unwrap();
			let parts: Vec<&str> = line.split_whitespace().collect();
			if parts.len() >= 4 && parts[2] == "MemTotal:" {
				ret.total_bytes = parts[3].parse::<u64>().map(|v| v * 1024).map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "bad value"))?;
			}
		}

		Ok(ret)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn list_parsing() {
		let list = "0-1\r\n";
		assert_eq!(parse_list(list.to_string()).unwrap(), vec![0, 1]);
	}
}