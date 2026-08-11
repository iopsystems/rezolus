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
    /// resolution (`subsystem_of` with no labels, i.e. its
    /// METRIC_SAMPLERS-then-prefix tiers) agrees with the agent's own
    /// module-path attribution (`attribute_sampler`) for every registered
    /// metric it can unambiguously check.
    ///
    /// Platform-gated by compilation: `metriken::metrics()` only contains
    /// metrics compiled for the current target, so this test only exercises
    /// whatever samplers that target registers. On macOS that's `cpu_usage`
    /// and `rezolus_rusage` (`gpu_apple`'s own metrics live in a sibling
    /// `stats` module under `gpu::macos`, not a descendant of the `apple`
    /// sampler's module, so `attribute_sampler` itself calls them
    /// `unattributed` today — a pre-existing quirk unrelated to this guard,
    /// and covered by the "skip unattributed" rule below). Linux CI is what
    /// actually exercises the Linux-only samplers (every BPF-backed one,
    /// drivehealth, the GPU vendor samplers, etc) — this test passing on
    /// one platform does not certify the table for the others.
    ///
    /// Two kinds of metric are skipped rather than asserted on:
    /// - Metrics the agent itself attributes `unattributed` (no ground
    ///   truth to check against).
    /// - Names in `AMBIGUOUS_METRIC_NAMES`: declared identically by more
    ///   than one sampler, so no flat table entry could be correct for all
    ///   of them (see that constant's doc in context.rs).
    ///
    /// On failure, the assertion message names the correct
    /// `("metric", "sampler")` line to paste into `METRIC_SAMPLERS`.
    #[test]
    fn metric_samplers_match_agent_attribution() {
        use crate::analysis::extract::context::AMBIGUOUS_METRIC_NAMES;
        use crate::analysis::extract::subsystem_of;

        let mods = super::sampler_modules();
        for metric in metriken::metrics().iter() {
            let name = metric.name();
            if AMBIGUOUS_METRIC_NAMES.contains(&name) {
                continue;
            }
            let truth = super::attribute_sampler(metric.module(), &mods);
            if truth == "unattributed" {
                continue;
            }
            let resolved = subsystem_of(name, &[]);
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
}
