use super::{CacheSummary, GpuSummary, SystemSummary};
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub fn collect() -> SystemSummary {
    let kernel = read_string("/proc/version").unwrap_or_default();
    let hostname = read_string("/proc/sys/kernel/hostname").ok();

    let (cpu_model, cpu_vendor, cpus, cores, packages, smt) = collect_cpu_info();
    let memory_total_bytes = collect_memory();
    let numa_nodes = collect_numa_nodes();
    let caches = collect_caches();
    let gpus = collect_gpus();

    SystemSummary {
        os: "linux".to_string(),
        kernel,
        arch: std::env::consts::ARCH.to_string(),
        hostname,
        cpu_model,
        cpu_vendor,
        cpus,
        cores,
        packages,
        smt,
        memory_total_bytes,
        numa_nodes,
        caches,
        gpus,
    }
}

fn collect_cpu_info() -> (
    Option<String>,
    Option<String>,
    usize,
    Option<usize>,
    Option<usize>,
    Option<bool>,
) {
    let cpu_ids = read_list("/sys/devices/system/cpu/online").unwrap_or_default();
    let cpus = cpu_ids.len();

    // Deduplicate cores and packages from topology
    let mut core_set = HashSet::new();
    let mut package_set = HashSet::new();

    for id in &cpu_ids {
        if let Ok(core_id) = read_usize(format!("/sys/devices/system/cpu/cpu{id}/topology/core_id"))
        {
            let pkg_id = read_usize(format!(
                "/sys/devices/system/cpu/cpu{id}/topology/physical_package_id"
            ))
            .unwrap_or(0);
            core_set.insert((pkg_id, core_id));
            package_set.insert(pkg_id);
        }
    }

    let cores = if core_set.is_empty() {
        None
    } else {
        Some(core_set.len())
    };
    let packages = if package_set.is_empty() {
        None
    } else {
        Some(package_set.len())
    };

    // SMT
    let smt = read_usize("/sys/devices/system/cpu/smt/active")
        .ok()
        .map(|v| v == 1);

    // Model and vendor from /proc/cpuinfo (just first CPU)
    let (model, vendor) = read_cpuinfo();

    (model, vendor, cpus, cores, packages, smt)
}

fn read_cpuinfo() -> (Option<String>, Option<String>) {
    let file = match fs::File::open("/proc/cpuinfo") {
        Ok(f) => f,
        Err(_) => return (None, None),
    };

    let reader = BufReader::new(file);
    let mut model = None;
    let mut vendor = None;

    for line in reader.lines().map_while(Result::ok) {
        let parts: Vec<&str> = line.split(':').map(|v| v.trim()).collect();
        if parts.len() != 2 {
            continue;
        }
        match parts[0] {
            "model name" => {
                if model.is_none() {
                    model = Some(parts[1].to_string());
                }
            }
            "vendor_id" => {
                if vendor.is_none() {
                    vendor = Some(parts[1].to_string());
                }
            }
            _ => {}
        }
        if model.is_some() && vendor.is_some() {
            break;
        }
    }

    (model, vendor)
}

fn collect_memory() -> Option<u64> {
    let file = fs::File::open("/proc/meminfo").ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines().map_while(Result::ok) {
        if line.starts_with("MemTotal:") {
            let mut parts = line.split_ascii_whitespace().skip(1);
            if let Some(value) = parts.next() {
                if let Ok(kb) = value.parse::<u64>() {
                    return Some(kb * 1024);
                }
            }
        }
    }

    None
}

fn collect_numa_nodes() -> Option<usize> {
    read_list("/sys/devices/system/node/online")
        .ok()
        .map(|nodes| nodes.len())
}

fn collect_caches() -> Vec<CacheSummary> {
    let cpu_ids = match read_list("/sys/devices/system/cpu/online") {
        Ok(ids) => ids,
        Err(_) => return Vec::new(),
    };

    let mut summaries = Vec::new();

    // Check cache indices 0-3 (L1i, L1d, L2, L3)
    for index in 0..4 {
        let mut count = 0usize;
        let mut size: Option<String> = None;
        let mut cache_type: Option<String> = None;
        let mut seen_leaders = HashSet::new();

        for cpu_id in &cpu_ids {
            let base = format!("/sys/devices/system/cpu/cpu{cpu_id}/cache/index{index}");

            if !Path::new(&base).exists() {
                continue;
            }

            // Deduplicate by shared_cpu_list leader
            let shared = read_list(format!("{base}/shared_cpu_list")).unwrap_or_default();
            let leader = shared.first().copied().unwrap_or(*cpu_id);
            if !seen_leaders.insert(leader) {
                continue;
            }

            count += 1;

            if size.is_none() {
                size = read_string(format!("{base}/size")).ok();
            }
            if cache_type.is_none() {
                cache_type = read_string(format!("{base}/type")).ok();
            }
        }

        if count == 0 {
            continue;
        }

        // Map index + type to a level name
        let level = match cache_type.as_deref() {
            Some("Data") => format!("L{}d", index_to_level(index)),
            Some("Instruction") => format!("L{}i", index_to_level(index)),
            _ => format!("L{}", index_to_level(index)),
        };

        summaries.push(CacheSummary {
            level,
            size,
            instances: count,
        });
    }

    summaries
}

/// Map sysfs cache index to cache level.
/// Typically: index 0 = L1d, index 1 = L1i, index 2 = L2, index 3 = L3
/// but the actual level is in the `level` file.
fn index_to_level(index: usize) -> usize {
    // Try to read the actual level from sysfs for cpu0
    read_usize(format!(
        "/sys/devices/system/cpu/cpu0/cache/index{index}/level"
    ))
    .unwrap_or(index + 1)
}

fn collect_gpus() -> Vec<GpuSummary> {
    collect_nvidia_gpus()
}

fn collect_nvidia_gpus() -> Vec<GpuSummary> {
    // Try to detect NVIDIA GPUs by reading /proc/driver/nvidia/gpus/
    let gpu_dir = Path::new("/proc/driver/nvidia/gpus");
    if !gpu_dir.exists() {
        return Vec::new();
    }

    let mut gpus = Vec::new();

    let driver = read_string("/proc/driver/nvidia/version")
        .ok()
        .and_then(|v| {
            // First line typically: "NVRM version: NVIDIA UNIX ... <version> ..."
            v.lines().next().and_then(|line| {
                line.split_whitespace()
                    .position(|w| w.starts_with("5") || w.starts_with("4") || w.starts_with("3"))
                    .and_then(|pos| line.split_whitespace().nth(pos))
                    .map(|s| s.to_string())
            })
        });

    if let Ok(entries) = fs::read_dir(gpu_dir) {
        for entry in entries.flatten() {
            let info_path = entry.path().join("information");
            if let Ok(contents) = fs::read_to_string(&info_path) {
                let mut name = None;

                for line in contents.lines() {
                    if let Some(val) = line.strip_prefix("Model:") {
                        name = Some(val.trim().to_string());
                    }
                }

                gpus.push(GpuSummary {
                    name,
                    vendor: "nvidia".to_string(),
                    memory_bytes: None, // Not available from procfs
                    driver: driver.clone(),
                });
            }
        }
    }

    gpus
}

// Simple sysfs reading helpers (standalone, not using hwinfo::util to keep
// the summary module self-contained)

fn read_string(path: impl AsRef<Path>) -> Result<String, std::io::Error> {
    Ok(fs::read_to_string(path)?.trim().to_string())
}

fn read_usize(path: impl AsRef<Path>) -> Result<usize, Box<dyn std::error::Error>> {
    Ok(read_string(path)?.parse()?)
}

fn read_list(path: impl AsRef<Path>) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
    let raw = read_string(path)?;
    let mut ret = Vec::new();

    for range in raw.split(',') {
        let mut parts = range.trim().split('-');
        let first: usize = parts.next().ok_or("empty range")?.parse()?;
        match parts.next() {
            Some(end) => {
                let last: usize = end.parse()?;
                ret.extend(first..=last);
            }
            None => ret.push(first),
        }
    }

    Ok(ret)
}
