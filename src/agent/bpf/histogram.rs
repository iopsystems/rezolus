use super::*;
use crate::*;

use metriken::RwLockHistogram;

use crate::agent::timing::AcquisitionGroup;

use std::os::fd::{AsFd, AsRawFd, FromRawFd};

/// Represents a histogram in a BPF map. The distribution must be created
/// with:
///
/// ```c
/// struct {
///     __uint(type, BPF_MAP_TYPE_ARRAY);
///     __uint(map_flags, BPF_F_MMAPABLE);
///     __type(key, u32);
///     __type(value, u64);
///     __uint(max_entries, HISTOGRAM_BUCKETS * HISTOGRAM_SLICES);
/// } some_distribution_name SEC(".maps");
/// ```
///
/// and written with `histogram_incr` or `histogram_index` from `helpers.h`,
/// which put each CPU's buckets in its own slice. The builder sets the map to
/// one slice per possible CPU before load, and `refresh()` sums the slices.
/// At grouping power 3 a slice is 496 buckets, 3968 bytes, so a histogram
/// takes 744 KiB at 192 possible CPUs. The kernel allocates every slice when
/// the map is created, and the agent mmaps and reads the whole map, so the
/// same pages count in its RSS.
///
/// The distribution should be given some meaningful name in the BPF program.
///
/// # Windowing
///
/// A `Histogram` does NOT bracket its own read. Acquisition is owned by the
/// [`HistogramBatch`] it belongs to: several histograms that are LIKE
/// ENTITIES — instances of one metric family distinguished by a label
/// (e.g. syscall_latency's 16 `op`-labeled latency histograms) — are read
/// together under ONE acquisition, not one each. See `HistogramBatch`'s
/// doc comment for the granularity rule and why, and the `# Granularity
/// rule` section on
/// [`crate::agent::samplers::ACQUISITION_GROUPS`]. `refresh()` here is just
/// the mmap read + the plain, windowless [`RwLockHistogram::update_from`]
/// — no group, no window, no acquire/finish. That used to be
/// `RwLockHistogram::set_with_window`, stamping a window on every single
/// histogram; the window is now stamped once, by the owning batch, after
/// every histogram in it has refreshed.
pub struct Histogram<'a> {
    _map: &'a libbpf_rs::Map<'a>,
    mmap: memmap2::MmapMut,
    slices: usize,
    sum: Vec<u64>,
    histogram: &'static RwLockHistogram,
}

impl<'a> Histogram<'a> {
    pub fn new(map: &'a libbpf_rs::Map, histogram: &'static RwLockHistogram) -> Self {
        let buckets = histogram.config().total_buckets();
        let slices = possible_cpus();

        let mmap_len = whole_pages::<u64>(buckets * slices) * PAGE_SIZE;

        let fd = map.as_fd().as_raw_fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mmap = unsafe {
            memmap2::MmapOptions::new()
                .len(mmap_len)
                .map_mut(&file)
                .expect("failed to mmap() bpf distribution")
        };

        // check the alignment
        let (_prefix, data, _suffix) = unsafe { mmap.align_to::<u64>() };
        let expected_len = mmap_len / std::mem::size_of::<u64>();

        if data.len() != expected_len {
            error!("mmap region not aligned or width doesn't match");
            panic!();
        }

        Self {
            _map: map,
            mmap,
            slices,
            sum: vec![0; buckets],
            histogram,
        }
    }

    /// Read the BPF map and update the histogram's bucket data. Windowless
    /// on purpose — see the `# Windowing` section on the type doc comment:
    /// the owning [`HistogramBatch`] is what stamps a window, once, around
    /// every histogram in the batch's `refresh()`.
    pub fn refresh(&mut self) {
        let (_prefix, mapped, _suffix) = unsafe { self.mmap.align_to::<u64>() };
        read_slices(mapped, self.slices, &mut self.sum);
        let _ = self.histogram.update_from(&self.sum);
    }
}

/// A set of histograms that share ONE [`AcquisitionGroup`] — one read
/// section, stamped once, over all of them.
///
/// # Granularity rule
///
/// A group is one read section over LIKE ENTITIES: instances of a single
/// metric family, distinguished from one another by a label (syscall
/// class, block IO op, TCP direction...), share the sweep that reads them
/// and so share one group — not one group per instance. Before this, every
/// `Histogram` bracketed its own read, which reproduced the exact problem
/// the acquisition-groups design set out to fix for exactly this shape of
/// sampler: syscall_latency's 16 op-class histograms turned into 16
/// one-member "groups" (16 acquisitions per tick) instead of the single
/// sweep the design calls for. See
/// `docs/journal/2026-08-17-window-sidecar-cost.md`'s addendum (which
/// names syscall_latency, alongside drivehealth, as the collapse-to-one-
/// group case) and the `# Granularity rule` section on
/// [`crate::agent::samplers::ACQUISITION_GROUPS`].
///
/// DIFFERENT metric families stay on separate groups even when their BPF
/// programs read back-to-back in the same sampler's refresh — e.g.
/// tcp_receive's `srtt` and `jitter` are two distinct measurements, not
/// label-instances of one family, so each keeps its own single-member
/// batch; scheduler_runqueue's `runqlat`/`running`/`offcpu` are three
/// distinct families for the same reason. A single-histogram sampler (e.g.
/// tcp_packet_latency) is simply a batch of one.
///
/// # Windowing
///
/// `refresh()` brackets the WHOLE batch's sweep with one
/// [`AcquisitionGroup`] acquisition: `acquire()` before any member
/// refreshes, `finish()` after every member has (stamp-last — see
/// [`AcquisitionGroup::acquire`]). Each member `Histogram::refresh()` sets
/// its own bucket data plain (windowless); the single window this stamps
/// covers every histogram in the batch's read, not just the last one —
/// restamping the group once per histogram (rather than once per batch)
/// would leave only the LAST member's individual read timing as the
/// group's window, silently discarding the rest. The bracket is
/// correspondingly wider than any one member's actual read — the same
/// honest-upper-bound tradeoff as `counters.rs`'s `Counters`/`CpuCounters`.
///
/// # Single-writer contract
///
/// One batch owns its group exclusively — the same single-writer contract
/// [`AcquisitionGroup::acquire`] documents applies to the whole batch, not
/// per-member: two different `HistogramBatch`es must never be constructed
/// for the same group. `builder.rs`'s `Builder::histogram` registration
/// only ever produces one batch per distinct group reference, by
/// construction (grouped by pointer identity at `build()` time) — see its
/// doc comment.
pub struct HistogramBatch<'a> {
    group: &'static AcquisitionGroup,
    histograms: Vec<Histogram<'a>>,
}

impl<'a> HistogramBatch<'a> {
    /// Construct a batch directly from its already-assembled members.
    /// Grouping a flat `.histogram()` registration list by group pointer
    /// identity (which member goes in which batch) is `builder.rs`'s
    /// `batch_by_group`'s job, not this type's — see its doc comment and
    /// tests for that pure grouping logic.
    pub fn new(group: &'static AcquisitionGroup, histograms: Vec<Histogram<'a>>) -> Self {
        Self { group, histograms }
    }

    pub fn refresh(&mut self) {
        // Bracket the whole batch's sweep as one acquisition; see the
        // `# Windowing` section above for why this must be per-batch, not
        // per-histogram.
        let acq = self.group.acquire();

        for histogram in self.histograms.iter_mut() {
            histogram.refresh();
        }

        acq.finish();
    }
}

/// The number of slices a histogram map is declared with
/// (`HISTOGRAM_SLICES` in helpers.h, the agent's `MAX_CPUS`).
pub const HISTOGRAM_SLICES: usize = 1024;

/// Sum the first `slices` per-CPU slices of `sum.len()` buckets each from a
/// mapped histogram (which may carry page padding after them) into `sum`.
fn read_slices(mapped: &[u64], slices: usize, sum: &mut [u64]) {
    let n = sum.len();
    sum.fill(0);
    for slice in mapped[0..n * slices].chunks_exact(n) {
        for (total, value) in sum.iter_mut().zip(slice) {
            *total = total.wrapping_add(*value);
        }
    }
}

#[cfg(test)]
mod read_slices_tests {
    use super::{read_slices, HISTOGRAM_SLICES};

    /// A map of three 496-bucket slices followed by a fourth slice's worth
    /// of padding, with a distinct count in the same bucket of each slice:
    /// the sum has all three, and the padding is not read.
    #[test]
    fn sums_every_slice_and_ignores_padding() {
        let buckets = 496;
        let mut mapped = vec![0u64; buckets * 4];
        mapped[100] = 1;
        mapped[buckets + 100] = 10;
        mapped[2 * buckets + 100] = 100;
        mapped[2 * buckets + 495] = 7;
        mapped[3 * buckets] = 999;
        let mut sum = vec![5u64; buckets];
        read_slices(&mapped, 3, &mut sum);
        assert_eq!(sum[100], 111);
        assert_eq!(sum[495], 7);
        assert_eq!(sum.iter().sum::<u64>(), 118);
    }

    /// The agent's histograms have the bucket count BPF strides by,
    /// `HISTOGRAM_SLICE_BUCKETS(3)` = (65 - 3) << 3 = 496, and the slice
    /// count matches MAX_CPUS.
    #[test]
    fn slice_width_matches_bpf() {
        let config =
            histogram::Config::new(crate::common::HISTOGRAM_GROUPING_POWER, 64).expect("config");
        assert_eq!(config.total_buckets(), 496);
        assert_eq!(HISTOGRAM_SLICES, crate::agent::MAX_CPUS);
    }

    /// `helpers.h` declares the same slice count and slice width as the
    /// Rust side reads with.
    #[test]
    fn bpf_helper_has_the_same_slices() {
        let helpers = include_str!("helpers.h");
        assert!(helpers.contains(&format!("#define HISTOGRAM_SLICES {HISTOGRAM_SLICES}\n")));
        assert!(helpers.contains(
            "#define HISTOGRAM_SLICE_BUCKETS(grouping_power) \
             ((65 - (grouping_power)) << (grouping_power))\n"
        ));
    }
}
