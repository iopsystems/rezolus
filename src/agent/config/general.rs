use super::*;

fn default_snapshot_format() -> SnapshotFormat {
    SnapshotFormat::V3
}

/// The wire format used for metric snapshots served on `/metrics/binary`.
///
/// `V3` (the default) emits acquisition-group snapshots: metrics are grouped
/// by the read that produced them, and the group carries one acquisition
/// window rather than every metric carrying its own. `V2` is the older flat
/// per-metric format, kept as an escape hatch for a fleet whose consumers
/// have not caught up.
///
/// Setting this back to `V2` is for one situation: a consumer that cannot
/// yet read V3. See `config/agent.toml` for the operator detail. The short
/// version is that `/metrics/json` changes shape wholesale — a flat
/// `{counters, gauges, histograms}` becomes `{groups: [...]}` — so anything
/// parsing that endpoint directly needs updating, and a consumer built
/// before `SnapshotV3` existed cannot decode `/metrics/binary` at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotFormat {
    V2,
    /// Marked `#[default]` so `SnapshotFormat::default()` agrees with
    /// `default_snapshot_format()`. They are reached by different paths — the
    /// derive covers a `General` built without deserializing, the function
    /// covers a config file that omits the key — and a fleet where those two
    /// disagreed would serve different wire formats depending on how its
    /// config happened to be loaded.
    #[default]
    V3,
}

#[derive(Deserialize, Default)]
pub struct General {
    #[serde(default = "listen")]
    listen: String,

    // the agent caches metrics snapshots with the following TTL to prevent
    // excessive resource utilization
    #[serde(default = "ttl")]
    ttl: String,

    // path to external BTF file for BPF programs (optional)
    #[serde(default)]
    btf_path: Option<String>,

    // wire format for metric snapshots served on `/metrics/binary` (see
    // `SnapshotFormat` for details); transitional, defaults to v2
    #[serde(default = "default_snapshot_format")]
    snapshot_format: SnapshotFormat,

    // PMU hardware counters to leave free for everything else on the machine
    // (see `reserved_pmu_counters`)
    #[serde(default)]
    reserved_pmu_counters: usize,

    // order in which PMU samplers claim counters (see `pmu_priority`)
    #[serde(default)]
    pmu_priority: Vec<String>,

    // CPUs the reservation applies to (see `reserved_pmu_cpus`)
    #[serde(default)]
    reserved_pmu_cpus: Option<String>,
}

impl General {
    pub fn check(&self) {
        if let Err(e) = self.ttl.parse::<humantime::Duration>() {
            eprintln!("ttl couldn't be parsed: {e}");
            std::process::exit(1);
        }

        if let Some(ref spec) = self.reserved_pmu_cpus {
            if crate::agent::pmu::parse_cpu_list(spec).is_none() {
                eprintln!(
                    "reserved_pmu_cpus couldn't be parsed: {spec:?} (expected a list like \
                     \"0-3,8,12-15\")"
                );
                std::process::exit(1);
            }
        }

        if let Some(ref btf_path) = self.btf_path {
            if !std::path::Path::new(btf_path).exists() {
                eprintln!("BTF file not found: {btf_path}");
                std::process::exit(1);
            }
        }
    }

    /// Hardware counters to leave free for PMU consumers other than this agent.
    ///
    /// Defaults to 0, which is the behaviour that shipped before budgeting
    /// existed: the agent claims what it can and whatever else wanted counters
    /// goes without.
    ///
    /// Raising it matters most on a hypervisor. KVM backs a guest's virtual PMU
    /// with host perf events, so an exhausted host PMU leaves guests with a
    /// vPMU that reports itself working and counts nothing — measured, a guest
    /// retiring more than 1e9 instructions read 395 of them, and could not tell
    /// from the inside that anything was wrong. Reserving trades some of this
    /// agent's own metrics for theirs.
    pub fn reserved_pmu_counters(&self) -> usize {
        self.reserved_pmu_counters
    }

    /// The order in which PMU samplers claim counters; empty means the built-in
    /// default (see `crate::agent::pmu::DEFAULT_PRIORITY`).
    ///
    /// Worth overriding when the default's ranking disagrees with the
    /// investigation at hand — someone chasing cache behaviour legitimately
    /// wants `cpu_l3` to outrank `cpu_perf`.
    pub fn pmu_priority(&self) -> &[String] {
        &self.pmu_priority
    }

    /// The CPUs `reserved_pmu_counters` applies to, as a list like
    /// `"0-3,8,12-15"`. Unset means every CPU.
    ///
    /// Reserving uniformly is usually more than is needed. The consumers a
    /// reservation protects are rarely spread evenly — guest vCPUs run on some
    /// cores, an isolated workload lives on specific ones — so holding counters
    /// back everywhere costs the agent coverage on CPUs where nothing else
    /// wanted them.
    pub fn reserved_pmu_cpus(&self) -> Option<&str> {
        self.reserved_pmu_cpus.as_deref()
    }

    pub fn listen(&self) -> SocketAddr {
        self.listen
            .to_socket_addrs()
            .map_err(|e| {
                eprintln!("bad listen address: {e}");
                std::process::exit(1);
            })
            .unwrap()
            .next()
            .ok_or_else(|| {
                eprintln!("could not resolve socket addr");
                std::process::exit(1);
            })
            .unwrap()
    }

    pub fn ttl(&self) -> std::time::Duration {
        *self.ttl.parse::<humantime::Duration>().unwrap()
    }

    #[allow(dead_code)]
    pub fn btf_path(&self) -> Option<&str> {
        self.btf_path.as_deref()
    }

    pub fn snapshot_format(&self) -> SnapshotFormat {
        self.snapshot_format
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn general(toml: &str) -> General {
        toml::from_str(toml).expect("valid config")
    }

    #[test]
    fn snapshot_format_v3_parses() {
        let g = general("snapshot_format = \"v3\"\n");
        assert_eq!(g.snapshot_format(), SnapshotFormat::V3);
    }

    #[test]
    fn snapshot_format_defaults_to_v3_when_absent() {
        let g = general("");
        assert_eq!(g.snapshot_format(), SnapshotFormat::V3);
    }

    /// The two defaults must agree.
    ///
    /// `#[serde(default = "default_snapshot_format")]` covers a config file
    /// that omits the key; `#[derive(Default)]` on the enum covers a `General`
    /// built without deserializing at all. They are reached by different paths,
    /// and a fleet where they disagreed would serve different wire formats
    /// depending on how its config happened to be loaded.
    #[test]
    fn the_serde_default_and_the_derived_default_agree() {
        assert_eq!(default_snapshot_format(), SnapshotFormat::default());
    }

    /// v2 stays reachable. It is the escape hatch for a consumer that cannot
    /// read v3 yet, so it has to keep parsing, not merely keep existing.
    #[test]
    fn snapshot_format_v2_is_still_selectable() {
        let g = general("snapshot_format = \"v2\"\n");
        assert_eq!(g.snapshot_format(), SnapshotFormat::V2);
    }

    #[test]
    fn snapshot_format_rejects_unknown_value() {
        let result: Result<General, _> = toml::from_str("snapshot_format = \"v9\"\n");
        assert!(result.is_err());
    }
}
