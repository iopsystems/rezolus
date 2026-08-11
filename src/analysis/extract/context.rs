//! Recording-level context: source, version, duration, interval, systeminfo,
//! and the coverage map (subsystems present vs absent).
//!
//! Subsystem attribution (see `extract::subsystem_of`) has two sources: a
//! `sampler` label stamped by agents >= 5.17.1 (preferred), or — for older
//! recordings that carry no such label — the longest sampler name in
//! [`EXPECTED_SUBSYSTEMS`] that is a `_`-boundary prefix of the metric name.
//! When neither source disambiguates, the metric is `unattributed` and its
//! *domain* ([`domain_of`]) becomes "uncertain": `build_coverage` excludes
//! any sampler in an uncertain domain from `subsystems_absent`, since its
//! true presence/absence can't be known from an unattributed name. It is
//! also not added to `subsystems_present` — we refuse to claim absence we
//! can't know, and we don't fabricate presence either.

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

/// Metric-name domains that diverge from their owning sampler's leading
/// token — the sampler's metrics don't share its name prefix, so the plain
/// first-token rule below would compute the wrong domain for them. Applied
/// uniformly to sampler names too for simplicity; harmless in practice
/// since no *sampler* name in [`EXPECTED_SUBSYSTEMS`] starts with `drive_`
/// or `gpmu_` (`domain_of("drivehealth")` still yields `"drivehealth"` —
/// its first token doesn't match either alias key).
const DOMAIN_ALIASES: &[(&str, &str)] = &[
    ("drive", "drivehealth"), // drivehealth sampler emits drive_* metrics
    ("gpmu", "gpu"),          // gpu_amd_pmu sampler emits gpmu_* metrics
];

/// Domain of a sampler or metric name: its first `_`-separated token, except
/// names prefixed `cgroup_` strip that prefix first and take the *next*
/// token (`cgroup_cpu_usage` -> `cpu`, `cgroup_scheduler_offcpu` ->
/// `scheduler`, `cgroup_syscall` -> `syscall`), and the result is mapped
/// through [`DOMAIN_ALIASES`] (`drive_temperature` -> `drive` -> aliased to
/// `drivehealth`; `gpmu_busy_cycles` -> `gpmu` -> aliased to `gpu`).
/// Applied to sampler names (`blockio_latency` -> `blockio`, `drivehealth`
/// -> `drivehealth`, `rezolus_rusage` -> `rezolus`) and to metric names
/// alike, so `build_coverage`'s pruning and extraction's uncertain-domain
/// collection agree on the same notion of "domain".
pub(crate) fn domain_of(name: &str) -> &str {
    let stripped = name.strip_prefix("cgroup_").unwrap_or(name);
    let token = stripped.split('_').next().unwrap_or(stripped);
    DOMAIN_ALIASES
        .iter()
        .find(|(from, _)| *from == token)
        .map_or(token, |(_, to)| *to)
}

/// present = the distinct `sampler` label values observed in the recording;
/// callers insert the synthetic `unattributed` for metrics lacking the
/// label (this function does not add it). absent = the static universe
/// minus present, minus any sampler whose domain is in `uncertain_domains`
/// (a domain with at least one still-`unattributed` metric after
/// name-prefix inference — its true presence/absence can't be known, so we
/// exclude it from `subsystems_absent` rather than assert a false absence;
/// it is *not* added to present either, since we don't fabricate presence
/// we can't confirm). Both output lists sorted (BTreeSet iteration order).
pub(crate) fn build_coverage(
    present: &BTreeSet<String>,
    uncertain_domains: &BTreeSet<String>,
) -> Coverage {
    Coverage {
        subsystems_present: present.iter().cloned().collect(),
        subsystems_absent: EXPECTED_SUBSYSTEMS
            .iter()
            .filter(|&&s| !present.contains(s) && !uncertain_domains.contains(domain_of(s)))
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
    uncertain_domains: &BTreeSet<String>,
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
        coverage: build_coverage(present, uncertain_domains),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_of_takes_first_token() {
        // sampler names
        assert_eq!(domain_of("blockio_latency"), "blockio");
        assert_eq!(domain_of("drivehealth"), "drivehealth");
        assert_eq!(domain_of("rezolus_rusage"), "rezolus");
        assert_eq!(domain_of("cpu_usage"), "cpu");
        // metric names, including the cgroup-prefix strip
        assert_eq!(domain_of("cgroup_cpu_usage"), "cpu");
        assert_eq!(domain_of("cgroup_scheduler_offcpu"), "scheduler");
        assert_eq!(domain_of("cgroup_syscall"), "syscall");
    }

    #[test]
    fn domain_of_aliases_metric_domains_that_diverge_from_their_sampler() {
        // drivehealth sampler emits drive_* metrics: first-token domain
        // "drive" must alias to the sampler's domain "drivehealth".
        assert_eq!(domain_of("drive_temperature"), "drivehealth");
        // gpu_amd_pmu sampler emits gpmu_* metrics: first-token domain
        // "gpmu" must alias to the sampler's domain "gpu".
        assert_eq!(domain_of("gpmu_busy_cycles"), "gpu");
        // the alias must not clash with the sampler name's own domain.
        assert_eq!(domain_of("drivehealth"), "drivehealth");
    }

    #[test]
    fn coverage_diffs_present_against_universe() {
        let mut present = BTreeSet::new();
        present.insert("cpu_usage".to_string());
        present.insert("scheduler_runqueue".to_string());
        present.insert("unattributed".to_string());
        // empty uncertain_domains preserves old (pre-inference) behavior.
        let c = build_coverage(&present, &BTreeSet::new());
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
    fn coverage_prunes_uncertain_domains_from_absent_without_adding_to_present() {
        let present = BTreeSet::new();
        let mut uncertain = BTreeSet::new();
        uncertain.insert("scheduler".to_string());
        uncertain.insert("blockio".to_string());
        let c = build_coverage(&present, &uncertain);
        // pruned domains' samplers are excluded from absent...
        assert!(!c
            .subsystems_absent
            .contains(&"scheduler_runqueue".to_string()));
        assert!(!c.subsystems_absent.contains(&"blockio_latency".to_string()));
        assert!(!c
            .subsystems_absent
            .contains(&"blockio_requests".to_string()));
        // ...but not fabricated into present either.
        assert!(c.subsystems_present.is_empty());
        // unrelated domains are unaffected.
        assert!(c.subsystems_absent.contains(&"cpu_usage".to_string()));
    }

    #[test]
    fn coverage_prunes_drivehealth_and_gpu_via_aliased_metric_domains() {
        let present = BTreeSet::new();
        // As extraction would compute it: domain_of("drive_temperature") ==
        // "drivehealth", domain_of("gpmu_busy_cycles") == "gpu".
        let mut uncertain = BTreeSet::new();
        uncertain.insert(domain_of("drive_temperature").to_string());
        uncertain.insert(domain_of("gpmu_busy_cycles").to_string());
        let c = build_coverage(&present, &uncertain);
        assert!(!c.subsystems_absent.contains(&"drivehealth".to_string()));
        // every gpu_* sampler is pruned, not just the pmu one.
        assert!(!c.subsystems_absent.contains(&"gpu_amd_pmu".to_string()));
        assert!(!c.subsystems_absent.contains(&"gpu_amd_smi".to_string()));
        assert!(!c.subsystems_absent.contains(&"gpu_apple".to_string()));
        assert!(!c.subsystems_absent.contains(&"gpu_nvidia".to_string()));
        // unrelated domains still assert absence.
        assert!(c.subsystems_absent.contains(&"cpu_usage".to_string()));
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
            &BTreeSet::new(),
        );
        assert_eq!(bad.agent_version.as_deref(), Some("1.2.3"));
        assert_eq!(bad.systeminfo, None);
    }
}
