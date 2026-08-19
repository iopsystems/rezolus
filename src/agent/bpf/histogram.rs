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
///     __uint(max_entries, 976);
/// } some_distribution_name SEC(".maps");
/// ```
///
/// This histogram must also be indexed into using the `value_to_index`
/// helper from `histogram.h`. This results in a histogram that uses 64bit
/// counters and covers the entire range of u64 values. This histogram occupies
/// 60KB in kernel space and an additional 60KB in user space.
///
/// The distribution should be given some meaningful name in the BPF program.
///
/// # Windowing
///
/// The mmap read is bracketed by one [`AcquisitionGroup`] acquisition:
/// `acquire()` before the read, `finish()` after the bucket data has been
/// written into the histogram (stamp-last — see
/// [`AcquisitionGroup::acquire`]). The bucket data itself is written with
/// the plain, windowless [`RwLockHistogram::update_from`]; the group's
/// acquisition window is the ONLY place this read's timing is recorded now
/// — there is no more per-metric window stamped alongside the buckets (that
/// was `RwLockHistogram::set_with_window`, which this replaces). This
/// mirrors `counters.rs`'s `Counters`/`CpuCounters`: one group per read
/// section, member data set plain, window stamped once at the end of the
/// section.
///
/// Each `Histogram` wraps exactly one BPF map and its own `refresh()` reads
/// that one map — nothing here ever brackets more than one map's read in a
/// single acquisition, so every declared histogram group in the migrated
/// samplers is one group per histogram (never shared across histograms);
/// see `docs/superpowers/plans/2026-08-18-stage3c-wave1-sampler-migration.md`
/// for the samplers this was rolled out to.
///
/// The bracket is wider than the actual bucket read for the same reason as
/// the counters machinery: `finish()` only runs once `update_from` has
/// returned, so the window's `end` is when the WHOLE read (mmap access +
/// bucket copy) completed, not some earlier instant within it — an honest
/// upper bound on the true acquisition time, never an underestimate.
pub struct Histogram<'a> {
    _map: &'a libbpf_rs::Map<'a>,
    mmap: memmap2::MmapMut,
    buckets: usize,
    histogram: &'static RwLockHistogram,
    group: &'static AcquisitionGroup,
}

impl<'a> Histogram<'a> {
    pub fn new(
        map: &'a libbpf_rs::Map,
        histogram: &'static RwLockHistogram,
        group: &'static AcquisitionGroup,
    ) -> Self {
        let buckets = histogram.config().total_buckets();

        let mmap_len = whole_pages::<u64>(buckets) * PAGE_SIZE;

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
            buckets,
            histogram,
            group,
        }
    }

    pub fn refresh(&mut self) {
        // Bracket the mmap read + bucket copy as one acquisition; the
        // histogram's buckets are written plain (`update_from`), and the
        // group's window covers the whole read (stamped by `finish()`,
        // last — see the `# Windowing` section above).
        let acq = self.group.acquire();

        let (_prefix, buckets, _suffix) = unsafe { self.mmap.align_to::<u64>() };
        let n = self.buckets;
        let _ = self.histogram.update_from(&buckets[0..n]);

        acq.finish();
    }
}
