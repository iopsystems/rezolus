//! Recording-level context: source, version, duration, interval, systeminfo,
//! and the coverage map (subsystems present vs absent).
//!
//! Subsystem attribution (see `extract::subsystem_of`) has one prerequisite
//! and three tiers. The prerequisite: a `sampler` label is always trusted
//! (agents >= 5.17.1 stamp it), but *inference from the metric name* is
//! trusted only when the recording's `source` metadata is exactly
//! `"rezolus"` (`extract()` computes this once as `infer` and threads it
//! through). A name like `cpu_usage` or `memory_free` proves nothing about
//! a Prometheus-scraped or otherwise foreign recording — the name is just a
//! string some other exporter happened to also use — so a missing, empty,
//! or non-`"rezolus"` source disables inference entirely and every
//! unlabeled metric is `unattributed`. rezolus-agent recordings (parquet
//! and `.rez`, every agent version) always carry `source = "rezolus"`.
//!
//! When inference is trusted, the three tiers, tried in order, are: the
//! `sampler` label (still checked first, redundantly with the
//! prerequisite, since it's authoritative regardless); an exact lookup in
//! [`METRIC_SAMPLERS`] — a static table of metric names whose sampler can't
//! be recovered from the name alone; and name-prefix inference — the
//! longest sampler name in [`EXPECTED_SUBSYSTEMS`] that is a `_`-boundary
//! prefix of the metric name. Before either of the latter two, a name in
//! [`AMBIGUOUS_METRICS`] (declared by more than one sampler, so a flat
//! mapping can't be correct) is forced to `unattributed` rather than
//! guessing one of its candidates.
//!
//! When nothing disambiguates, the metric is `unattributed`, and
//! `extract()` credits its uncertainty at the tightest granularity it can:
//! a name in [`AMBIGUOUS_METRICS`] contributes its exact *candidate sampler
//! set* to `uncertain_samplers`; anything else (inference untrusted, or a
//! genuinely unknown name) contributes its *domain* ([`domain_of`]) to
//! `uncertain_domains`. `build_coverage` excludes both — every sampler in
//! an uncertain domain, and every sampler in `uncertain_samplers` — from
//! `subsystems_absent`, since their true presence/absence can't be known.
//! Nothing in either set is added to `subsystems_present` either — we
//! refuse to claim absence we can't know, and we don't fabricate presence
//! we can't confirm. This is all defense-in-depth for foreign sources,
//! ambiguous vocabulary, and future metrics, not the primary resolution
//! mechanism now that [`METRIC_SAMPLERS`] covers the known non-prefixing
//! cases on trusted (rezolus-sourced) recordings.

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
    "cpu_power",
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

/// Explicit metric-name -> sampler mapping for metrics whose name cannot be
/// resolved against [`EXPECTED_SUBSYSTEMS`] by exact match or `_`-boundary
/// prefix (e.g. `cpu_cycles` has no `cpu_perf`-prefixed spelling, and
/// `cgroup_cpu_usage` doesn't share `cpu_usage`'s prefix at all — it's
/// declared inside the `cpu_usage` sampler's own module, there is no
/// separate cgroup sampler).
///
/// Provenance: harvested by reading every sampler's `stats.rs`
/// (`src/agent/samplers/*/*/*/stats.rs` and `src/agent/samplers/*/stats.rs`,
/// all platforms) and mapping each `metric(name = "...")` declaration to
/// the sampler whose `const NAME` governs that module tree, cross-checked
/// against each sampler's `mod.rs` (`attribute_sampler` in
/// `src/agent/samplers/mod.rs` attributes by module-path prefix, so e.g.
/// everything under `samplers/cpu/linux/perf/` belongs to `cpu_perf`,
/// `samplers/tcp/linux/traffic/` to `tcp_traffic`). Only entries that
/// *need* it are included — a name that already resolves correctly via
/// [`EXPECTED_SUBSYSTEMS`] prefix matching (e.g. `cpu_branch_instructions`
/// -> `cpu_branch`, `tcp_connect_latency` -> `tcp_connect_latency`) is left
/// out to keep the table minimal. Sorted alphabetically.
///
/// Deliberately excludes [`AMBIGUOUS_METRICS`] — names declared identically
/// by more than one sampler, where a flat mapping can't be correct for all
/// of them.
///
/// `metric_samplers_match_agent_attribution` in `src/agent/samplers/mod.rs`
/// is this table's drift guard: it walks the live `metriken` registry and
/// asserts this table (plus prefix inference) agrees with the agent's own
/// module-path attribution, for every registered metric whose name isn't in
/// [`AMBIGUOUS_METRICS`] and that the agent doesn't itself call
/// `unattributed`, printing the correct line to paste in here when it
/// doesn't. See that test's doc comment for what it can't check (metrics
/// the agent's own `attribute_sampler` calls `unattributed`, and samplers
/// not registered on whatever platform compiled the test).
pub(crate) const METRIC_SAMPLERS: &[(&str, &str)] = &[
    ("blockio_bytes", "blockio_requests"),
    ("blockio_errors", "blockio_requests"),
    ("blockio_operations", "blockio_requests"),
    ("blockio_requeues", "blockio_requests"),
    ("blockio_size", "blockio_requests"),
    ("cgroup_cpu_bandwidth_period_duration", "cpu_bandwidth"),
    ("cgroup_cpu_bandwidth_periods", "cpu_bandwidth"),
    ("cgroup_cpu_bandwidth_quota", "cpu_bandwidth"),
    ("cgroup_cpu_bandwidth_throttled_periods", "cpu_bandwidth"),
    ("cgroup_cpu_bandwidth_throttled_time", "cpu_bandwidth"),
    ("cgroup_cpu_cycles", "cpu_perf"),
    ("cgroup_cpu_instructions", "cpu_perf"),
    ("cgroup_cpu_migrations", "cpu_migrations"),
    ("cgroup_cpu_throttled", "cpu_bandwidth"),
    ("cgroup_cpu_throttled_time", "cpu_bandwidth"),
    ("cgroup_cpu_tlb_flush", "cpu_tlb_flush"),
    ("cgroup_cpu_usage", "cpu_usage"),
    ("cgroup_cpu_usage_exited_tasks", "cpu_usage"),
    ("cgroup_scheduler_context_switch", "scheduler_runqueue"),
    ("cgroup_scheduler_offcpu", "scheduler_runqueue"),
    ("cgroup_scheduler_runqueue_wait", "scheduler_runqueue"),
    ("cgroup_syscall", "syscall_counts"),
    ("core_c10_residency", "cpu_power"),
    ("core_c1_residency", "cpu_power"),
    ("core_c2_residency", "cpu_power"),
    ("core_c3_residency", "cpu_power"),
    ("core_c6_residency", "cpu_power"),
    ("core_c7_residency", "cpu_power"),
    ("core_c8_residency", "cpu_power"),
    ("core_c9_residency", "cpu_power"),
    ("core_cstate_residency", "cpu_power"),
    ("cpu_aperf", "cpu_frequency"),
    ("cpu_core_energy", "cpu_power"),
    ("cpu_cores_energy", "cpu_power"),
    ("cpu_cycles", "cpu_perf"),
    ("cpu_dram_energy", "cpu_power"),
    ("cpu_igpu_energy", "cpu_power"),
    ("cpu_instructions", "cpu_perf"),
    ("cpu_mperf", "cpu_frequency"),
    ("cpu_package_energy", "cpu_power"),
    ("cpu_platform_energy", "cpu_power"),
    ("cpu_tsc", "cpu_frequency"),
    ("drive_temperature", "drivehealth"),
    ("drive_temperature_critical_time", "drivehealth"),
    ("drive_temperature_warning_time", "drivehealth"),
    ("drive_thermal_throttle_time", "drivehealth"),
    ("drive_thermal_throttle_transitions", "drivehealth"),
    ("gpmu_active_clock", "gpu_amd_pmu"),
    ("gpmu_busy_cycles", "gpu_amd_pmu"),
    ("gpmu_clock", "gpu_amd_pmu"),
    ("gpmu_icache_hits", "gpu_amd_pmu"),
    ("gpmu_icache_requests", "gpu_amd_pmu"),
    ("gpmu_l2_hits", "gpu_amd_pmu"),
    ("gpmu_l2_misses", "gpu_amd_pmu"),
    ("gpmu_lds_instructions", "gpu_amd_pmu"),
    ("gpmu_salu_instructions", "gpu_amd_pmu"),
    ("gpmu_valu_instructions", "gpu_amd_pmu"),
    ("gpmu_vram_read_requests", "gpu_amd_pmu"),
    ("gpmu_vram_write_requests", "gpu_amd_pmu"),
    ("gpmu_wave_cycles", "gpu_amd_pmu"),
    ("gpmu_waves", "gpu_amd_pmu"),
    ("gpu_dram_bandwidth_utilization", "gpu_nvidia"),
    ("gpu_pcie_bandwidth", "gpu_nvidia"),
    ("gpu_sm_occupancy", "gpu_nvidia"),
    ("gpu_sm_utilization", "gpu_nvidia"),
    ("gpu_tensor_utilization", "gpu_nvidia"),
    ("memory_available", "memory_meminfo"),
    ("memory_buffers", "memory_meminfo"),
    ("memory_cached", "memory_meminfo"),
    ("memory_free", "memory_meminfo"),
    ("memory_numa_foreign", "memory_vmstat"),
    ("memory_numa_hit", "memory_vmstat"),
    ("memory_numa_interleave", "memory_vmstat"),
    ("memory_numa_local", "memory_vmstat"),
    ("memory_numa_miss", "memory_vmstat"),
    ("memory_numa_other", "memory_vmstat"),
    ("memory_total", "memory_meminfo"),
    ("network_bytes", "network_traffic"),
    ("network_drop", "network_interfaces"),
    (
        "network_ena_bandwidth_allowance_exceeded",
        "network_ethtool",
    ),
    (
        "network_ena_conntrack_allowance_exceeded",
        "network_ethtool",
    ),
    (
        "network_ena_linklocal_allowance_exceeded",
        "network_ethtool",
    ),
    ("network_ena_pps_allowance_exceeded", "network_ethtool"),
    ("network_packets", "network_traffic"),
    ("network_transmit_busy", "network_interfaces"),
    ("network_transmit_complete", "network_interfaces"),
    ("network_transmit_timeout", "network_interfaces"),
    ("package_c10_residency", "cpu_power"),
    ("package_c1_residency", "cpu_power"),
    ("package_c2_residency", "cpu_power"),
    ("package_c3_residency", "cpu_power"),
    ("package_c6_residency", "cpu_power"),
    ("package_c7_residency", "cpu_power"),
    ("package_c8_residency", "cpu_power"),
    ("package_c9_residency", "cpu_power"),
    ("rezolus_blockio_operations", "rezolus_rusage"),
    ("rezolus_context_switch", "rezolus_rusage"),
    ("rezolus_cpu_usage", "rezolus_rusage"),
    ("rezolus_memory_page_faults", "rezolus_rusage"),
    ("rezolus_memory_page_reclaims", "rezolus_rusage"),
    ("rezolus_memory_usage_resident_set_size", "rezolus_rusage"),
    ("scheduler_context_switch", "scheduler_runqueue"),
    ("scheduler_discarded_samples", "scheduler_runqueue"),
    ("scheduler_offcpu", "scheduler_runqueue"),
    ("scheduler_running", "scheduler_runqueue"),
    ("softirq", "cpu_usage"),
    ("softirq_time", "cpu_usage"),
    ("syscall", "syscall_counts"),
    ("task_cpu_usage", "cpu_usage"),
    ("tcp_bytes", "tcp_traffic"),
    ("tcp_jitter", "tcp_receive"),
    ("tcp_packets", "tcp_traffic"),
    ("tcp_size", "tcp_traffic"),
    ("tcp_srtt", "tcp_receive"),
];

/// The samplers that self-report `rezolus_bpf_run_count`/
/// `rezolus_bpf_run_time`: every eBPF-backed sampler declares its own pair
/// of these two metrics under its own module (self-timing instrumentation
/// baked into each BPF-backed `stats.rs`), so the name alone can't tell you
/// which one ran. Harvested by reading every `stats.rs` that declares
/// `rezolus_bpf_run_count`; matches the "BPF-enabled samplers" list in
/// `CLAUDE.md` (`blockio/{latency,requests}`,
/// `cpu/{bandwidth,migrations,perf,tlb_flush,usage}`,
/// `network/{interfaces,traffic}`, `scheduler/runqueue`,
/// `syscall/{counts,latency}`,
/// `tcp/{connect_latency,packet_latency,receive,retransmit,traffic}`) —
/// cross-checked independently rather than assumed from that doc. Sorted
/// alphabetically.
const BPF_SAMPLERS: &[&str] = &[
    "blockio_latency",
    "blockio_requests",
    "cpu_bandwidth",
    "cpu_migrations",
    "cpu_perf",
    "cpu_tlb_flush",
    "cpu_usage",
    "network_interfaces",
    "network_traffic",
    "scheduler_runqueue",
    "syscall_counts",
    "syscall_latency",
    "tcp_connect_latency",
    "tcp_packet_latency",
    "tcp_receive",
    "tcp_retransmit",
    "tcp_traffic",
];

/// Metric names declared identically by more than one sampler (verified by
/// reading every sampler's `stats.rs`): a flat name -> sampler mapping
/// can't be correct for all of them, but the name still proves *one of* its
/// candidate samplers ran. `subsystem_of` resolves these to `unattributed`
/// (deliberately absent from [`METRIC_SAMPLERS`] and from
/// `metric_samplers_match_agent_attribution`'s strict per-metric check)
/// rather than guessing a single one — but `extract()` credits the
/// **candidate set**, not a whole name-token domain, to
/// `uncertain_samplers`, so `build_coverage` prunes exactly those samplers
/// from `subsystems_absent`. This is deliberately tighter than the
/// `uncertain_domains` fallback: crediting `rezolus_bpf_run_count`'s whole
/// `"rezolus"` domain, for example, would also prune the unrelated
/// `rezolus_rusage` sampler on essentially every unlabeled BPF recording —
/// a real precision loss the candidate-set form avoids.
///
/// Only consulted when inference is trusted (`infer`, see the module doc):
/// on a non-`rezolus` source we don't trust that a name like `gpu_clock`
/// came from *our* GPU samplers at all, so no candidate set is credited
/// there either — the metric just falls into `uncertain_domains` like any
/// other unlabeled name on an untrusted source.
///
/// Freshly recorded data (agents >= 5.17.1) is unaffected either way: the
/// `sampler` label resolves these correctly without consulting this table.
///
/// - `cpu_cores`: the Linux `cpu_cores` sampler's own metric, but also
///   emitted by the macOS `cpu_usage` sampler (macOS folds core-count
///   reporting into its usage sampler rather than having a standalone
///   `cpu_cores` one) — same name, different true sampler depending on
///   platform.
/// - `gpu_clock`, `gpu_energy_consumption`, `gpu_power_usage`,
///   `gpu_utilization`: shared vocabulary across `gpu_amd_smi`,
///   `gpu_apple`, and `gpu_nvidia`.
/// - `gpu_memory`, `gpu_memory_utilization`, `gpu_pcie_throughput`,
///   `gpu_temperature`: shared between `gpu_amd_smi` and `gpu_nvidia` only
///   (no macOS/`gpu_apple` equivalent — `gpu/macos/stats.rs` declares no
///   metric under these names).
/// - `rezolus_bpf_run_count`, `rezolus_bpf_run_time`: see [`BPF_SAMPLERS`].
///
/// Sorted alphabetically by name; each candidate list sorted alphabetically
/// too.
pub(crate) const AMBIGUOUS_METRICS: &[(&str, &[&str])] = &[
    ("cpu_cores", &["cpu_cores", "cpu_usage"]),
    ("gpu_clock", &["gpu_amd_smi", "gpu_apple", "gpu_nvidia"]),
    (
        "gpu_energy_consumption",
        &["gpu_amd_smi", "gpu_apple", "gpu_nvidia"],
    ),
    ("gpu_memory", &["gpu_amd_smi", "gpu_nvidia"]),
    ("gpu_memory_utilization", &["gpu_amd_smi", "gpu_nvidia"]),
    ("gpu_pcie_throughput", &["gpu_amd_smi", "gpu_nvidia"]),
    (
        "gpu_power_usage",
        &["gpu_amd_smi", "gpu_apple", "gpu_nvidia"],
    ),
    ("gpu_temperature", &["gpu_amd_smi", "gpu_nvidia"]),
    (
        "gpu_utilization",
        &["gpu_amd_smi", "gpu_apple", "gpu_nvidia"],
    ),
    ("rezolus_bpf_run_count", BPF_SAMPLERS),
    ("rezolus_bpf_run_time", BPF_SAMPLERS),
];

/// Metric-name domains that diverge from their owning sampler's leading
/// token — the sampler's metrics don't share its name prefix, so the plain
/// first-token rule below would compute the wrong domain for them. Applied
/// uniformly to sampler names too for simplicity; harmless in practice
/// since no *sampler* name in [`EXPECTED_SUBSYSTEMS`] starts with `drive_`,
/// `gpmu_`, `core_` or `package_` (`domain_of("drivehealth")` still yields
/// `"drivehealth"` — its first token doesn't match any alias key).
const DOMAIN_ALIASES: &[(&str, &str)] = &[
    ("core", "cpu"),          // cpu_power sampler emits core_c*_residency metrics
    ("drive", "drivehealth"), // drivehealth sampler emits drive_* metrics
    ("gpmu", "gpu"),          // gpu_amd_pmu sampler emits gpmu_* metrics
    ("package", "cpu"),       // cpu_power sampler emits package_c*_residency metrics
];

/// Domain of a sampler or metric name: its first `_`-separated token, except
/// names prefixed `cgroup_` strip that prefix first and take the *next*
/// token (`cgroup_cpu_usage` -> `cpu`, `cgroup_scheduler_offcpu` ->
/// `scheduler`, `cgroup_syscall` -> `syscall`), and the result is mapped
/// through [`DOMAIN_ALIASES`] (`drive_temperature` -> `drive` -> aliased to
/// `drivehealth`; `gpmu_busy_cycles` -> `gpmu` -> aliased to `gpu`;
/// `core_c6_residency` -> `core` and `package_c6_residency` -> `package`, both
/// aliased to `cpu`, since `cpu_power` emits them without its own prefix).
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

/// The two "we don't know" sets `extract()` accumulates while attributing
/// metric names to subsystems, bundled into one type so `build_context`
/// doesn't need an unrelated arg-count workaround for what is conceptually
/// a single uncertainty bundle (see the module doc for what each means).
pub(crate) struct Uncertainty<'a> {
    /// Domains ([`domain_of`]) with at least one still-`unattributed`
    /// metric not covered by a tighter [`AMBIGUOUS_METRICS`] entry.
    pub domains: &'a BTreeSet<String>,
    /// Samplers named directly as an [`AMBIGUOUS_METRICS`] candidate for
    /// some unattributed metric — pruned individually rather than by
    /// domain.
    pub samplers: &'a BTreeSet<String>,
}

/// present = the distinct `sampler` label values observed in the recording;
/// callers insert the synthetic `unattributed` for metrics lacking the
/// label (this function does not add it). absent = the static universe
/// minus present, minus any sampler whose domain is in
/// `uncertainty.domains` (its true presence/absence can't be known, so we
/// exclude it from `subsystems_absent` rather than assert a false absence),
/// minus any sampler named directly in `uncertainty.samplers` (the exact
/// candidate set of an [`AMBIGUOUS_METRICS`] name seen unlabeled — see that
/// constant's doc for why this is tighter than domain pruning). Neither set
/// is added to present either, since we don't fabricate presence we can't
/// confirm. Both output lists sorted (BTreeSet iteration order).
pub(crate) fn build_coverage(present: &BTreeSet<String>, uncertainty: &Uncertainty) -> Coverage {
    Coverage {
        subsystems_present: present.iter().cloned().collect(),
        subsystems_absent: EXPECTED_SUBSYSTEMS
            .iter()
            .filter(|&&s| {
                !present.contains(s)
                    && !uncertainty.domains.contains(domain_of(s))
                    && !uncertainty.samplers.contains(s)
            })
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
    uncertainty: &Uncertainty,
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
        coverage: build_coverage(present, uncertainty),
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
        // empty uncertainty sets preserve old (pre-inference) behavior.
        let c = build_coverage(
            &present,
            &Uncertainty {
                domains: &BTreeSet::new(),
                samplers: &BTreeSet::new(),
            },
        );
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
        let c = build_coverage(
            &present,
            &Uncertainty {
                domains: &uncertain,
                samplers: &BTreeSet::new(),
            },
        );
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
        let c = build_coverage(
            &present,
            &Uncertainty {
                domains: &uncertain,
                samplers: &BTreeSet::new(),
            },
        );
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
    fn coverage_prunes_exactly_the_candidate_set_for_an_ambiguous_metric() {
        // As extraction would compute it for an unlabeled cpu_cores metric
        // on a trusted (rezolus) source: exactly {cpu_cores, cpu_usage} is
        // credited, not the whole "cpu" domain.
        let present = BTreeSet::new();
        let mut uncertain_samplers = BTreeSet::new();
        uncertain_samplers.insert("cpu_cores".to_string());
        uncertain_samplers.insert("cpu_usage".to_string());
        let c = build_coverage(
            &present,
            &Uncertainty {
                domains: &BTreeSet::new(),
                samplers: &uncertain_samplers,
            },
        );
        assert!(!c.subsystems_absent.contains(&"cpu_cores".to_string()));
        assert!(!c.subsystems_absent.contains(&"cpu_usage".to_string()));
        // every other "cpu" sampler still asserts absence -- the candidate
        // set is exact, not the whole domain.
        assert!(c.subsystems_absent.contains(&"cpu_perf".to_string()));
        assert!(c.subsystems_absent.contains(&"cpu_bandwidth".to_string()));
        assert!(c.subsystems_present.is_empty());
    }

    #[test]
    fn coverage_prunes_bpf_set_without_touching_unrelated_rezolus_rusage() {
        // rezolus_bpf_run_count's candidate set is BPF_SAMPLERS; it must
        // NOT prune rezolus_rusage even though domain_of both names is
        // "rezolus" -- this is exactly the precision loss the candidate-set
        // form (vs. whole-domain pruning) is meant to avoid.
        let present = BTreeSet::new();
        let uncertain_samplers: BTreeSet<String> =
            BPF_SAMPLERS.iter().map(|s| s.to_string()).collect();
        let c = build_coverage(
            &present,
            &Uncertainty {
                domains: &BTreeSet::new(),
                samplers: &uncertain_samplers,
            },
        );
        for sampler in BPF_SAMPLERS {
            assert!(
                !c.subsystems_absent.contains(&sampler.to_string()),
                "{sampler} should be pruned (in the BPF candidate set)"
            );
        }
        assert!(
            c.subsystems_absent.contains(&"rezolus_rusage".to_string()),
            "rezolus_rusage must still assert absence -- it is not a BPF sampler"
        );
    }

    #[test]
    fn context_fields_normalized() {
        let empty_uncertainty = Uncertainty {
            domains: &BTreeSet::new(),
            samplers: &BTreeSet::new(),
        };
        let ctx = build_context(
            "rezolus".to_string(),
            String::new(), // .rez recordings have no version metadata
            120.0,
            1.0,
            Some(r#"{"os":"linux"}"#.to_string()),
            &BTreeSet::new(),
            &empty_uncertainty,
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
            &empty_uncertainty,
        );
        assert_eq!(bad.agent_version.as_deref(), Some("1.2.3"));
        assert_eq!(bad.systeminfo, None);
    }
}
