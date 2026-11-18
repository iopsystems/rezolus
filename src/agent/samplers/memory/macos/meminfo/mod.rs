//! Memory gauges on macOS: capacity, the page cache and anonymous memory,
//! swap, and the compressor, all from sysctls, which are not rate-limited.
//!
//! `memory_available` has the Linux meaning: memory the kernel can hand out
//! without compressing or swapping anonymous memory. That is free pages,
//! file-backed pages and purgeable pages. `vm.page_free_count` excludes
//! speculative pages (file read-ahead), which the file-backed count includes.
//! `vm.page_pageable_internal_count` omits anonymous pages on the per-CPU
//! local queues, which `host_statistics64` adds, so `vm_stat`'s "Anonymous
//! pages" can read slightly higher.
//!
//! The compressor gauges are set only when their sysctls are readable; the
//! sampler does not depend on them.

const NAME: &str = "memory_meminfo";

use crate::agent::*;

use super::{page_size, swap_usage, sysctl_u64};

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

    let page_size = page_size().ok_or_else(|| anyhow::anyhow!("{NAME}: no vm.pagesize"))?;
    let total =
        sysctl_u64(c"hw.memsize").ok_or_else(|| anyhow::anyhow!("{NAME}: no hw.memsize"))?;

    let sampler = Meminfo { page_size, total };
    if sampler.read().is_none() {
        return Err(crate::agent::sampler_status::Unsupported(
            "the vm.page_* sysctls are not readable".to_string(),
        )
        .into());
    }

    Ok(Some(Box::new(sampler)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Meminfo {
    page_size: u64,
    total: u64,
}

/// One read of the page-queue sysctls, in pages.
struct Reading {
    free: u64,
    speculative: u64,
    file: u64,
    anon: u64,
    purgeable: u64,
}

impl Meminfo {
    fn read(&self) -> Option<Reading> {
        Some(Reading {
            free: sysctl_u64(c"vm.page_free_count")?,
            speculative: sysctl_u64(c"vm.page_speculative_count")?,
            file: sysctl_u64(c"vm.page_pageable_external_count")?,
            anon: sysctl_u64(c"vm.page_pageable_internal_count")?,
            purgeable: sysctl_u64(c"vm.page_purgeable_count")?,
        })
    }
}

#[async_trait]
impl Sampler for Meminfo {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let guard = MEMINFO_ACQ.acquire();

        let Some(r) = self.read() else {
            return;
        };
        let swap = swap_usage();

        let bytes = |pages: u64| (pages * self.page_size) as i64;

        MEMORY_TOTAL.set(self.total as i64);
        MEMORY_FREE.set(bytes(r.free));
        MEMORY_AVAILABLE.set(bytes(r.free + r.file + r.purgeable));
        MEMORY_CACHED.set(bytes(r.file));
        MEMORY_ANON.set(bytes(r.anon));

        if let Some(swap) = swap {
            MEMORY_SWAP_TOTAL.set(swap.xsu_total as i64);
            MEMORY_SWAP_FREE.set(swap.xsu_avail as i64);
        }

        MEMORY_PURGEABLE.set(bytes(r.purgeable));
        MEMORY_SPECULATIVE.set(bytes(r.speculative));
        if let Some(n) = sysctl_u64(c"vm.compressor_bytes_used") {
            MEMORY_COMPRESSED.set(n as i64);
        }
        if let Some(n) = sysctl_u64(c"vm.compressor_segment_pages_compressed") {
            MEMORY_COMPRESSED_STORED.set(bytes(n));
        }

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
