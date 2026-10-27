//! Cumulative memory event counters on macOS: faults, swap traffic, the
//! compressor, and file paging.
//!
//! Swap on macOS holds compressed memory: `swapins` and `swapouts` count the
//! pages of compressed segments moved to and from the swap files, which is
//! the unit Linux's `pswpin` and `pswpout` use.

const NAME: &str = "memory_vmstat";

use crate::agent::*;

use super::{host, vm_statistics};

mod stats {
    include!("../../linux/vmstat/stats.rs");
}
mod macos_stats;

use macos_stats::*;
use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let host = host();

    if vm_statistics(host).is_none() {
        return Err(crate::agent::sampler_status::Unsupported(
            "host_statistics64 has no compressor counts".to_string(),
        )
        .into());
    }

    Ok(Some(Box::new(Vmstat { host })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Vmstat {
    host: libc::mach_port_t,
}

#[async_trait]
impl Sampler for Vmstat {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let guard = VMSTAT_ACQ.acquire();

        let Some(vm) = vm_statistics(self.host) else {
            return;
        };

        MEMORY_PAGE_FAULTS.set(vm.faults);
        MEMORY_SWAP_IN.set(vm.swapins);
        MEMORY_SWAP_OUT.set(vm.swapouts);

        MEMORY_COMPRESSIONS.set(vm.compressions);
        MEMORY_DECOMPRESSIONS.set(vm.decompressions);
        MEMORY_PAGEINS.set(vm.pageins);
        MEMORY_PAGEOUTS.set(vm.pageouts);

        guard.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // See the identical test in `memory_meminfo`.
    #[test]
    fn stats_attribute_to_this_sampler() {
        let module = module_path!().trim_end_matches("::tests");
        let modules = crate::agent::samplers::sampler_modules();

        for stats in ["stats", "macos_stats"] {
            assert_eq!(
                crate::agent::samplers::attribute_sampler(&format!("{module}::{stats}"), &modules),
                NAME
            );
        }
        assert_eq!(VMSTAT_ACQ.sampler, NAME);
    }

    #[tokio::test]
    async fn refresh_sets_the_mapped_counters_and_stamps_the_group() {
        let sampler = init(Arc::new(toml::from_str("").unwrap()))
            .unwrap()
            .unwrap();
        sampler.refresh().await;

        assert!(VMSTAT_ACQ.window().is_some());

        // Every booted Mac has faulted and compressed pages.
        assert!(MEMORY_PAGE_FAULTS.value() > 0);
        assert!(MEMORY_COMPRESSIONS.value() > 0);
        assert!(metriken::Lazy::get(&MEMORY_SWAP_IN).is_some());

        // No macOS equivalent; stays unset.
        assert!(metriken::Lazy::get(&MEMORY_MAJOR_PAGE_FAULTS).is_none());
    }
}
