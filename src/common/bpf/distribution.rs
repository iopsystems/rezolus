use super::*;

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
/// 60KB in kernel space and an additional ~3.5MB in user space.
///
/// The distribution should be given some meaningful name in the BPF program.
pub struct Distribution<'a> {
    _map: &'a libbpf_rs::Map,
    mmap: memmap2::MmapMut,
    buffer: [u64; HISTOGRAM_BUCKETS],
    histogram: &'static RwLockHistogram,
}

impl<'a> Distribution<'a> {
    pub fn new(map: &'a libbpf_rs::Map, histogram: &'static RwLockHistogram) -> Self {
        let fd = map.as_fd().as_raw_fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mmap = unsafe {
            memmap2::MmapOptions::new()
                .len(HISTOGRAM_PAGES * PAGE_SIZE) // TODO(bmartin): double check this...
                .map_mut(&file)
                .expect("failed to mmap() bpf distribution")
        };

        Self {
            _map: map,
            mmap,
            buffer: [0; HISTOGRAM_BUCKETS],
            histogram,
        }
    }

    pub fn refresh(&mut self) {
        // If the mmap'd region is properly aligned we can more efficiently
        // update the histogram. Otherwise, fall-back to the old strategy.

        let (_prefix, buckets, _suffix) = unsafe { self.mmap.align_to::<u64>() };

        if values.len() == HISTOGRAM_BUCKETS {
            let _ = self.histogram.update_from(&values);

            for (value, bucket) in values.iter().zip(buckets.iter()) {
                bucket.store(*value, Ordering::Relaxed)
            }
        } else {
            for (idx, bucket) in buckets.iter().enumerate() {
                let start = idx * std::mem::size_of::<u64>();

                if start + 7 >= self.mmap.len() {
                    break;
                }

                let val = u64::from_ne_bytes([
                    self.mmap[start + 0],
                    self.mmap[start + 1],
                    self.mmap[start + 2],
                    self.mmap[start + 3],
                    self.mmap[start + 4],
                    self.mmap[start + 5],
                    self.mmap[start + 6],
                    self.mmap[start + 7],
                ]);

                bucket.store(val, Ordering::Relaxed);
            }

            let _ = self.histogram.update_from(&self.buffer);
        }
    }
}
