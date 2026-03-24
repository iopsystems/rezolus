use super::{CacheSummary, GpuSummary, SystemSummary};
use std::process::Command;

pub fn collect() -> SystemSummary {
    let kernel = sysctl_string("kern.version").unwrap_or_default();
    let hostname = sysctl_string("kern.hostname");

    let cpu_model = sysctl_string("machdep.cpu.brand_string");
    let cpu_vendor = sysctl_string("machdep.cpu.vendor");
    let cpus = sysctl_u64("hw.logicalcpu").unwrap_or(1) as usize;
    let cores = sysctl_u64("hw.physicalcpu").map(|v| v as usize);
    let smt = cores.map(|c| cpus > c);

    let memory_total_bytes = sysctl_u64("hw.memsize");

    let caches = collect_caches();
    let gpus = collect_gpus();

    SystemSummary {
        os: "macos".to_string(),
        kernel,
        arch: std::env::consts::ARCH.to_string(),
        hostname,
        cpu_model,
        cpu_vendor,
        cpus,
        cores,
        packages: Some(1), // macOS always has a single SoC/package
        smt,
        memory_total_bytes,
        numa_nodes: None, // macOS does not expose NUMA topology
        caches,
        gpus,
    }
}

fn collect_caches() -> Vec<CacheSummary> {
    let mut caches = Vec::new();

    // L1 data cache
    if let Some(size) = sysctl_u64("hw.l1dcachesize") {
        let instances = sysctl_u64("hw.physicalcpu").unwrap_or(1) as usize;
        caches.push(CacheSummary {
            level: "L1d".to_string(),
            size: Some(format_cache_size(size)),
            instances,
        });
    }

    // L1 instruction cache
    if let Some(size) = sysctl_u64("hw.l1icachesize") {
        let instances = sysctl_u64("hw.physicalcpu").unwrap_or(1) as usize;
        caches.push(CacheSummary {
            level: "L1i".to_string(),
            size: Some(format_cache_size(size)),
            instances,
        });
    }

    // L2 cache
    if let Some(size) = sysctl_u64("hw.l2cachesize") {
        caches.push(CacheSummary {
            level: "L2".to_string(),
            size: Some(format_cache_size(size)),
            instances: sysctl_u64("hw.physicalcpu").unwrap_or(1) as usize,
        });
    }

    // L3 cache (if present — not all Apple Silicon has L3)
    if let Some(size) = sysctl_u64("hw.l3cachesize") {
        if size > 0 {
            caches.push(CacheSummary {
                level: "L3".to_string(),
                size: Some(format_cache_size(size)),
                instances: 1,
            });
        }
    }

    caches
}

fn collect_gpus() -> Vec<GpuSummary> {
    // Use system_profiler to get GPU information
    let output = match Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };

    let json: serde_json::Value = match serde_json::from_slice(&output) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut gpus = Vec::new();

    if let Some(displays) = json.get("SPDisplaysDataType").and_then(|v| v.as_array()) {
        for gpu in displays {
            let name = gpu
                .get("sppci_model")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let memory_bytes = gpu
                .get("sppci_vram")
                .and_then(|v| v.as_str())
                .and_then(parse_vram_string);

            let vendor = gpu
                .get("sppci_vendor")
                .and_then(|v| v.as_str())
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| "apple".to_string());

            // Normalize vendor string
            let vendor = if vendor.contains("apple") {
                "apple".to_string()
            } else if vendor.contains("nvidia") {
                "nvidia".to_string()
            } else if vendor.contains("amd") || vendor.contains("ati") {
                "amd".to_string()
            } else if vendor.contains("intel") {
                "intel".to_string()
            } else {
                vendor
            };

            gpus.push(GpuSummary {
                name,
                vendor,
                memory_bytes,
                driver: None,
            });
        }
    }

    gpus
}

/// Parse VRAM strings like "1536 MB" or "16 GB"
fn parse_vram_string(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }

    let value: u64 = parts[0].parse().ok()?;
    match parts[1].to_uppercase().as_str() {
        "MB" => Some(value * 1024 * 1024),
        "GB" => Some(value * 1024 * 1024 * 1024),
        "TB" => Some(value * 1024 * 1024 * 1024 * 1024),
        _ => None,
    }
}

fn format_cache_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{}M", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{}K", bytes / 1024)
    } else {
        format!("{bytes}")
    }
}

fn sysctl_string(name: &str) -> Option<String> {
    let output = Command::new("sysctl")
        .arg("-n")
        .arg(name)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn sysctl_u64(name: &str) -> Option<u64> {
    sysctl_string(name)?.parse().ok()
}
