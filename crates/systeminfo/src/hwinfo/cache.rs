use std::path::Path;

use super::util::*;
use crate::error::ErrorSource;
use crate::{Error, Result};

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cache {
    pub coherency_line_size: usize,
    pub number_of_sets: usize,
    pub shared_cpus: Vec<usize>,
    pub size: String,
    pub r#type: CacheType,
    pub ways_of_associativity: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheType {
    Data,
    Instruction,
    Unified,
}

impl Cache {
    pub fn new(cpu: usize, index: usize) -> Result<Self> {
        let coherency_line_size = read_usize(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/coherency_line_size"
        ))?;
        let number_of_sets = read_usize(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/number_of_sets"
        ))?;
        let shared_cpus = read_list(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/shared_cpu_list"
        ))?;
        let size = read_string(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/size"
        ))?;
        let r#type = read_cache_type(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/type"
        ))?;
        let ways_of_associativity = read_usize(format!(
            "/sys/devices/system/cpu/cpu{cpu}/cache/index{index}/ways_of_associativity"
        ))?;

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

fn read_cache_type(path: impl AsRef<Path>) -> Result<CacheType> {
    let path = path.as_ref();

    let raw = std::fs::read_to_string(path).map_err(|e| Error::unreadable(e, path))?;
    let raw = raw.trim();

    match raw {
        _ if raw.eq_ignore_ascii_case("data") => Ok(CacheType::Data),
        _ if raw.eq_ignore_ascii_case("instruction") => Ok(CacheType::Instruction),
        _ if raw.eq_ignore_ascii_case("unified") => Ok(CacheType::Unified),
        _ => Err(Error::unparseable(ErrorSource::None, path)),
    }
}

pub fn get_caches() -> Result<Vec<Vec<Cache>>> {
    // This is sufficient for up to four caches: L1i, L1d, L2, L3
    let max_cache_index = 4; // inclusive

    let mut ret = vec![vec![]; max_cache_index];

    let cpu_ids = read_list("/sys/devices/system/cpu/online")?;

    for (index, caches) in ret.iter_mut().enumerate() {
        for cpu_id in &cpu_ids {
            if let Ok(cache) = Cache::new(*cpu_id, index) {
                if cache.shared_cpus[0] != *cpu_id {
                    continue;
                }

                caches.push(cache);
            }
        }
    }

    Ok(ret)
}
