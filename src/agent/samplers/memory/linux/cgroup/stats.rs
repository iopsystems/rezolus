use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use crate::agent::MAX_CGROUPS;
use linkme::distributed_slice;

// One cgroup-major sweep, with independently fallible reads per visit (the
// device-visit archetype in principle 18). The blocking task is the sole writer.
pub static CGROUP_MEMORY_ACQ: AcquisitionGroup = AcquisitionGroup::new_cached_sparse(
    crate::agent::samplers::bpf_sampler_name("memory_cgroup"),
    "memory_cgroup_sweep",
);

#[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
static CGROUP_MEMORY_ACQ_REG: &'static AcquisitionGroup = &CGROUP_MEMORY_ACQ;

#[metric(
    name = "cgroup_memory_usage",
    description = "Memory charged to the cgroup and its descendants, excluding swap",
    metadata = { unit = "bytes", acq_group = "memory_cgroup_sweep" }
)]
pub static CGROUP_MEMORY_USAGE: GaugeGroup = GaugeGroup::new(MAX_CGROUPS);

#[metric(
    name = "cgroup_memory_swap_usage",
    description = "Swap charged to the cgroup and its descendants",
    metadata = { unit = "bytes", acq_group = "memory_cgroup_sweep" }
)]
pub static CGROUP_MEMORY_SWAP_USAGE: GaugeGroup = GaugeGroup::new(MAX_CGROUPS);

// Each kind is a column over the same cgroup slots. The long stream joins
// columns by slot, so a flattened cgroup × kind index would mix occupants.
macro_rules! memory_stats {
    ($($metric:ident => $kind:literal),+ $(,)?) => {
        #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
        pub const KINDS: &[&str] = &[$($kind),+];
        #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
        pub static CGROUP_MEMORY_STAT: &[&GaugeGroup] = &[$(&$metric),+];
        $(
            #[metric(
                name = "cgroup_memory_stat",
                description = "Memory by kind for the cgroup and its descendants; kinds overlap and must not be summed",
                metadata = { unit = "bytes", acq_group = "memory_cgroup_sweep", kind = $kind }
            )]
            pub static $metric: GaugeGroup = GaugeGroup::new(MAX_CGROUPS);
        )+
    }
}

// Only bytes: event and page counters are not a memory footprint.
memory_stats! {
    MEMORY_ANON => "anon",
    MEMORY_FILE => "file",
    MEMORY_KERNEL => "kernel",
    MEMORY_KERNEL_STACK => "kernel_stack",
    MEMORY_PAGETABLES => "pagetables",
    MEMORY_SEC_PAGETABLES => "sec_pagetables",
    MEMORY_PERCPU => "percpu",
    MEMORY_SOCK => "sock",
    MEMORY_VMALLOC => "vmalloc",
    MEMORY_SHMEM => "shmem",
    MEMORY_ZSWAP => "zswap",
    MEMORY_ZSWAPPED => "zswapped",
    MEMORY_FILE_MAPPED => "file_mapped",
    MEMORY_FILE_DIRTY => "file_dirty",
    MEMORY_FILE_WRITEBACK => "file_writeback",
    MEMORY_SWAPCACHED => "swapcached",
    MEMORY_ANON_THP => "anon_thp",
    MEMORY_FILE_THP => "file_thp",
    MEMORY_SHMEM_THP => "shmem_thp",
    MEMORY_INACTIVE_ANON => "inactive_anon",
    MEMORY_ACTIVE_ANON => "active_anon",
    MEMORY_INACTIVE_FILE => "inactive_file",
    MEMORY_ACTIVE_FILE => "active_file",
    MEMORY_UNEVICTABLE => "unevictable",
    MEMORY_SLAB_RECLAIMABLE => "slab_reclaimable",
    MEMORY_SLAB_UNRECLAIMABLE => "slab_unreclaimable",
    MEMORY_SLAB => "slab",
    MEMORY_HUGETLB => "hugetlb",
}
