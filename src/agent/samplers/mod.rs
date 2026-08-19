use crate::agent::Config;
use crate::debug;
use crate::Instant;
use async_trait::async_trait;
use linkme::distributed_slice;
use std::sync::Arc;

mod blockio;
mod cpu;
mod drivehealth;
mod gpu;
mod memory;
mod network;
mod rezolus;
mod scheduler;
mod syscall;
mod tcp;

/// A registered sampler: its stable name plus its init function.
pub struct SamplerEntry {
    pub name: &'static str,
    pub module: &'static str,
    pub init: fn(config: Arc<Config>) -> SamplerResult,
}

#[distributed_slice]
pub static SAMPLERS: [SamplerEntry] = [..];

/// Declared acquisition groups (design: one per source read section).
/// Samplers register their groups here; the V3 snapshot builder enumerates
/// this slice. `(sampler, name)` pairs must be unique and the name `main`
/// is reserved for the transitional default group.
///
/// # Naming rule
///
/// A group's `name` must be `<sampler>_<shortname>`, where `<sampler>` is
/// the Linux sampler name (e.g. `"blockio_requests"`) and `<shortname>` is
/// the map/builder-call it brackets (e.g. `"errors"`), UNLESS `<shortname>`
/// is identical to `<sampler>` itself — a full duplicate that would stutter
/// (`cpu_usage_cpu_usage`) — in which case use a short, unambiguous token
/// instead (`cpu_usage`'s own per-state group is `cpu_usage_cpu`, not
/// `cpu_usage_cpu_usage`).
///
/// This is stricter than "unique within the sampler" because every BPF
/// sampler's `stats.rs` is ALSO compiled directly on non-Linux platforms
/// (see `bpf_sampler_name`'s doc comment), where every one of them
/// attributes to the single shared `"unattributed"` sampler bucket — so two
/// different samplers both naming a group `"counters"` would collide there
/// even though they never collide on Linux. Always prefixing with the real
/// sampler name keeps the group name globally unique across every sampler
/// EVEN under that collapse. `group_registry()` (in
/// `agent::exposition::http::snapshot`) `debug_assert!`s this at first use.
#[distributed_slice]
pub static ACQUISITION_GROUPS: [&'static crate::agent::timing::AcquisitionGroup] = [..];

/// The (module_path, sampler_name) pairs for every registered sampler.
pub fn sampler_modules() -> Vec<(&'static str, &'static str)> {
    SAMPLERS.iter().map(|e| (e.module, e.name)).collect()
}

/// True when `prefix` is `module` or a `::`-delimited ancestor module of it.
fn is_module_prefix(prefix: &str, module: &str) -> bool {
    module == prefix
        || module
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with("::"))
}

/// The sampler name that [`attribute_sampler`] will actually resolve a
/// Linux-only BPF sampler's `stats.rs` module to, on the CURRENT platform.
///
/// A BPF sampler's metric statics live in `stats.rs`, which is compiled two
/// different ways: on Linux, `mod stats;` inside the real sampler's own
/// `mod.rs` (so the metric's module path is a descendant of the
/// `SamplerEntry` registered there, and `attribute_sampler` resolves it to
/// `linux_name`); on every other platform, `stats.rs` is `include!`d
/// directly under the sampler family's `#[cfg(not(target_os = "linux"))]
/// mod stats` fallback (to keep metric identity/descriptions stable across
/// platforms) with no matching `SamplerEntry` anywhere — so
/// `attribute_sampler` resolves it to `"unattributed"` instead, regardless
/// of `linux_name`.
///
/// An [`crate::agent::timing::AcquisitionGroup`] declared for such a
/// sampler's metrics must register under whichever of the two this platform
/// will actually produce, or `create_v3`'s routing (which looks the group
/// up by the resolved sampler name) can never find it.
#[cfg(target_os = "linux")]
pub(crate) const fn bpf_sampler_name(linux_name: &'static str) -> &'static str {
    linux_name
}

/// See the `#[cfg(target_os = "linux")]` overload's doc comment.
#[cfg(not(target_os = "linux"))]
pub(crate) const fn bpf_sampler_name(_linux_name: &'static str) -> &'static str {
    "unattributed"
}

/// Attribute a metric (identified by its definition module path) to the
/// sampler whose registered module is the longest prefix of that path. Metrics
/// with no matching sampler fall into the `"unattributed"` bucket.
pub fn attribute_sampler<'a>(metric_module: &str, samplers: &'a [(&'a str, &'a str)]) -> &'a str {
    samplers
        .iter()
        .filter(|(module, _)| is_module_prefix(module, metric_module))
        .max_by_key(|(module, _)| module.len())
        .map(|(_, name)| *name)
        .unwrap_or("unattributed")
}

#[async_trait]
pub trait Sampler: Send + Sync {
    fn name(&self) -> &'static str;

    async fn refresh(&self);

    async fn refresh_with_logging(&self) {
        let start = Instant::now();

        self.refresh().await;

        let duration = start.elapsed().as_micros();

        debug!("{} sampling latency: {duration} us", self.name());
    }
}

pub type SamplerResult = anyhow::Result<Option<Box<dyn Sampler>>>;

#[cfg(test)]
mod attribution_tests {
    use super::{attribute_sampler, is_module_prefix};

    #[test]
    fn longest_prefix_wins() {
        let samplers = [
            ("rezolus::agent::samplers::cpu", "cpu"),
            ("rezolus::agent::samplers::cpu::linux::usage", "cpu_usage"),
        ];
        assert_eq!(
            attribute_sampler(
                "rezolus::agent::samplers::cpu::linux::usage::stats",
                &samplers
            ),
            "cpu_usage",
        );
    }

    #[test]
    fn exact_module_match_attributes_to_itself() {
        let samplers = [("rezolus::agent::samplers::cpu::linux::usage", "cpu_usage")];
        assert_eq!(
            attribute_sampler("rezolus::agent::samplers::cpu::linux::usage", &samplers),
            "cpu_usage",
        );
    }

    #[test]
    fn no_prefix_falls_back_to_unattributed() {
        let samplers = [("rezolus::agent::samplers::cpu::linux::usage", "cpu_usage")];
        assert_eq!(
            attribute_sampler("rezolus::agent::external_metrics::store", &samplers),
            "unattributed"
        );
    }

    #[test]
    fn prefix_requires_component_boundary() {
        assert!(!is_module_prefix(
            "rezolus::a::cpu",
            "rezolus::a::cpurious::x"
        ));
        assert!(is_module_prefix("rezolus::a::cpu", "rezolus::a::cpu::x"));
        assert!(is_module_prefix("rezolus::a::cpu", "rezolus::a::cpu"));
    }

    #[test]
    fn registered_samplers_are_in_expected_universe() {
        for e in super::SAMPLERS {
            assert!(
                crate::analysis::extract::context::EXPECTED_SUBSYSTEMS.contains(&e.name),
                "sampler `{}` is not in EXPECTED_SUBSYSTEMS (src/analysis/extract/context.rs)",
                e.name
            );
        }
    }

    /// Drift guard for `crate::analysis::extract::context::METRIC_SAMPLERS`:
    /// walks the live `metriken` registry and asserts the analysis-side
    /// resolution (`subsystem_of` called with `infer = true`, i.e. its
    /// AMBIGUOUS_METRICS-then-METRIC_SAMPLERS-then-prefix tiers) agrees with
    /// the agent's own module-path attribution (`attribute_sampler`) for
    /// every registered metric it is *able* to check. `infer = true` here is
    /// deliberate and always correct for this test regardless of the
    /// `source`-gating rule extraction applies at runtime: this guard
    /// verifies the inference tiers' correctness in isolation, not whether
    /// a given recording should trust them.
    ///
    /// This test does NOT validate the table in full — two real gaps, stated
    /// plainly rather than glossed over:
    ///
    /// - It only checks metrics the CURRENT PLATFORM'S build registers.
    ///   `metriken::metrics()` only contains metrics compiled for the
    ///   current target, so a macOS run only exercises macOS samplers
    ///   (`cpu_usage`, `rezolus_rusage`) and macOS's cpu_cores/ambiguous
    ///   cases; a Linux CI run is what actually exercises the Linux-only
    ///   samplers (every BPF-backed one, drivehealth, the GPU vendor
    ///   samplers, etc). Passing on one platform does not certify the table
    ///   rows that platform never registers.
    /// - It cannot check metrics the agent's OWN `attribute_sampler` calls
    ///   `unattributed` — there is no ground truth to compare against. On
    ///   macOS today this silently excludes `gpu_apple`'s own metrics:
    ///   `gpu/macos/stats.rs` is a sibling module of `gpu/macos/apple.rs`,
    ///   not a descendant of the `apple` sampler's registered module, so
    ///   `attribute_sampler`'s module-prefix rule misses it. That's a
    ///   pre-existing quirk in `attribute_sampler` itself, unrelated to this
    ///   guard or to `METRIC_SAMPLERS`/`AMBIGUOUS_METRICS` — not something
    ///   this commit fixes.
    ///
    /// What IS tight: every metric the registry has an opinion on (agent
    /// attributes it to a real sampler, not `unattributed`) and whose name
    /// isn't in `AMBIGUOUS_METRICS` is unconditionally asserted — no further
    /// skipping. On failure, the assertion message names the correct
    /// `("metric", "sampler")` line to paste into `METRIC_SAMPLERS`.
    #[test]
    fn metric_samplers_match_agent_attribution() {
        use crate::analysis::extract::context::AMBIGUOUS_METRICS;
        use crate::analysis::extract::subsystem_of;

        let mods = super::sampler_modules();
        for metric in metriken::metrics().iter() {
            let name = metric.name();
            if AMBIGUOUS_METRICS.iter().any(|(n, _)| *n == name) {
                continue;
            }
            let truth = super::attribute_sampler(metric.module(), &mods);
            if truth == "unattributed" {
                continue;
            }
            let resolved = subsystem_of(name, &[], true);
            assert_eq!(
                resolved,
                truth,
                "metric `{name}` (module `{}`) attributes to sampler `{truth}` via \
                 attribute_sampler, but analysis-side subsystem_of resolves it to \
                 `{resolved}`; add (\"{name}\", \"{truth}\") to METRIC_SAMPLERS in \
                 src/analysis/extract/context.rs",
                metric.module(),
            );
        }
    }

    /// No PRODUCTION sampler has migrated to acquisition groups yet, but
    /// the slice is NOT empty in this test binary: the `snapshot` module's
    /// own V3-builder tests register fixture `AcquisitionGroup`s via
    /// `#[distributed_slice(ACQUISITION_GROUPS)]` (`linkme` slices are
    /// populated crate-wide at link time, cfg(test) included, regardless of
    /// which test actually runs). Guards future sampler migrations too:
    /// once real samplers start registering groups, this still catches a
    /// copy-pasted duplicate `(sampler, name)` pair or an accidental use of
    /// the reserved `"main"` group name.
    #[test]
    fn acquisition_groups_have_unique_names_and_avoid_the_reserved_main_name() {
        use std::collections::HashSet;

        let mut seen = HashSet::new();
        for group in super::ACQUISITION_GROUPS {
            assert_ne!(
                group.name, "main",
                "`main` is reserved for the snapshot builder's transitional default group \
                 (sampler `{}`)",
                group.sampler
            );
            assert!(
                seen.insert((group.sampler, group.name)),
                "duplicate acquisition group ({}, {})",
                group.sampler,
                group.name
            );
        }
    }
}
