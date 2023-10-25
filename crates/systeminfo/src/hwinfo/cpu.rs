use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;

use super::util::*;
use super::Cache;
use crate::{Error, Result};

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cpu {
    pub id: usize,

    pub core_id: usize,
    pub die_id: usize,
    pub package_id: usize,

    pub core_cpus: Vec<usize>,
    pub die_cpus: Vec<usize>,
    pub package_cpus: Vec<usize>,

    pub core_siblings: Vec<usize>,
    pub thread_siblings: Vec<usize>,

    pub microcode: Option<String>,
    pub vendor: Option<String>,
    pub model_name: Option<String>,
    pub features: Option<HashSet<String>>,

    pub caches: Vec<Cache>,
}

impl Cpu {
    pub fn id(&self) -> usize {
        self.id
    }

    pub fn core(&self) -> usize {
        self.core_id
    }

    pub fn die(&self) -> usize {
        self.die_id
    }

    pub fn package(&self) -> usize {
        self.package_id
    }
}

pub fn get_cpus() -> Result<Vec<Cpu>> {
    let mut tmp = BTreeMap::new();

    // first read from /sys and build up some basic information
    let ids = read_list("/sys/devices/system/cpu/online")?;
    for id in ids {
        let core_id = read_usize(format!("/sys/devices/system/cpu/cpu{id}/topology/core_id"))?;
        let package_id = read_usize(format!(
            "/sys/devices/system/cpu/cpu{id}/topology/physical_package_id"
        ))?;

        // if the platform does not expose die topology, use the package id
        let die_id = read_usize(format!("/sys/devices/system/cpu/cpu{id}/topology/die_id"))
            .unwrap_or(package_id);

        let core_cpus = read_list(format!(
            "/sys/devices/system/cpu/cpu{id}/topology/core_cpus_list"
        ))?;
        let package_cpus = read_list(format!(
            "/sys/devices/system/cpu/cpu{id}/topology/package_cpus_list"
        ))?;

        // if the platform does not expose die topology, treat all cpus in same
        // package as on the same die
        let die_cpus = read_list(format!(
            "/sys/devices/system/cpu/cpu{id}/topology/die_cpus_list"
        ))
        .unwrap_or(package_cpus.clone());

        let core_siblings = read_list(format!(
            "/sys/devices/system/cpu/cpu{id}/topology/core_siblings_list"
        ))?;
        let thread_siblings = read_list(format!(
            "/sys/devices/system/cpu/cpu{id}/topology/thread_siblings_list"
        ))?;

        let mut caches = Vec::new();

        for index in 0..4 {
            if let Ok(cache) = Cache::new(id, index) {
                caches.push(cache);
            }
        }

        tmp.insert(
            id,
            Cpu {
                id,
                core_id,
                die_id,
                package_id,
                core_cpus,
                die_cpus,
                package_cpus,
                core_siblings,
                thread_siblings,
                microcode: None,
                vendor: None,
                model_name: None,
                features: None,
                caches,
            },
        );
    }

    // there's a lot of information that's easier to get from /proc/cpuinfo

    let path = "/proc/cpuinfo";
    let file = File::open(path).map_err(|e| Error::unreadable(e, path))?;
    let mut reader = BufReader::new(file);

    let mut id: Option<usize> = None;
    let mut line = String::new();

    while reader
        .read_line(&mut line)
        .map_err(|e| Error::unreadable(e, path))?
        != 0
    {
        let line = ClearGuard::new(&mut line);
        let parts: Vec<&str> = line.split(':').map(|v| v.trim()).collect();

        if parts.len() != 2 {
            continue;
        }

        match parts[0] {
            "processor" => {
                if let Ok(v) = parts[1].parse() {
                    id = Some(v);
                }
            }
            "vendor_id" => {
                if let Some(id) = id {
                    if let Some(cpu) = tmp.get_mut(&id) {
                        cpu.vendor = Some(parts[1].to_owned());
                    }
                }
            }
            "model name" => {
                if let Some(id) = id {
                    if let Some(cpu) = tmp.get_mut(&id) {
                        cpu.model_name = Some(parts[1].to_owned());
                    }
                }
            }
            "microcode" => {
                if let Some(id) = id {
                    if let Some(cpu) = tmp.get_mut(&id) {
                        cpu.microcode = Some(parts[1].to_owned());
                    }
                }
            }
            "flags" | "Features" => {
                if let Some(id) = id {
                    if let Some(cpu) = tmp.get_mut(&id) {
                        cpu.features = Some(
                            parts[1]
                                .split_ascii_whitespace()
                                .map(|s| s.to_owned())
                                .collect(),
                        );
                    }
                }
            }
            _ => (),
        }
    }

    Ok(tmp.into_values().collect())
}
