use crate::common::bpf::*;
use crate::*;

use metriken::RwLockHistogram;

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
pub struct Histogram<'a> {
    _map: &'a libbpf_rs::Map<'a>,
    mmap: memmap2::MmapMut,
    buckets: usize,
    histogram: &'static RwLockHistogram,
}

impl<'a> Histogram<'a> {
    pub fn new(map: &'a libbpf_rs::Map, histogram: &'static RwLockHistogram) -> Self {
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
        }
    }

    pub fn refresh(&mut self) {
        let (_prefix, buckets, _suffix) = unsafe { self.mmap.align_to::<u64>() };
        let _ = self.histogram.update_from(&buckets[0..self.buckets]);
    }
}
