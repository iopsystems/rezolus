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
}
