use std::io::Read;
use std::iter::zip;
use std::io::{Error, ErrorKind};
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

		let mut data = String::new();
		file.read_to_string(&mut data)?;

		let mut inner = HashMap::new();

		let mut lines = data.lines();

		loop {
			let k_line = lines.next();
			if k_line.is_none() {
				break;
			}

			let v_line = lines.next();
			if v_line.is_none() {
				break;
			}

			let keys: Vec<&str> = k_line.unwrap().split_whitespace().collect();
			let values: Vec<&str> = v_line.unwrap().split_whitespace().collect();

			if keys.is_empty() || values.is_empty() {
				continue;
			}

			if keys[0] != values[0] {
				println!("pkey mismatch parsing nested map: {} != {}", keys[0], values[0]);
				return Err(Error::new(ErrorKind::InvalidData, "pkey mismatch"));
			}

			let mut map = HashMap::with_capacity(keys.len() - 1);
			for (key, value) in zip(keys.iter().skip(1).map(|k| k.to_owned()), values.iter().skip(1)) {
				if let Ok(value) = value.parse::<u64>() {
					map.insert(key.to_owned(), value);
				}
			}

			inner.insert(keys[0].to_owned(), map);
		}

		Ok(Self {
			inner,
		})
	}
}