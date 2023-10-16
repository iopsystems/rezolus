use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use super::util::*;
use crate::{Error, Result};

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Memory {
    pub total_bytes: u64,
}

impl Memory {
    pub fn new() -> Result<Self> {
        Self::from_file("/proc/meminfo")
    }

    pub fn node(id: usize) -> Result<Self> {
        Self::from_file(format!("/sys/devices/system/node/node{id}/meminfo"))
    }

    fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|e| Error::unreadable(e, path))?;
        let mut reader = BufReader::new(file);

        let mut memory = Self { total_bytes: 0 };
        let mut line = String::new();

        while reader
            .read_line(&mut line)
            .map_err(|e| Error::unreadable(e, path))?
            != 0
        {
            let line = ClearGuard::new(&mut line);

            if !line.starts_with("MemTotal:") {
                continue;
            }

            let mut parts = line.split_ascii_whitespace().skip(1);
            let Some(value) = parts.next() else {
                continue;
            };
            let Some(_unit) = parts.next() else {
                continue;
            };

            if parts.next().is_some() {
                continue;
            }

            let kilobytes: u64 = value.parse().map_err(|e| Error::unparseable(e, path))?;
            memory.total_bytes = kilobytes * 1024;
        }

        Ok(memory)
    }
}
