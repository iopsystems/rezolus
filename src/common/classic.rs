use std::io::BufRead;
use std::iter::zip;
use std::io::{Error, ErrorKind};
use std::io::BufReader;
use std::collections::HashMap;
use std::io::Seek;
use std::fs::File;

pub struct NestedMap {
	inner: HashMap<String, HashMap<String, u64>>
}

impl NestedMap {
	pub fn get(&self, pkey: &str, lkey: &str) -> Option<u64> {
		self.inner.get(pkey)?.get(lkey).copied()
	}

	/// Tries to create a new NestedMap from a file that would be found in procfs
	/// such as `/proc/net/snmp` with the following format:
	/// ```plain
	/// pkey1 lkey1 ... lkeyN
	/// pkey1 value1 ... valueN
	/// ...
	/// pkeyN lkey1 ... lkeyN
	/// pkeyN value1 ... lkeyN
	/// ```
	pub fn try_from_procfs(file: &mut File) -> Result<Self, std::io::Error> {
		// seek to start to cause reload of content
		file.rewind()?;

		let mut reader = BufReader::new(file);
		let mut inner = HashMap::new();

		let mut k_line = String::new();
		let mut v_line = String::new();

		loop {
			if reader.read_line(&mut k_line)? == 0 {
				break;
			}
			if reader.read_line(&mut v_line)? == 0 {
				break;
			}

			let keys: Vec<&str> = k_line.split_whitespace().collect();
			let values: Vec<&str> = v_line.split_whitespace().collect();

			if keys.is_empty() || values.is_empty() {
				continue;
			}

			if keys[0] != values[0] {
				debug!("pkey mismatch parsing nested map: {} != {}", keys[0], values[0]);
				return Err(Error::new(ErrorKind::InvalidData, "pkey mismatch"));
			}

			let mut map = HashMap::with_capacity(keys.len() - 1);
			for (key, value) in zip(keys.iter().skip(1).map(|k| k.to_owned()), values.iter().skip(1)) {
				if let Ok(value) = value.parse::<u64>() {
					map.insert(key.to_owned(), value);
				} else {
					return Err(Error::new(ErrorKind::InvalidData, "value was not valid u64"));
				}
			}

			inner.insert(keys[0].to_owned(), map);
		}

		Ok(Self {
			inner,
		})
	}
}