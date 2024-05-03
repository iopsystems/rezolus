use super::*;
use ringlog::*;

/// Represents a distribution in a BPF map. The distribution must be created
/// with:
///
/// ```c
/// struct {
///     __uint(type, BPF_MAP_TYPE_ARRAY);
///     __uint(map_flags, BPF_F_MMAPABLE);
///     __type(key, u32);
///     __type(value, u64);
///     __uint(max_entries, 7424);
/// } some_distribution_name SEC(".maps");
/// ```
///
/// This distribution must also be indexed into using the `value_to_index`
/// helper from `histogram.h`. This results in a histogram that uses 64bit
/// counters and covers the entire range of u64 values. This histogram occupies
/// 60KB in kernel space and an additional 60KB in user space.
///
/// The distribution should be given some meaningful name in the BPF program.
pub struct Distribution<'a> {
    _map: &'a libbpf_rs::Map,
    mmap: memmap2::MmapMut,
    buffer: Vec<u64>,
    buckets: usize,
    aligned: bool,
    histogram: &'static RwLockHistogram,
}

impl<'a> Distribution<'a> {
    pub fn new(map: &'a libbpf_rs::Map, histogram: &'static RwLockHistogram) -> Self {
        let buckets = histogram.config().total_buckets();

        let mmap_len = histogram_pages(buckets) * PAGE_SIZE;

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

        let aligned = if data.len() == expected_len {
            true
        } else {
            warn!("mmap region misaligned or did not have expected number of values {} != {expected_len}", data.len());
            false
        };

        Self {
            _map: map,
            mmap,
            buffer: Vec::new(),
            buckets,
            aligned,
            histogram,
        }
    }

    pub fn refresh(&mut self) {
        if self.aligned {
            let (_prefix, buckets, _suffix) = unsafe { self.mmap.align_to::<u64>() };
            let _ = self.histogram.update_from(&buckets[0..self.buckets]);
        } else {
            self.buffer.resize(self.buckets, 0);

            for (idx, bucket) in self.buffer.iter_mut().enumerate() {
                let start = idx * std::mem::size_of::<u64>();

                if start + std::mem::size_of::<u64>() > self.mmap.len() {
                    break;
                }

                let val = u64::from_ne_bytes(
                    <[u8; std::mem::size_of::<u64>()]>::try_from(
                        &self.mmap[start..(start + std::mem::size_of::<u64>())],
                    )
                    .unwrap(),
                );

                *bucket = val;
            }

            let _ = self.histogram.update_from(&self.buffer[0..self.buckets]);
        }
    }
}
