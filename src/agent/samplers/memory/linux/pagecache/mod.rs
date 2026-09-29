//! Counts the page cache's traffic using BPF and traces:
//! * `fentry` on `filemap_read` (`generic_file_buffered_read` before 5.12) —
//!   buffered read calls and the bytes they ask for
//! * `mm_filemap_add_to_page_cache` — pages filled, by what the filling task
//!   was doing: a read syscall, a write syscall, a page fault, or other
//! * `mm_filemap_delete_from_page_cache` — pages evicted
//! * `fentry` on `filemap_fault` — mmap faults served by the cache
//!
//! And produces these stats:
//! * `pagecache_reads`, `pagecache_read_bytes`, `pagecache_pages_added{reason}`,
//!   `pagecache_pages_evicted`, `pagecache_faults` — per filesystem (`mount`,
//!   `fstype`, `devnum`, `block_device`, plus `mount="other"` for filesystems
//!   the slot registry does not track and for block devices' own cache)
//! * `cgroup_pagecache_reads`, `cgroup_pagecache_read_bytes`,
//!   `cgroup_pagecache_pages_added` — per cgroup of the task, only with
//!   `cgroup_attribution = true`
//!
//! `pagecache_pages_added{reason="read"} × 4096 / pagecache_read_bytes` is the
//! page-level miss ratio of reads, readahead included; `1 −` that is the hit
//! ratio. There is no per-call hit/miss and no latency: the design that had
//! them cost an `fexit` and task storage on every read, and this one costs one
//! `fentry` (`docs/journal/2026-09-29-pagecache-hit-ratio.md`).
//!
//! **Opt-in**: the read hook runs once per read call, on the request path. It
//! is in the config's `OPT_IN_SAMPLERS`, so `[defaults]` never enables it.
//!
//! Kernel support: the tracepoints and `filemap_fault` are old; `filemap_read`
//! is 5.12+ and `generic_file_buffered_read` serves before it, selected from
//! BTF; a kernel with neither reports the sampler unsupported. Classifying a
//! fill needs `bpf_task_pt_regs` (5.15+): before it the plain twin loads and
//! every fill is `reason="other"`. Folio sizes come from the folio order,
//! wherever the running kernel keeps it (`_flags_1` from 6.6,
//! `_folio_order` on 6.1–6.5, the second page's `compound_order` on
//! 5.16–6.0), so a large folio counts as its pages on every kernel with
//! folios. Bytes read are clamped at end of file, so a short read of a small
//! file counts the bytes it could return, not the buffer it passed.

const NAME: &str = "memory_pagecache";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/memory_pagecache.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

unsafe impl plain::Plain for bpf::types::cgroup_info {}
impl_cgroup_info!(bpf::types::cgroup_info);

/// The tracepoints the fill and eviction programs attach to. Both or nothing:
/// a `tp_btf` target missing from BTF fails the whole skeleton at load.
const TRACEPOINTS: &[&str] = &[
    "mm_filemap_add_to_page_cache",
    "mm_filemap_delete_from_page_cache",
];

/// What `syscall_lut` maps a syscall number to: the classes the BPF side
/// tests for. Everything else is 0.
const LUT_READ: u64 = 1;
const LUT_WRITE: u64 = 2;

/// Entries in `syscall_lut`; must match `MAX_SYSCALL_ID` in mod.bpf.c.
const MAX_SYSCALL_ID: usize = 1024;

/// What a filesystem slot means, published when the mount table changes.
static FS_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(FS_IDENTITY_GROUPS);

static FS_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[
    &PAGECACHE_READS,
    &PAGECACHE_READ_BYTES,
    &PAGECACHE_PAGES_ADDED_READ,
    &PAGECACHE_PAGES_ADDED_WRITE,
    &PAGECACHE_PAGES_ADDED_FAULT,
    &PAGECACHE_PAGES_ADDED_OTHER,
    &PAGECACHE_PAGES_EVICTED,
    &PAGECACHE_FAULTS,
]];

/// What a cgroup slot means, published as the BPF side discovers cgroups.
static CGROUP_IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(CGROUP_IDENTITY_GROUPS);

static CGROUP_IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[
    &CGROUP_PAGECACHE_READS,
    &CGROUP_PAGECACHE_READ_BYTES,
    &CGROUP_PAGECACHE_PAGES_ADDED,
]];

fn handle_cgroup_info(data: &[u8]) -> i32 {
    process_cgroup_info::<bpf::types::cgroup_info>(data, &CGROUP_IDENTITY)
}

/// Syscall number → class for the running architecture: the read and write
/// families that add pages to the cache on their own behalf. `sendfile`,
/// `splice` and `copy_file_range` read through the cache too but are left as
/// `other`: they are not what a hit ratio is asked about.
fn syscall_lut() -> Vec<u64> {
    (0..MAX_SYSCALL_ID)
        .map(
            |id| match syscall_numbers::native::sys_call_name(id as i64) {
                Some("read" | "pread64" | "readv" | "preadv" | "preadv2") => LUT_READ,
                Some("write" | "pwrite64" | "writev" | "pwritev" | "pwritev2") => LUT_WRITE,
                _ => 0,
            },
        )
        .collect()
}

/// Which programs to load, from the kernel's BTF and helpers: the names to
/// disable and the (program, capability) pairs required.
fn select_programs(
    has_filemap_read: bool,
    has_buffered_read: bool,
    has_task_pt_regs: bool,
) -> (Vec<&'static str>, Vec<(&'static str, &'static str)>) {
    let mut disabled = Vec::new();
    let mut required = vec![
        ("filemap_delete", "pages evicted"),
        ("filemap_fault_fentry", "mmap faults"),
    ];

    // One read entry point per kernel: filemap_read from 5.12,
    // generic_file_buffered_read before. Prefer the newer name if BTF had both.
    if has_filemap_read {
        required.push(("filemap_read_fentry", "read calls and bytes"));
        disabled.push("generic_file_buffered_read_fentry");
    } else if has_buffered_read {
        required.push(("generic_file_buffered_read_fentry", "read calls and bytes"));
        disabled.push("filemap_read_fentry");
    } else {
        disabled.push("filemap_read_fentry");
        disabled.push("generic_file_buffered_read_fentry");
    }

    // The classified fill program calls bpf_task_pt_regs (5.15); the plain
    // twin counts every fill as `other`.
    if has_task_pt_regs {
        required.push(("filemap_add_classified", "pages filled by reason"));
        disabled.push("filemap_add_plain");
    } else {
        required.push(("filemap_add_plain", "pages filled"));
        disabled.push("filemap_add_classified");
    }

    (disabled, required)
}

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    if !kernel_btf_has_tracepoints(TRACEPOINTS) {
        return Err(crate::agent::sampler_status::Unsupported(
            "the kernel's BTF does not describe the page-cache add and delete tracepoints"
                .to_string(),
        )
        .into());
    }

    let has_filemap_read = kernel_btf_has_funcs(&["filemap_read"]);
    let has_buffered_read =
        !has_filemap_read && kernel_btf_has_funcs(&["generic_file_buffered_read"]);
    if !has_filemap_read && !has_buffered_read {
        return Err(crate::agent::sampler_status::Unsupported(
            "the kernel's BTF has neither filemap_read nor generic_file_buffered_read to attach \
             the read hook to"
                .to_string(),
        )
        .into());
    }

    let has_task_pt_regs = kernel_tracing_has_task_pt_regs();
    if !has_task_pt_regs {
        info!(
            "{NAME}: tracing programs cannot call bpf_task_pt_regs on this kernel (needs 5.15+); \
             every fill is reason=\"other\""
        );
    }

    let (disabled, required) =
        select_programs(has_filemap_read, has_buffered_read, has_task_pt_regs);
    let cgroup_attribution = config.cgroup_attribution(NAME);

    // Order MUST match the C_* indices in mod.bpf.c: reads, bytes, added by
    // reason (read, write, fault, other), evicted, faults.
    let counters = vec![
        &PAGECACHE_READS,
        &PAGECACHE_READ_BYTES,
        &PAGECACHE_PAGES_ADDED_READ,
        &PAGECACHE_PAGES_ADDED_WRITE,
        &PAGECACHE_PAGES_ADDED_FAULT,
        &PAGECACHE_PAGES_ADDED_OTHER,
        &PAGECACHE_PAGES_EVICTED,
        &PAGECACHE_FAULTS,
    ];

    let mut builder = BpfBuilder::new(
        &config,
        NAME,
        BpfProgStats {
            run_time: &BPF_RUN_TIME,
            run_count: &BPF_RUN_COUNT,
        },
        ModSkelBuilder::default,
    )
    .filesystem_counters(
        "counters",
        "fs_slots",
        counters,
        &COUNTERS_ACQ,
        &FS_IDENTITY,
    )
    .map("syscall_lut", syscall_lut())
    .disabled_programs(&disabled)
    .required_programs(&required)
    // The switch is read-only data the verifier folds at load; the cgroup
    // maps and their series exist only when it is on.
    .pre_load(move |open| {
        open.maps
            .rodata_data
            .as_mut()
            .expect("the program declares read-only data")
            .cgroup_attribution = cgroup_attribution as u8;
    });

    if cgroup_attribution {
        builder = builder
            .packed_counters("cgroup_reads", &CGROUP_PAGECACHE_READS, &CGROUP_ACQ)
            .packed_counters(
                "cgroup_read_bytes",
                &CGROUP_PAGECACHE_READ_BYTES,
                &CGROUP_ACQ,
            )
            .packed_counters(
                "cgroup_pages_added",
                &CGROUP_PAGECACHE_PAGES_ADDED,
                &CGROUP_ACQ,
            )
            .ringbuf_handler("cgroup_info", handle_cgroup_info);
    }

    let bpf = builder.build()?;

    Ok(Some(Box::new(bpf)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

impl SkelExt for ModSkel<'_> {
    fn map(&self, name: &str) -> &libbpf_rs::Map<'_> {
        match name {
            "counters" => &self.maps.counters,
            "fs_slots" => &self.maps.fs_slots,
            "syscall_lut" => &self.maps.syscall_lut,
            "cgroup_info" => &self.maps.cgroup_info,
            "cgroup_reads" => &self.maps.cgroup_reads,
            "cgroup_read_bytes" => &self.maps.cgroup_read_bytes,
            "cgroup_pages_added" => &self.maps.cgroup_pages_added,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} filemap_read_fentry() BPF instruction count: {}",
            self.progs.filemap_read_fentry.insn_cnt()
        );
        debug!(
            "{NAME} filemap_add_classified() BPF instruction count: {}",
            self.progs.filemap_add_classified.insn_cnt()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The read and write families the lookup table classifies, on this
    /// architecture's numbering; everything else is 0.
    #[test]
    fn the_syscall_lut_classifies_the_read_and_write_families() {
        let lut = syscall_lut();
        assert_eq!(lut.len(), MAX_SYSCALL_ID);
        let nr = |name: &str| {
            (0..lut.len())
                .find(|&id| syscall_numbers::native::sys_call_name(id as i64) == Some(name))
                .unwrap_or_else(|| panic!("no {name} on this architecture"))
        };
        assert_eq!(lut[nr("read")], LUT_READ);
        assert_eq!(lut[nr("pread64")], LUT_READ);
        assert_eq!(lut[nr("write")], LUT_WRITE);
        assert_eq!(lut[nr("pwritev2")], LUT_WRITE);
        assert_eq!(lut[nr("openat")], 0);
        assert_eq!(lut[nr("mmap")], 0);
    }

    /// Exactly one read entry point and one fill program are loaded, chosen
    /// by what the kernel has; the eviction and fault programs always are.
    #[test]
    fn programs_are_selected_by_kernel_capability() {
        let (disabled, required) = select_programs(true, false, true);
        assert!(required.iter().any(|(p, _)| *p == "filemap_read_fentry"));
        assert!(required.iter().any(|(p, _)| *p == "filemap_add_classified"));
        assert!(disabled.contains(&"generic_file_buffered_read_fentry"));
        assert!(disabled.contains(&"filemap_add_plain"));

        let (disabled, required) = select_programs(false, true, false);
        assert!(required
            .iter()
            .any(|(p, _)| *p == "generic_file_buffered_read_fentry"));
        assert!(required.iter().any(|(p, _)| *p == "filemap_add_plain"));
        assert!(disabled.contains(&"filemap_read_fentry"));
        assert!(disabled.contains(&"filemap_add_classified"));

        for (a, b, c) in [
            (true, false, true),
            (false, true, false),
            (false, false, true),
        ] {
            let (disabled, required) = select_programs(a, b, c);
            for (prog, _) in &required {
                assert!(
                    !disabled.contains(prog),
                    "{prog} both disabled and required"
                );
            }
            assert!(required.iter().any(|(p, _)| *p == "filemap_delete"));
            assert!(required.iter().any(|(p, _)| *p == "filemap_fault_fentry"));
        }
    }
}
