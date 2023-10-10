use serde::Serialize;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Result;
use std::path::Path;
use walkdir::DirEntry;
use walkdir::WalkDir;

mod cache;
mod cpu;
mod memory;
mod net;
mod node;

use cache::*;
use cpu::*;
use memory::*;
use net::*;
use node::*;

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HwInfo {
    pub caches: Vec<Vec<Cache>>,
    pub cpus: Vec<Cpu>,
    pub memory: Memory,
    pub network: Vec<Interface>,
    pub nodes: Vec<Node>,
}

impl HwInfo {
    pub fn new() -> Result<Self> {
        Ok(Self {
            caches: get_caches()?,
            cpus: get_cpus()?,
            memory: Memory::new()?,
            network: get_interfaces(),
            nodes: get_nodes()?,
        })
    }

    pub fn get_cpus(&self) -> &Vec<Cpu> {
        return &self.cpus;
    }
}

fn read_usize(path: impl AsRef<Path>) -> Result<usize> {
    let raw = std::fs::read_to_string(path)?;
    let raw = raw.trim();

    raw.parse()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "not a number"))
}

fn read_string(path: impl AsRef<Path>) -> Result<String> {
    let raw = std::fs::read_to_string(path)?;
    let raw = raw.trim();

    Ok(raw.to_string())
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
            ret.push(parts[0].parse().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "not a number")
            })?);
        } else if parts.len() == 2 {
            let start: usize = parts[0].parse().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "not a number")
            })?;
            let stop: usize = parts[1].parse().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "not a number")
            })?;
            for i in start..=stop {
                ret.push(i);
            }
        }
    }

    Ok(ret)
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
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
