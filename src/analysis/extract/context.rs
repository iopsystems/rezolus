//! Recording-level context: source, version, duration, interval, systeminfo,
//! and the coverage map (subsystems present vs absent).

use std::collections::BTreeSet;

use crate::analysis::record::{Context, Coverage};

/// The expected sampler universe. Mirrors the `const NAME` registrations
/// under `src/agent/samplers/`. A static list (rather than reading the
/// linker-populated SAMPLERS slice at runtime) because the slice is
/// platform-cfg-gated — a macOS binary analyzing a Linux recording would
/// see only macOS samplers — and because the recording may come from a
/// different rezolus version anyway. A guard test in
/// `src/agent/samplers/mod.rs` catches drift when samplers are added.
/// (Deliberately excludes the synthetic `unattributed`.)
pub(crate) const EXPECTED_SUBSYSTEMS: &[&str] = &[
    "blockio_latency",
    "blockio_requests",
    "cpu_bandwidth",
    "cpu_branch",
    "cpu_cores",
    "cpu_dtlb",
    "cpu_frequency",
    "cpu_l3",
    "cpu_migrations",
    "cpu_perf",
    "cpu_tlb_flush",
    "cpu_usage",
    "drivehealth",
    "gpu_amd_pmu",
    "gpu_amd_smi",
    "gpu_apple",
    "gpu_nvidia",
    "memory_meminfo",
    "memory_vmstat",
    "network_ethtool",
    "network_interfaces",
    "network_traffic",
    "rezolus_rusage",
    "scheduler_runqueue",
    "syscall_counts",
    "syscall_latency",
    "tcp_connect_latency",
    "tcp_packet_latency",
    "tcp_receive",
    "tcp_retransmit",
    "tcp_traffic",
];

/// present = the distinct `sampler` label values observed in the recording;
/// callers insert the synthetic `unattributed` for metrics lacking the
/// label (this function does not add it). absent = the static universe
/// minus present. Both sorted (BTreeSet iteration order).
pub(crate) fn build_coverage(present: &BTreeSet<String>) -> Coverage {
    Coverage {
        subsystems_present: present.iter().cloned().collect(),
        subsystems_absent: EXPECTED_SUBSYSTEMS
            .iter()
            .filter(|&&s| !present.contains(s))
            .map(|s| s.to_string())
            .collect(),
    }
}

/// Assemble the record context. Empty version -> None (`.rez` recordings
/// carry no version metadata). `systeminfo` is a JSON passthrough; invalid
/// JSON -> None rather than an error (the recording is still analyzable).
pub(crate) fn build_context(
    source: String,
    version: String,
    duration_s: f64,
    sampling_interval_s: f64,
    systeminfo_raw: Option<String>,
    present: &BTreeSet<String>,
) -> Context {
    Context {
        source,
        agent_version: if version.is_empty() {
            None
        } else {
            Some(version)
        },
        duration_s,
        sampling_interval_s,
        systeminfo: systeminfo_raw.and_then(|raw| serde_json::from_str(&raw).ok()),
        coverage: build_coverage(present),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_diffs_present_against_universe() {
        let mut present = BTreeSet::new();
        present.insert("cpu_usage".to_string());
        present.insert("scheduler_runqueue".to_string());
        present.insert("unattributed".to_string());
        let c = build_coverage(&present);
        assert_eq!(
            c.subsystems_present,
            vec![
                "cpu_usage".to_string(),
                "scheduler_runqueue".to_string(),
                "unattributed".to_string()
            ]
        );
        // absent list is the known universe minus present, sorted; spot-check
        assert!(c.subsystems_absent.contains(&"blockio_latency".to_string()));
        assert!(!c.subsystems_absent.contains(&"cpu_usage".to_string()));
        assert!(!c.subsystems_absent.contains(&"unattributed".to_string()));
        let mut sorted = c.subsystems_absent.clone();
        sorted.sort();
        assert_eq!(c.subsystems_absent, sorted);
    }

    #[test]
    fn context_fields_normalized() {
        let ctx = build_context(
            "rezolus".to_string(),
            String::new(), // .rez recordings have no version metadata
            120.0,
            1.0,
            Some(r#"{"os":"linux"}"#.to_string()),
            &BTreeSet::new(),
        );
        assert_eq!(ctx.agent_version, None);
        assert_eq!(ctx.duration_s, 120.0);
        assert!(ctx.systeminfo.is_some());
        let bad = build_context(
            "p".into(),
            "1.2.3".into(),
            10.0,
            1.0,
            Some("not json".into()),
            &BTreeSet::new(),
        );
        assert_eq!(bad.agent_version.as_deref(), Some("1.2.3"));
        assert_eq!(bad.systeminfo, None);
    }
}
