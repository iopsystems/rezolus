use super::*;

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

    microcode: Option<String>,
    vendor: Option<String>,
    model_name: Option<String>,
    features: Option<String>,

    caches: Vec<Cache>,
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
    let mut tmp = HashMap::new();

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

    let file = File::open("/proc/cpuinfo")?;
    let reader = BufReader::new(file);

    let mut id: Option<usize> = None;

    for line in reader.lines() {
        if line.is_err() {
            break;
        }

        let line = line.unwrap();

        let parts: Vec<String> = line.split(':').map(|v| v.trim().to_owned()).collect();

        if parts.len() == 2 {
            match parts[0].as_str() {
                "processor" => {
                    if let Ok(v) = parts[1].parse() {
                        id = Some(v);
                    }
                }
                "vendor_id" => {
                    if let Some(id) = id {
                        if let Some(cpu) = tmp.get_mut(&id) {
                            cpu.vendor = Some(parts[1].clone());
                        }
                    }
                }
                "model name" => {
                    if let Some(id) = id {
                        if let Some(cpu) = tmp.get_mut(&id) {
                            cpu.model_name = Some(parts[1].clone());
                        }
                    }
                }
                "microcode" => {
                    if let Some(id) = id {
                        if let Some(cpu) = tmp.get_mut(&id) {
                            cpu.microcode = Some(parts[1].clone());
                        }
                    }
                }
                "flags" | "Features" => {
                    if let Some(id) = id {
                        if let Some(cpu) = tmp.get_mut(&id) {
                            cpu.features = Some(parts[1].clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut ret: Vec<Cpu> = tmp.drain().map(|(_, v)| v).collect();

    ret.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(ret)
}
