use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

/// A compact, cross-platform summary of the system hardware and configuration.
/// Designed to be embedded as metadata in parquet recordings.
#[non_exhaustive]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemSummary {
    /// Operating system name (e.g., "linux", "macos")
    pub os: String,
    /// Kernel version string
    pub kernel: String,
    /// CPU architecture (e.g., "x86_64", "aarch64")
    pub arch: String,
    /// Hostname
    pub hostname: Option<String>,

    // CPU
    /// CPU model name (e.g., "Apple M2 Max", "Intel Xeon E5-2690 v4")
    pub cpu_model: Option<String>,
    /// CPU vendor (e.g., "GenuineIntel", "AuthenticAMD", "Apple")
    pub cpu_vendor: Option<String>,
    /// Total logical CPUs
    pub cpus: usize,
    /// Physical cores
    pub cores: Option<usize>,
    /// CPU sockets / packages
    pub packages: Option<usize>,
    /// Whether SMT / Hyperthreading is enabled
    pub smt: Option<bool>,

    // Memory
    /// Total system memory in bytes
    pub memory_total_bytes: Option<u64>,

    // NUMA
    /// Number of NUMA nodes (Linux only)
    pub numa_nodes: Option<usize>,

    // CPU topology — per-CPU placement in the physical hierarchy
    pub cpu_topology: Vec<CpuTopologyEntry>,

    // Cache topology (deduplicated)
    pub caches: Vec<CacheSummary>,

    // Network interfaces
    pub nics: Vec<NicSummary>,

    // GPUs
    pub gpus: Vec<GpuSummary>,
}

/// Placement of a single logical CPU in the physical hierarchy.
#[non_exhaustive]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuTopologyEntry {
    /// Logical CPU ID (matches metric labels like `cpu_usage{id="42"}`)
    pub cpu: usize,
    /// Physical core ID
    pub core: usize,
    /// Die ID within the package
    pub die: usize,
    /// Package / socket ID
    pub package: usize,
    /// NUMA node this CPU belongs to
    pub numa_node: Option<usize>,
}

/// Summary of a single cache level, deduplicated across CPUs.
#[non_exhaustive]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheSummary {
    /// Cache level name (e.g., "L1d", "L1i", "L2", "L3")
    pub level: String,
    /// Cache size as human-readable string (e.g., "32K", "4M")
    pub size: Option<String>,
    /// Number of instances of this cache
    pub instances: usize,
    /// Which logical CPUs share each instance of this cache.
    /// Each inner Vec is one cache instance's set of CPU IDs.
    pub shared_cpus: Vec<Vec<usize>>,
}

/// Summary of a network interface.
#[non_exhaustive]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NicSummary {
    /// Interface name (e.g., "eth0", "en0")
    pub name: String,
    /// Link speed in Mbps
    pub speed: Option<usize>,
    /// NUMA node this NIC is attached to
    pub numa_node: Option<usize>,
    /// NIC driver name
    pub driver: Option<String>,
}

/// Summary of a GPU device.
#[non_exhaustive]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuSummary {
    /// Device name (e.g., "NVIDIA A100", "Apple M2 Max")
    pub name: Option<String>,
    /// Vendor identifier (e.g., "nvidia", "apple")
    pub vendor: String,
    /// Total video memory in bytes
    pub memory_bytes: Option<u64>,
    /// Driver version string
    pub driver: Option<String>,
    /// NUMA node this GPU is attached to
    pub numa_node: Option<usize>,
}

/// Collect a cross-platform system summary.
///
/// Returns `None` on unsupported platforms.
pub fn summary() -> Option<SystemSummary> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::collect())
    }
    #[cfg(target_os = "macos")]
    {
        Some(macos::collect())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_produces_valid_output() {
        let s = summary().expect("summary should succeed on this platform");

        assert!(!s.os.is_empty());
        assert!(!s.arch.is_empty());
        assert!(s.cpus > 0);

        // Should be serializable to JSON
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.is_empty());

        // Should round-trip through JSON
        let deserialized: SystemSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.os, s.os);
        assert_eq!(deserialized.cpus, s.cpus);
    }

    #[test]
    fn deserialize_missing_fields() {
        // Simulates reading a parquet file written by an older version that
        // had fewer fields. All missing fields should get defaults.
        let json = r#"{"os": "linux", "kernel": "5.15.0"}"#;
        let s: SystemSummary = serde_json::from_str(json).unwrap();
        assert_eq!(s.os, "linux");
        assert_eq!(s.cpus, 0);
        assert!(s.gpus.is_empty());
        assert!(s.caches.is_empty());
    }

    #[test]
    fn deserialize_unknown_fields() {
        // Simulates reading a parquet file written by a newer version that
        // has fields we don't know about yet. Should not fail.
        let json = r#"{"os": "linux", "kernel": "6.8.0", "arch": "x86_64", "cpus": 128, "some_future_field": true}"#;
        let s: SystemSummary = serde_json::from_str(json).unwrap();
        assert_eq!(s.os, "linux");
        assert_eq!(s.cpus, 128);
    }
}
