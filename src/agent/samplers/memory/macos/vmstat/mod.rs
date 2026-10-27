//! Cumulative memory event counters on macOS, and wired memory: the values
//! no sysctl reports, from one `host_statistics64` call per refresh.
//!
//! That call can return a cached result (see the module doc in
//! `memory/macos`). A cached copy of this sampler's own previous read has the
//! same fault count, and is discarded: the group keeps its previous window
//! and the series has no new point. Faults are counted continuously on a
//! running Mac, so two real reads with the same count are not expected. A
//! copy cached by another process's call cannot be told apart from a fresh
//! read; it is at most one second old.
//!
//! Swap on macOS holds compressed memory. `memory_swap_in` and
//! `memory_swap_out` come from the `vm.compressor.swapper` sysctls and count
//! kernel pages (16 KiB on Apple silicon) of compressed data moved to and
//! from the swap files, not pages of memory as Linux's `pswpin` and `pswpout`
//! do.

const NAME: &str = "memory_vmstat";

use crate::agent::*;

use super::{page_size, sysctl_u64, vm_statistics};

use std::sync::atomic::{AtomicU64, Ordering};

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

    #[allow(deprecated)]
    // SAFETY: no preconditions; returns this task's host port.
    let host = unsafe { libc::mach_host_self() };
    let page_size = page_size().ok_or_else(|| anyhow::anyhow!("{NAME}: no vm.pagesize"))?;

    if vm_statistics(host).is_none() {
        return Err(crate::agent::sampler_status::Unsupported(
            "host_statistics64 failed or returned fewer than its revision 1 fields".to_string(),
        )
        .into());
    }

    Ok(Some(Box::new(Vmstat {
        host,
        page_size,
        last_faults: AtomicU64::new(0),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Vmstat {
    host: libc::mach_port_t,
    page_size: u64,
    /// The fault count of the last read published.
    last_faults: AtomicU64,
}

impl Vmstat {
    /// Set every metric from `vm` and return true, or return false and set
    /// nothing if `vm` repeats the last published read's fault count.
    fn publish(&self, vm: &libc::vm_statistics64) -> bool {
        if vm.faults == self.last_faults.load(Ordering::Relaxed) {
            return false;
        }

        MEMORY_PAGE_FAULTS.set(vm.faults);
        MEMORY_COMPRESSIONS.set(vm.compressions);
        MEMORY_DECOMPRESSIONS.set(vm.decompressions);
        MEMORY_PAGEINS.set(vm.pageins);
        MEMORY_PAGEOUTS.set(vm.pageouts);
        MEMORY_WIRED.set((vm.wire_count as u64 * self.page_size) as i64);

        if let Some(n) = sysctl_u64(c"vm.compressor.swapper.swapins_total") {
            MEMORY_SWAP_IN.set(n);
        }
        if let Some(n) = sysctl_u64(c"vm.compressor.swapper.swapouts_total") {
            MEMORY_SWAP_OUT.set(n);
        }

        self.last_faults.store(vm.faults, Ordering::Relaxed);
        true
    }
}

#[async_trait]
impl Sampler for Vmstat {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let guard = VMSTAT_ACQ.acquire();

        if vm_statistics(self.host).is_some_and(|vm| self.publish(&vm)) {
            guard.finish();
        }
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

        assert!(MEMORY_PAGE_FAULTS.value() > 0);
        assert!(metriken::Lazy::get(&MEMORY_COMPRESSIONS).is_some());
        assert!(MEMORY_WIRED.value() > 0);

        // No macOS equivalent; stays unset.
        assert!(metriken::Lazy::get(&MEMORY_MAJOR_PAGE_FAULTS).is_none());
    }

    #[test]
    fn a_repeated_read_publishes_nothing() {
        let sampler = Vmstat {
            host: 0,
            page_size: 16384,
            last_faults: AtomicU64::new(0),
        };
        // SAFETY: plain integers; all-zero is valid.
        let mut vm: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
        // Non-zero values: these statics are shared with the refresh test,
        // which asserts the fault count and wired memory are above zero.
        vm.faults = 7;
        vm.wire_count = 1;
        vm.compressions = 3;

        assert!(sampler.publish(&vm));
        vm.compressions = 4;
        assert!(!sampler.publish(&vm), "same fault count is a cached copy");
        assert_eq!(sampler.last_faults.load(Ordering::Relaxed), 7);
    }
}
