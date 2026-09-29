//! Collects ext4 allocator, inode, writeback and metadata-read stats using BPF
//! and traces:
//! * `ext4_mballoc_alloc` — every extent allocation: requested vs allocated
//!   length, groups scanned, the criterion reached
//! * `ext4_free_blocks`, `ext4_allocate_inode`, `ext4_free_inode`
//! * `ext4_writepages_result` — every writeback pass: pages written, skipped, errors
//! * `ext4_trim_extent`, `ext4_discard_preallocations`
//! * `ext4_load_inode`, `ext4_read_block_bitmap_load`, `ext4_load_inode_bitmap`
//!   — synchronous metadata reads on the calling thread
//!
//! And produces these stats:
//! * `ext4_allocation_size`, `ext4_allocations`, `ext4_allocation_blocks{kind}`,
//!   `ext4_allocation_groups_scanned`, `ext4_allocations_by_criterion{criterion}`,
//!   `ext4_freed_blocks`
//! * `ext4_inodes{op}`
//! * `ext4_writepages`, `ext4_writepages_pages{outcome}`, `ext4_writepages_errors`
//! * `ext4_trimmed_blocks`, `ext4_preallocation_discards`,
//!   `ext4_preallocation_discarded_blocks`
//! * `ext4_inode_loads`, `ext4_bitmap_loads{kind}`
//!
//! Counters are per filesystem, exactly as `ext4_journal`'s: one slot per
//! mounted ext4 filesystem labeled `mount`, `fstype`, `devnum` and
//! `block_device`, plus slot 0 `mount="other"`; the slot is a `dev_t` lookup
//! on the superblock each hook reaches (`bpf/filesystem.h`,
//! `bpf/filesystems.rs`). The allocation-size histogram stays host-wide.
//! Kernel support is the same as `ext4_journal`'s: the allocator hook reads
//! `struct ext4_allocation_context` through CO-RE, which needs the ext4 types
//! in vmlinux or module BTF.

const NAME: &str = "ext4_alloc";

mod bpf {
    include!(concat!(env!("OUT_DIR"), "/ext4_alloc.bpf.rs"));
}

mod stats;

use bpf::*;
use stats::*;

use crate::agent::*;

use std::sync::Arc;

/// Every hook: the tracepoint name, its `tp_btf` and `raw_tp` program names,
/// and the capability label `rezolus status` shows if the active twin fails
/// to attach.
const HOOKS: &[(&str, &str, &str, &str)] = &[
    (
        "ext4_mballoc_alloc",
        "ext4_mballoc_alloc_btf",
        "ext4_mballoc_alloc_raw",
        "block allocator effort",
    ),
    (
        "ext4_free_blocks",
        "ext4_free_blocks_btf",
        "ext4_free_blocks_raw",
        "blocks freed",
    ),
    (
        "ext4_allocate_inode",
        "ext4_allocate_inode_btf",
        "ext4_allocate_inode_raw",
        "inodes allocated",
    ),
    (
        "ext4_free_inode",
        "ext4_free_inode_btf",
        "ext4_free_inode_raw",
        "inodes freed",
    ),
    (
        "ext4_writepages_result",
        "ext4_writepages_result_btf",
        "ext4_writepages_result_raw",
        "writeback results",
    ),
    (
        "ext4_trim_extent",
        "ext4_trim_extent_btf",
        "ext4_trim_extent_raw",
        "discarded blocks",
    ),
    (
        "ext4_discard_preallocations",
        "ext4_discard_preallocations_btf",
        "ext4_discard_preallocations_raw",
        "preallocation discards",
    ),
    (
        "ext4_load_inode",
        "ext4_load_inode_btf",
        "ext4_load_inode_raw",
        "inode-table reads",
    ),
    (
        "ext4_read_block_bitmap_load",
        "ext4_read_block_bitmap_load_btf",
        "ext4_read_block_bitmap_load_raw",
        "block bitmap reads",
    ),
    (
        "ext4_load_inode_bitmap",
        "ext4_load_inode_bitmap_btf",
        "ext4_load_inode_bitmap_raw",
        "inode bitmap reads",
    ),
];

/// What a filesystem slot means, published when the mount table changes.
static FS_IDENTITY: crate::agent::identity::SlotIdentity =
    crate::agent::identity::SlotIdentity::new(FS_IDENTITY_GROUPS);

#[linkme::distributed_slice(crate::agent::identity::SLOT_IDENTITIES)]
static FS_IDENTITY_REG: &'static crate::agent::identity::SlotIdentity = &FS_IDENTITY;

static FS_IDENTITY_GROUPS: &[crate::agent::identity::GroupMetrics] = &[(
    &COUNTERS_ACQ,
    &[
        &EXT4_ALLOCATIONS,
        &EXT4_ALLOCATION_BLOCKS_REQUESTED,
        &EXT4_ALLOCATION_BLOCKS_ALLOCATED,
        &EXT4_ALLOCATION_GROUPS_SCANNED,
        &EXT4_ALLOCATIONS_CR0,
        &EXT4_ALLOCATIONS_CR1,
        &EXT4_ALLOCATIONS_CR2,
        &EXT4_ALLOCATIONS_CR3,
        &EXT4_ALLOCATIONS_CR4,
        &EXT4_FREED_BLOCKS,
        &EXT4_INODES_ALLOCATED,
        &EXT4_INODES_FREED,
        &EXT4_WRITEPAGES,
        &EXT4_WRITEPAGES_PAGES_WRITTEN,
        &EXT4_WRITEPAGES_PAGES_SKIPPED,
        &EXT4_WRITEPAGES_ERRORS,
        &EXT4_TRIMMED_BLOCKS,
        &EXT4_PREALLOCATION_DISCARDS,
        &EXT4_PREALLOCATION_DISCARDED_BLOCKS,
        &EXT4_INODE_LOADS,
        &EXT4_BITMAP_LOADS_BLOCK,
        &EXT4_BITMAP_LOADS_INODE,
    ],
)];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Order MUST match the C_* indices in mod.bpf.c.
    let counters = vec![
        &EXT4_ALLOCATIONS,
        &EXT4_ALLOCATION_BLOCKS_REQUESTED,
        &EXT4_ALLOCATION_BLOCKS_ALLOCATED,
        &EXT4_ALLOCATION_GROUPS_SCANNED,
        &EXT4_ALLOCATIONS_CR0,
        &EXT4_ALLOCATIONS_CR1,
        &EXT4_ALLOCATIONS_CR2,
        &EXT4_ALLOCATIONS_CR3,
        &EXT4_ALLOCATIONS_CR4,
        &EXT4_FREED_BLOCKS,
        &EXT4_INODES_ALLOCATED,
        &EXT4_INODES_FREED,
        &EXT4_WRITEPAGES,
        &EXT4_WRITEPAGES_PAGES_WRITTEN,
        &EXT4_WRITEPAGES_PAGES_SKIPPED,
        &EXT4_WRITEPAGES_ERRORS,
        &EXT4_TRIMMED_BLOCKS,
        &EXT4_PREALLOCATION_DISCARDS,
        &EXT4_PREALLOCATION_DISCARDED_BLOCKS,
        &EXT4_INODE_LOADS,
        &EXT4_BITMAP_LOADS_BLOCK,
        &EXT4_BITMAP_LOADS_INODE,
    ];

    // Per hook, keep the tp_btf twin only when the kernel's BTF (vmlinux or a
    // module's) carries that tracepoint; otherwise the raw_tp twin. See
    // `kernel_btf_has_tracepoints`.
    let mut disabled: Vec<&'static str> = Vec::with_capacity(HOOKS.len());
    let mut required: Vec<(&'static str, &'static str)> = Vec::with_capacity(HOOKS.len());

    for (tracepoint, btf, raw, label) in HOOKS {
        if kernel_btf_has_tracepoints(&[tracepoint]) {
            disabled.push(raw);
            required.push((btf, label));
        } else {
            disabled.push(btf);
            required.push((raw, label));
        }
    }

    let bpf = BpfBuilder::new(
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
    .histogram(
        "allocation_size",
        &EXT4_ALLOCATION_SIZE,
        &ALLOCATION_SIZES_ACQ,
    )
    .disabled_programs(&disabled)
    .required_programs(&required)
    .build()?;

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
            "allocation_size" => &self.maps.allocation_size,
            _ => unimplemented!(),
        }
    }
}

impl OpenSkelExt for ModSkel<'_> {
    fn log_prog_instructions(&self) {
        debug!(
            "{NAME} ext4_mballoc_alloc_btf() BPF instruction count: {}",
            self.progs.ext4_mballoc_alloc_btf.insn_cnt()
        );
        debug!(
            "{NAME} ext4_writepages_result_btf() BPF instruction count: {}",
            self.progs.ext4_writepages_result_btf.insn_cnt()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every hook names two distinct programs and no program is named twice:
    /// `disabled_programs` refuses a name not in the skeleton, and a
    /// duplicate would leave a twin autoloaded and double-count.
    #[test]
    fn hook_program_names_are_distinct() {
        let mut names: Vec<&str> = HOOKS
            .iter()
            .flat_map(|(_, btf, raw, _)| [*btf, *raw])
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
        assert_eq!(total, 2 * HOOKS.len());
    }
}
