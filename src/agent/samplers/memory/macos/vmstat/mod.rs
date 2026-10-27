//! Cumulative memory event counters on macOS, and wired memory: the values
//! no sysctl reports, from one `host_statistics64` call per refresh.
//!
//! That call can return a cached result (see the module doc in
//! `memory/macos`), and the kernel copies into that cache without a lock, so
//! a cached result can be older than one this sampler already published, or
//! mix fields from two reads. A read is published only if its fault count is
//! higher than the last published one and none of its counters is lower;
//! otherwise the group keeps its previous window and the series has no new
//! point. Faults are counted continuously on a running Mac, so a real read
//! almost always passes. A newer copy cached by another process's call cannot
//! be told apart from a fresh read; it is from the current one-second window.
//!
//! Swap on macOS holds compressed memory. `memory_swap_in` and
//! `memory_swap_out` count kernel pages (16 KiB on Apple silicon) of
//! compressed data moved to and from the swap files, not pages of memory as
//! Linux's `pswpin` and `pswpout` do.

const NAME: &str = "memory_vmstat";

use crate::agent::*;

use super::{page_size, vm_statistics};

use std::sync::Mutex;

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
        last: Mutex::new(None),
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
    /// The counters of the last read published.
    last: Mutex<Option<Counters>>,
}

/// The cumulative counters, in the order they are compared and published:
/// faults, compressions, decompressions, page-ins, page-outs, swap-ins,
/// swap-outs.
type Counters = [u64; 7];

fn counters(vm: &libc::vm_statistics64) -> Counters {
    [
        vm.faults,
        vm.compressions,
        vm.decompressions,
        vm.pageins,
        vm.pageouts,
        vm.swapins,
        vm.swapouts,
    ]
}

/// True if `now` is newer than `last`: more faults, and no counter lower.
fn is_newer(now: &Counters, last: &Counters) -> bool {
    now[0] > last[0] && now.iter().zip(last).all(|(n, l)| n >= l)
}

impl Vmstat {
    /// Set every metric from `vm` and return true, or return false and set
    /// nothing if `vm` is not newer than the last published read.
    fn publish(&self, vm: &libc::vm_statistics64) -> bool {
        let now = counters(vm);
        let mut last = self.last.lock().unwrap();
        if last.is_some_and(|last| !is_newer(&now, &last)) {
            return false;
        }

        let [faults, compressions, decompressions, pageins, pageouts, swapins, swapouts] = now;
        MEMORY_PAGE_FAULTS.set(faults);
        MEMORY_COMPRESSIONS.set(compressions);
        MEMORY_DECOMPRESSIONS.set(decompressions);
        MEMORY_PAGEINS.set(pageins);
        MEMORY_PAGEOUTS.set(pageouts);
        MEMORY_SWAP_IN.set(swapins);
        MEMORY_SWAP_OUT.set(swapouts);
        MEMORY_WIRED.set((vm.wire_count as u64 * self.page_size) as i64);

        *last = Some(now);
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
    fn only_a_newer_read_is_published() {
        let sampler = Vmstat {
            host: 0,
            page_size: 16384,
            last: Mutex::new(None),
        };
        // SAFETY: plain integers; all-zero is valid.
        let mut vm: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
        // Non-zero values: these statics are shared with the refresh test,
        // which asserts the fault count and wired memory are above zero.
        vm.faults = 7;
        vm.wire_count = 1;
        vm.compressions = 3;

        assert!(sampler.publish(&vm));
        assert!(!sampler.publish(&vm), "a repeat is a cached copy");

        vm.faults = 6;
        assert!(!sampler.publish(&vm), "fewer faults is an older copy");

        vm.faults = 8;
        vm.compressions = 2;
        assert!(!sampler.publish(&vm), "a counter went down");

        vm.compressions = 3;
        assert!(sampler.publish(&vm));
        assert_eq!(*sampler.last.lock().unwrap(), Some([8, 3, 0, 0, 0, 0, 0]));
    }
}
