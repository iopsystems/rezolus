//! Memory gauges on macOS: capacity, the page cache and anonymous memory,
//! swap, and the compressor.
//!
//! `memory_available` has the Linux meaning: memory the kernel can hand out
//! without compressing or swapping anonymous memory. That is free pages,
//! file-backed pages and purgeable pages. Speculative pages (file read-ahead)
//! are counted in both `free_count` and the file-backed count, so they are
//! taken out of free. Activity Monitor's "Memory Used" leaves out roughly the
//! same pages, so total minus it is close to this figure but not equal.

const NAME: &str = "memory_meminfo";

use crate::agent::*;

use super::{host, page_size, sysctl, vm_statistics};

mod stats {
    include!("../../linux/meminfo/stats.rs");
}
mod macos_stats;

use macos_stats::*;
use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let host = host();
    let page_size = page_size().ok_or_else(|| anyhow::anyhow!("{NAME}: no hw.pagesize"))?;
    let total =
        sysctl::<u64>(c"hw.memsize").ok_or_else(|| anyhow::anyhow!("{NAME}: no hw.memsize"))?;

    if vm_statistics(host).is_none() {
        return Err(crate::agent::sampler_status::Unsupported(
            "host_statistics64 has no compressor counts".to_string(),
        )
        .into());
    }

    Ok(Some(Box::new(Meminfo {
        host,
        page_size,
        total,
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Meminfo {
    host: libc::mach_port_t,
    page_size: u64,
    total: u64,
}

#[async_trait]
impl Sampler for Meminfo {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let guard = MEMINFO_ACQ.acquire();

        let Some(vm) = vm_statistics(self.host) else {
            return;
        };
        let swap = sysctl::<libc::xsw_usage>(c"vm.swapusage");

        let bytes = |pages: u64| (pages * self.page_size) as i64;

        let free = (vm.free_count.saturating_sub(vm.speculative_count)) as u64;
        let file = vm.external_page_count as u64;
        let purgeable = vm.purgeable_count as u64;

        MEMORY_TOTAL.set(self.total as i64);
        MEMORY_FREE.set(bytes(free));
        MEMORY_AVAILABLE.set(bytes(free + file + purgeable));
        MEMORY_CACHED.set(bytes(file));
        MEMORY_ANON.set(bytes(vm.internal_page_count as u64));

        if let Some(swap) = swap {
            MEMORY_SWAP_TOTAL.set(swap.xsu_total as i64);
            MEMORY_SWAP_FREE.set(swap.xsu_avail as i64);
        }

        MEMORY_WIRED.set(bytes(vm.wire_count as u64));
        MEMORY_PURGEABLE.set(bytes(purgeable));
        MEMORY_SPECULATIVE.set(bytes(vm.speculative_count as u64));
        MEMORY_COMPRESSED.set(bytes(vm.compressor_page_count as u64));
        MEMORY_COMPRESSED_STORED.set(bytes(vm.total_uncompressed_pages_in_compressor));

        guard.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The snapshot routes these series to the group registered under (their
    // module's sampler, `memory_meminfo_read`), so both must be
    // `memory_meminfo`; a group whose sampler is not running is bounded to
    // zero members.
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
        assert_eq!(MEMINFO_ACQ.sampler, NAME);
    }

    #[tokio::test]
    async fn refresh_sets_the_mapped_gauges_and_stamps_the_group() {
        let sampler = init(Arc::new(toml::from_str("").unwrap()))
            .unwrap()
            .unwrap();
        sampler.refresh().await;

        assert!(MEMINFO_ACQ.window().is_some());

        let total = MEMORY_TOTAL.value();
        assert!(total > 0);
        for (name, gauge) in [
            ("free", &MEMORY_FREE),
            ("available", &MEMORY_AVAILABLE),
            ("cached", &MEMORY_CACHED),
            ("anon", &MEMORY_ANON),
            ("wired", &MEMORY_WIRED),
            ("compressed", &MEMORY_COMPRESSED),
        ] {
            let v = gauge.value();
            assert!((0..=total).contains(&v), "{name} = {v}, total = {total}");
        }
        assert!(MEMORY_FREE.value() <= MEMORY_AVAILABLE.value());

        // Active and inactive have no macOS equivalent and stay unset.
        assert!(metriken::Lazy::get(&MEMORY_ACTIVE_FILE).is_none());
        assert!(metriken::Lazy::get(&MEMORY_INACTIVE_ANON).is_none());
    }
}
