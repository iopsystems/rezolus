use super::*;

/// Represents a collection of counters in a open BPF map. The map must be
/// created with:
///
/// ```c
/// // counts the total number of syscalls
/// struct {
///     __uint(type, BPF_MAP_TYPE_ARRAY);
///     __uint(map_flags, BPF_F_MMAPABLE);
///     __type(key, u32);
///     __type(value, u64);
///     __uint(max_entries, 8192); // good for up to 1024 cores w/ 8 counters
/// } counters SEC(".maps");
/// ```
///
/// The number of entries is flexible, but should be a multiple of 8192. This
/// struct will automatically know the multiple based on the number of counters.
///
/// The name is also flexible, but it is recommended to pack the counters for
/// each BPF program into one map, so `counters` is a reasonable name to use.
pub struct Counters<'a> {
    _map: &'a libbpf_rs::Map,
    mmap: memmap2::MmapMut,
    values: Vec<u64>,
    cachelines: usize,
    counters: Vec<Counter>,
}

impl<'a> Counters<'a> {
    pub fn new(map: &'a libbpf_rs::Map, counters: Vec<Counter>) -> Self {
        let ncounters = counters.len();
        let cachelines = (ncounters as f64 / std::mem::size_of::<u64>() as f64).ceil() as usize;

        let fd = map.as_fd().as_raw_fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mmap = unsafe {
            memmap2::MmapOptions::new()
                .len(cachelines * CACHELINE_SIZE * MAX_CPUS)
                .map_mut(&file)
                .expect("failed to mmap() bpf counterset")
        };

        Self {
            _map: map,
            mmap,
            cachelines,
            counters,
            values: vec![0; ncounters],
        }
    }

    pub fn refresh(&mut self, now: Instant, elapsed: f64) {
        for value in self.values.iter_mut() {
            *value = 0;
        }

        for cpu in 0..MAX_CPUS {
            for idx in 0..self.counters.len() {
                let start =
                    (cpu * self.cachelines * CACHELINE_SIZE) + (idx * std::mem::size_of::<u64>());
                let value = u64::from_ne_bytes([
                    self.mmap[start + 0],
                    self.mmap[start + 1],
                    self.mmap[start + 2],
                    self.mmap[start + 3],
                    self.mmap[start + 4],
                    self.mmap[start + 5],
                    self.mmap[start + 6],
                    self.mmap[start + 7],
                ]);

                self.values[idx] = self.values[idx].wrapping_add(value);
            }
        }

        for (value, counter) in self.values.iter().zip(self.counters.iter_mut()) {
            counter.set(now, elapsed, *value);
        }
    }
}
