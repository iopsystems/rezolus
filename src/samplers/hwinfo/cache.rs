use super::*;

#[derive(Clone, Serialize)]
pub struct Cache {
    coherency_line_size: usize,
    number_of_sets: usize,
    shared_cpus: Vec<usize>,
    size: String,
    r#type: CacheType,
    ways_of_associativity: usize,
}

#[derive(Clone, Serialize)]
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
    let raw = std::fs::read_to_string(path)?;
    let raw = raw.trim();

    match raw {
        "Data" | "data" => Ok(CacheType::Data),
        "Instruction" | "instruction" => Ok(CacheType::Instruction),
        "Unified" | "unified" => Ok(CacheType::Unified),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unexpected cache type",
        )),
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
