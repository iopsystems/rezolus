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
/// # Granularity rule
///
/// A group is one read section over LIKE ENTITIES: several instances of
/// ONE metric family, distinguished from one another only by a label
/// within that family (a syscall class, a block IO op, a TCP direction, a
/// per-CPU index), share the sweep that reads them and therefore share ONE
/// group — not one group per instance. `cpu_usage`'s three `cpu_counters`
/// groups (`cpu`, `softirq`, `softirq_time`) are three separate BPF maps —
/// three separate read sections — each internally a per-CPU sweep of LIKE
/// entities (one group per map, not one per CPU). syscall_latency's 16
/// syscall-class latency histograms are the inverse shape: sixteen BPF
/// maps, but ONE read section over one conceptual family ("syscall
/// latency", distinguished by an `op` label) — one group, not sixteen.
///
/// DIFFERENT metric families keep separate groups even when their reads
/// happen back-to-back inside the same sampler's refresh: tcp_receive's
/// `srtt` and `jitter` are two distinct measurements, not label-instances
/// of one family, so they stay two groups; scheduler_runqueue's
/// `runqlat`/`running`/`offcpu` are three distinct families for the same
/// reason. See `docs/journal/2026-08-17-window-sidecar-cost.md`'s addendum
/// (which names syscall_latency, alongside drivehealth, as the
/// collapse-to-one-group case) and `src/agent/bpf/histogram.rs`'s
/// `HistogramBatch` for where this rule is mechanically enforced for
/// histograms.
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

/// Bound every acquisition group whose sampler did not come up to zero
/// members, and return how many were bounded.
///
/// Membership comes from registration, so a group's members exist as soon as it
/// is declared — whether or not anything ever writes to them. A sampler that is
/// disabled, unsupported on this platform, or failed to initialise never runs
/// the constructor that would call `set_member_bound`, so its groups keep the
/// unset bound and the snapshot walker falls back to the backing array's
/// `entries()` ceiling. That ceiling is an implementation maximum
/// (`MAX_CPUS` = 1024), not a population, so each such group contributes a
/// thousand-odd `None`s to every snapshot forever.
///
/// Measured before this: a macOS agent with 3 healthy samplers served a 3.47 MB
/// snapshot in which 43,456 of 43,469 scalar members were `None` — 99% phantom.
/// Every BPF sampler's `stats.rs` is compiled on non-Linux too (to keep metric
/// identity stable across platforms), where it attributes to `"unattributed"`
/// and no `SamplerEntry` exists, so *all* of those groups are unbacked at once.
///
/// The same shape reaches Linux whenever a sampler fails to load — an
/// unsupported kernel, a missing probe, a hardware counter that will not open.
/// #1081 fixed one instance of this by hand (GPUs of an absent vendor call
/// `set_member_bound(0)` in `init`); this closes the general case, so a newly
/// failing sampler cannot reintroduce it.
///
/// `Some(0)` is a real answer — "this group has no members" — and is what
/// distinguishes an empty group from an unbounded one. Only unbacked groups are
/// touched, and their samplers never set a bound, so nothing is overwritten.
pub fn bound_groups_without_a_live_sampler(
    live: &std::collections::HashSet<&'static str>,
) -> usize {
    bound_unbacked(ACQUISITION_GROUPS.iter().copied(), live)
}

/// The body of [`bound_groups_without_a_live_sampler`], over an explicit group
/// list so it can be exercised without mutating the real registered groups —
/// which are process-global statics that other tests read.
fn bound_unbacked<'a>(
    groups: impl Iterator<Item = &'a crate::agent::timing::AcquisitionGroup>,
    live: &std::collections::HashSet<&'static str>,
) -> usize {
    let mut bounded = 0;
    for group in groups {
        if !live.contains(group.sampler) {
            group.set_member_bound(0);
            bounded += 1;
        }
    }
    bounded
}

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

    /// The invariant `.rez`'s V3 native ingest rests on
    /// (`src/recorder/rez_v3_writer.rs`'s `is_group_table_key`): a V1/V2
    /// sampler table key is never mistaken for a V3 acquisition-group table
    /// key (`"<sampler>/<group>"`) because a plain sampler name never
    /// contains `/`. That discriminator is a naming convention doing
    /// structural work — this pins it against every name this binary can
    /// actually produce, the same way
    /// `acquisition_groups_have_unique_names_and_avoid_the_reserved_main_name`
    /// pins the acquisition-group naming rule above. A future sampler named
    /// with a `/` (there is no reason to, but nothing else stops it) would
    /// silently misroute its `.rez` table to the group decode path; this
    /// test — plus the `debug_assert!`s at both `StreamRecorderV3::ingest`
    /// and `ingest_v3`'s per-row loops — turns that into a loud failure
    /// instead.
    #[test]
    fn no_registered_sampler_name_contains_a_slash() {
        for entry in super::SAMPLERS {
            assert!(
                !entry.name.contains('/'),
                "sampler {:?} (module {}) contains '/', which `.rez`'s \
                 is_group_table_key reserves for V3 acquisition-group table keys",
                entry.name,
                entry.module
            );
        }
    }
}

#[cfg(test)]
mod unbacked_group_tests {
    use super::*;
    use crate::agent::timing::AcquisitionGroup;
    use std::collections::HashSet;

    static BACKED: AcquisitionGroup = AcquisitionGroup::new("live_sampler", "live_sampler_group");
    static UNBACKED: AcquisitionGroup = AcquisitionGroup::new("dead_sampler", "dead_sampler_group");

    /// A group whose sampler never came up declares NO members, while a group
    /// whose sampler is live is left completely alone.
    ///
    /// The second half is the one that matters: `Some(0)` means "this group has
    /// no members", so zeroing a live group would silently delete real metrics
    /// from every snapshot — a far worse failure than the phantom members this
    /// is fixing.
    #[test]
    fn only_groups_without_a_live_sampler_are_bounded() {
        let live: HashSet<&'static str> = ["live_sampler"].into_iter().collect();
        let bounded = bound_unbacked([&BACKED, &UNBACKED].into_iter(), &live);

        assert_eq!(bounded, 1, "exactly the unbacked group is bounded");
        assert_eq!(
            UNBACKED.member_bound(),
            Some(0),
            "a group with no live sampler must declare no members"
        );
        assert_eq!(
            BACKED.member_bound(),
            None,
            "a live sampler's group must keep its own bound — zeroing it would \
             erase real metrics"
        );
    }

    /// Every registered acquisition group names either a real sampler or the
    /// `"unattributed"` bucket — never anything else.
    ///
    /// This is the guard on the dangerous direction. Groups are matched to
    /// samplers by name, so a group naming a sampler that no longer exists —
    /// a rename, a typo — would be treated as unbacked and silently bounded to
    /// zero members, deleting its metrics from every snapshot. `Some(0)` is a
    /// real answer meaning "no members", so that failure would be silent.
    ///
    /// `"unattributed"` is deliberately allowed: it is the documented fallback
    /// [`attribute_sampler`] returns for a metric no `SamplerEntry` claims, and
    /// what [`bpf_sampler_name`] returns off Linux, so it is never a sampler
    /// name by design. Groups legitimately land there — including this crate's
    /// own `#[cfg(test)]` probe fixtures, which register into
    /// `ACQUISITION_GROUPS` under it and so are visible here but in no release
    /// binary. Being unbacked is the CORRECT state for that bucket; bounding it
    /// is the entire point of this mechanism.
    ///
    /// Linux only, because off Linux every BPF sampler's `stats.rs` compiles
    /// with no matching `SamplerEntry`, so the check would say nothing.
    #[cfg(target_os = "linux")]
    #[test]
    fn every_registered_group_names_a_real_sampler() {
        let known: HashSet<&'static str> = SAMPLERS.iter().map(|e| e.name).collect();
        let orphans: Vec<&str> = ACQUISITION_GROUPS
            .iter()
            .map(|g| g.sampler)
            .filter(|s| *s != "unattributed" && !known.contains(s))
            .collect();
        assert!(
            orphans.is_empty(),
            "these groups name a sampler that does not exist, so they would be \
             bounded to zero members and their metrics would vanish: {orphans:?}"
        );
    }
}
