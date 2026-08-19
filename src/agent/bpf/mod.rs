mod builder;
mod counters;
pub mod drivers;
mod histogram;
mod sync_primitive;

pub use builder::Builder as BpfBuilder;
pub use builder::{BpfProgStats, PerfEvent};

use std::path::Path;
use std::sync::OnceLock;

use crate::agent::samplers::Sampler;
use crate::agent::GroupMetadata;
use crate::*;

/// Returns true if the running kernel exposes its own BTF
/// (`/sys/kernel/btf/vmlinux`). Programs that need in-kernel BTF — `tp_btf`,
/// `fentry`/`fexit`, and the `bpf_get_current_task_btf` helper — can only be
/// attached when this is true. Checked once and cached.
pub fn kernel_has_btf() -> bool {
    static HAS_BTF: OnceLock<bool> = OnceLock::new();
    *HAS_BTF.get_or_init(|| Path::new("/sys/kernel/btf/vmlinux").exists())
}

pub trait OpenSkelExt {
    /// When called, the SkelBuilder should log instruction counts for each of
    /// the programs within the skeleton. Log level should be debug.
    fn log_prog_instructions(&self);
}

pub trait SkelExt {
    fn map(&self, name: &str) -> &libbpf_rs::Map<'_>;
}

pub trait CgroupInfo {
    fn id(&self) -> i32;
    fn level(&self) -> i32;
    fn name(&self) -> &[u8];
    fn pname(&self) -> &[u8];
    fn gpname(&self) -> &[u8];
}

#[macro_export]
macro_rules! impl_cgroup_info {
    ($type:ty) => {
        impl $crate::agent::bpf::CgroupInfo for $type {
            fn id(&self) -> i32 {
                self.id
            }

            fn level(&self) -> i32 {
                self.level
            }

            fn name(&self) -> &[u8] {
                &self.name
            }

            fn pname(&self) -> &[u8] {
                &self.pname
            }

            fn gpname(&self) -> &[u8] {
                &self.gpname
            }
        }
    };
}

const CACHELINE_SIZE: usize = 64;
const PAGE_SIZE: usize = 4096;

const COUNTER_SIZE: usize = std::mem::size_of::<u64>();
const COUNTERS_PER_CACHELINE: usize = CACHELINE_SIZE / COUNTER_SIZE;

fn whole_cachelines<T>(count: usize) -> usize {
    (count * std::mem::size_of::<T>()).div_ceil(CACHELINE_SIZE)
}

fn whole_pages<T>(count: usize) -> usize {
    (count * std::mem::size_of::<T>()).div_ceil(PAGE_SIZE)
}

use counters::{Counters, CpuCounters, PackedCounters};
use histogram::{Histogram, HistogramBatch};
pub use sync_primitive::SyncPrimitive;

/// Parse the CPU count implied by `/sys/devices/system/cpu/possible`
/// syntax: a comma-separated list of individual ids and/or `lo-hi` ranges
/// (e.g. `"0-31"`, `"0"`, `"0-3,8-11"`). The file lists which ids the kernel
/// considers *possible* (a CPU that could be hot-added), not which are
/// currently online, so the answer is `max_id + 1` — the number of possible
/// slots — not a count of the listed ids, which may have gaps. Returns
/// `None` if the content is empty, doesn't parse as expected, or the
/// implied count overflows `usize` (`checked_add`, not `+1`: a garbage
/// mask like `"0-18446744073709551615"` must fall back, not wrap to 0 in
/// release), so the caller can fall back rather than trust a bogus bound.
///
/// Deliberately NOT clamped to `MAX_CPUS` here — that is a separate
/// concern ([`clamp_possible_cpus`]) so this function stays a pure parse of
/// what the file says, testable against arbitrarily large masks.
fn parse_possible_cpus(contents: &str) -> Option<usize> {
    let mut max_id: Option<usize> = None;

    for part in contents.trim().split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        let hi = match part.split_once('-') {
            Some((_, hi)) => hi.parse::<usize>().ok()?,
            None => part.parse::<usize>().ok()?,
        };

        max_id = Some(max_id.map_or(hi, |m| m.max(hi)));
    }

    max_id.and_then(|m| m.checked_add(1))
}

/// Clamp a parsed possible-CPU count to [`crate::agent::MAX_CPUS`].
///
/// Every per-CPU BPF counter map is sized for exactly `MAX_CPUS` slots —
/// `CounterMap::new`'s mmap region is `bank_cachelines * CACHELINE_SIZE *
/// MAX_CPUS` bytes, and every sampler's `mod.bpf.c` sizes `max_entries` the
/// same way — so a sweep bound above `MAX_CPUS` indexes
/// `counters[idx + cpu * bank_width]` off the end of the mapped slice. A
/// host whose `/sys/devices/system/cpu/possible` mask exceeds `MAX_CPUS`
/// (large `CONFIG_NR_CPUS`, hypervisor firmware reporting a big possible
/// range) is real, not hypothetical, so every migrated sampler's `refresh()`
/// would panic every tick without this clamp. This is the documented
/// silent-undercount tradeoff, not a crash: `docs/principles.md` principle
/// 6 already accepts "over-allocates on small machines, silently
/// under-counts past 1024 CPUs" as `MAX_CPUS`'s known ceiling; clamping
/// here is what actually delivers that promise for a possible-CPU sweep
/// bound instead of a fixed one.
fn clamp_possible_cpus(n: usize) -> usize {
    n.min(crate::agent::MAX_CPUS)
}

/// Number of possible CPUs on this host, per
/// `/sys/devices/system/cpu/possible` — POSSIBLE, deliberately not ONLINE,
/// so a CPU that comes up mid-recording was already counted and a sweep
/// bound taken at agent start does not miss it. Parsed once and cached,
/// then clamped to [`crate::agent::MAX_CPUS`] (see [`clamp_possible_cpus`])
/// so no caller can forget the clamp and index a per-CPU BPF map's mmap
/// region out of bounds. Falls back to `MAX_CPUS` if the file is missing,
/// empty, or fails to parse, so a sweep bound here degrades to the old
/// fixed bound rather than to zero.
pub(crate) fn possible_cpus() -> usize {
    static CACHE: OnceLock<usize> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let contents = match std::fs::read_to_string("/sys/devices/system/cpu/possible") {
            Ok(contents) => contents,
            Err(e) => {
                debug!(
                    "failed to read /sys/devices/system/cpu/possible ({e}); \
                     falling back to MAX_CPUS={}",
                    crate::agent::MAX_CPUS
                );
                return crate::agent::MAX_CPUS;
            }
        };

        let Some(n) = parse_possible_cpus(&contents) else {
            warn!(
                "failed to parse /sys/devices/system/cpu/possible ({contents:?}); \
                 falling back to MAX_CPUS={}",
                crate::agent::MAX_CPUS
            );
            return crate::agent::MAX_CPUS;
        };

        let clamped = clamp_possible_cpus(n);
        if clamped != n {
            warn!(
                "possible CPU count {n} exceeds MAX_CPUS={}; per-CPU BPF counter maps are \
                 sized for MAX_CPUS, so clamping the sweep bound — CPUs beyond MAX_CPUS are \
                 silently excluded from per-CPU sweeps (docs/principles.md principle 6)",
                crate::agent::MAX_CPUS
            );
        }
        clamped
    })
}

pub fn process_cgroup_info<T>(data: &[u8], metrics: &[&dyn GroupMetadata]) -> i32
where
    T: CgroupInfo + plain::Plain + Default,
{
    let mut cgroup_info = T::default();

    if plain::copy_from_bytes(&mut cgroup_info, data).is_ok() {
        let name = std::str::from_utf8(cgroup_info.name())
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let pname = std::str::from_utf8(cgroup_info.pname())
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let gpname = std::str::from_utf8(cgroup_info.gpname())
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        // Construct hierarchical path based on level and available parent names
        let path = if name == "/" {
            // Root cgroup - just use "/"
            "/".to_string()
        } else if !gpname.is_empty() {
            if cgroup_info.level() > 3 {
                format!(".../{gpname}/{pname}/{name}")
            } else {
                format!("/{gpname}/{pname}/{name}")
            }
        } else if !pname.is_empty() {
            format!("/{pname}/{name}")
        } else if !name.is_empty() {
            format!("/{name}")
        } else {
            "".to_string()
        };

        // Update metadata for all provided metrics
        if !path.is_empty() {
            let id = cgroup_info.id() as usize;
            for metric in metrics {
                metric.insert_metadata(id, "name".to_string(), path.clone());
            }
        }
    }

    0
}

pub struct AsyncBpf {
    name: &'static str,
    thread: std::thread::JoinHandle<Result<(), libbpf_rs::Error>>,
    sync: SyncPrimitive,
    perf_threads: Vec<std::thread::JoinHandle<()>>,
    perf_sync: Vec<SyncPrimitive>,
}

#[async_trait]
impl Sampler for AsyncBpf {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn refresh(&self) {
        if self.thread.is_finished() {
            panic!("{} bpf thread exited early", self.name);
        }

        self.sync.trigger();

        self.sync.wait_notify().await;

        for thread in self.perf_threads.iter() {
            if thread.is_finished() {
                panic!("{} perf thread exited early", self.name);
            }
        }

        let perf_futures: Vec<_> = self
            .perf_sync
            .iter()
            .map(|s| {
                s.trigger();
                s.wait_notify()
            })
            .collect();

        futures::future::join_all(perf_futures.into_iter()).await;
    }
}

#[cfg(test)]
mod possible_cpus_tests {
    use super::{clamp_possible_cpus, parse_possible_cpus};

    #[test]
    fn parses_a_single_range() {
        assert_eq!(parse_possible_cpus("0-31"), Some(32));
    }

    #[test]
    fn parses_a_single_cpu() {
        assert_eq!(parse_possible_cpus("0"), Some(1));
    }

    #[test]
    fn parses_a_trailing_newline() {
        assert_eq!(parse_possible_cpus("0-31\n"), Some(32));
    }

    #[test]
    fn parses_a_list_of_ranges_and_singletons() {
        assert_eq!(parse_possible_cpus("0-3,8-11"), Some(12));
    }

    #[test]
    fn takes_the_max_id_across_unordered_parts() {
        // Not realistic content for this file, but the parser should not
        // assume the list arrives sorted.
        assert_eq!(parse_possible_cpus("8-11,0-3"), Some(12));
    }

    #[test]
    fn empty_content_is_unparseable() {
        assert_eq!(parse_possible_cpus(""), None);
        assert_eq!(parse_possible_cpus("\n"), None);
    }

    #[test]
    fn garbage_content_is_unparseable() {
        assert_eq!(parse_possible_cpus("not-a-cpu-list"), None);
    }

    #[test]
    fn parser_does_not_clamp_a_mask_beyond_max_cpus() {
        // The parser is NOT the bound: a possible mask bigger than
        // MAX_CPUS (1024) parses to its literal value. Clamping is
        // `clamp_possible_cpus`'s job, exercised separately below.
        assert_eq!(parse_possible_cpus("0-8191"), Some(8192));
    }

    #[test]
    fn overflowing_max_id_falls_back_to_unparseable_rather_than_wrapping() {
        // usize::MAX as the high end of a range: max_id + 1 must not wrap
        // to 0 (which would silently produce a zero-CPU sweep in release,
        // where integer overflow does not panic).
        assert_eq!(parse_possible_cpus("0-18446744073709551615"), None);
    }

    #[test]
    fn clamp_is_a_no_op_at_or_under_max_cpus() {
        assert_eq!(clamp_possible_cpus(1), 1);
        assert_eq!(clamp_possible_cpus(32), 32);
        assert_eq!(
            clamp_possible_cpus(crate::agent::MAX_CPUS),
            crate::agent::MAX_CPUS
        );
    }

    #[test]
    fn clamp_bounds_a_mask_beyond_max_cpus() {
        assert_eq!(clamp_possible_cpus(8192), crate::agent::MAX_CPUS);
        assert_eq!(clamp_possible_cpus(usize::MAX), crate::agent::MAX_CPUS);
    }
}
