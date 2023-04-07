#![allow(dead_code)]

pub use ouroboros::*;

use super::*;
use std::os::fd::FromRawFd;

mod keys;

use keys::KEYS;

const PAGE_SIZE: usize = 4096;
const CACHELINE_SIZE: usize = 64;

/// The maximum number of CPUs supported. Used to make `CounterSet`s behave like
/// per-CPU counters by packing counters into cacheline sized chunks such that
/// no CPUs will share cacheline sized segments of the counter map.
static MAX_CPUS: usize = 1024;

/// The number of histogram buckets based on a rustcommon histogram with the
/// parameters `m = 0`, `r = 8`, `n = 64`.
///
/// NOTE: this *must* remain in-sync across both C and Rust components of BPF
/// code.
const HISTOGRAM_BUCKETS: usize = 7424;
const HISTOGRAM_BYTES: usize = HISTOGRAM_BUCKETS * std::mem::size_of::<u64>();
const HISTOGRAM_PAGES: usize = 15;

/// This function converts indices back to values for rustcommon histogram with
/// the parameters `m = 0`, `r = 8`, `n = 64`. This covers the entire range from
/// 1 to u64::MAX and uses 7424 buckets per histogram, which works out to 58KB
/// for each histogram in kernelspace (64bit counters). In userspace, we will
/// we will likely have 61 histograms => 1769KB per stat in userspace.
pub fn key_to_value(index: u64) -> u64 {
    // g = index >> (r - m - 1)
    let g = index >> 7;
    // b = index - g * G + 1
    let b = index - g * 128 + 1;

    if g < 1 {
        // (1 << m) * b - 1
        b - 1
    } else {
        // (1 << (r - 2 + g)) + (1 << (m + g - 1)) * b - 1
        (1 << (6 + g)) + (1 << (g - 1)) * b - 1
    }
}

pub struct MemmapDistribution<'a> {
    map: &'a libbpf_rs::Map,
    mmap: memmap2::MmapMut,
    prev: [u64; HISTOGRAM_BUCKETS],
    heatmap: &'static LazyHeatmap,
}

impl<'a> MemmapDistribution<'a> {
    pub fn new(map: &'a libbpf_rs::Map, heatmap: &'static LazyHeatmap) -> Self {
        let fd = map.fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mmap = unsafe {
            memmap2::MmapOptions::new()
                .len(HISTOGRAM_PAGES * PAGE_SIZE) // TODO(bmartin): double check this...
                .map_mut(&file)
                .expect("failed to mmap() bpf distribution")
        };

        Self {
            map,
            mmap,
            prev: [0; HISTOGRAM_BUCKETS],
            heatmap,
        }
    }

    pub fn refresh(&mut self, now: Instant) {
        for (idx, prev) in self.prev.iter_mut().enumerate() {
            let start = idx * std::mem::size_of::<u64>();
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

            let delta = val - *prev;

            *prev = val;

            if delta > 0 {
                let value = key_to_value(idx as u64);
                self.heatmap.increment(now, value as _, delta as _);
            }
        }
    }
}

pub struct MemmapCounterSet<'a> {
    map: &'a libbpf_rs::Map,
    mmap: memmap2::MmapMut,
    values: Vec<u64>,
    cachelines: usize,
    counters: Vec<Counter>,
}

// impl Drop for MemmapCounterSet<'a> {
//     fn drop(&mut self) {
//         // let alignment = self.ptr as usize % PAGE_SIZE;
//         // let len = self.len() + alignment;
//         // let len = len.max(1);
//         // // Any errors during unmapping/closing are ignored as the only way
//         // // to report them would be through panicking which is highly discouraged
//         // // in Drop impls, c.f. https://github.com/rust-lang/lang-team/issues/97
//         // unsafe {
//         //     let ptr = self.ptr.offset(-(alignment as isize));
//         //     libc::munmap(ptr, len as libc::size_t);
//         // }
//     }
// }

impl<'a> MemmapCounterSet<'a> {
    pub fn new(map: &'a libbpf_rs::Map, counters: Vec<Counter>) -> Self {
        let ncounters = counters.len();
        let cachelines = (ncounters as f64 / std::mem::size_of::<u64>() as f64).ceil() as usize;

        let fd = map.fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mmap = unsafe {
            memmap2::MmapOptions::new()
                .len(cachelines * CACHELINE_SIZE * MAX_CPUS)
                .map_mut(&file)
                .expect("failed to mmap() bpf counterset")
        };

        Self {
            map,
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
                let start = (cpu * self.cachelines * CACHELINE_SIZE) + (idx * std::mem::size_of::<u64>());
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

#[self_referencing]
pub struct Bpf<T: 'static> {
    skel: T,
    #[borrows(skel)]
    #[covariant]
    memmap_counter_sets: Vec<MemmapCounterSet<'this>>,
    #[borrows(skel)]
    #[covariant]
    memmap_distributions: Vec<MemmapDistribution<'this>>,
}

pub trait GetMap {
    fn map(&self, name: &str) -> &libbpf_rs::Map;
}

impl<T: 'static + GetMap> Bpf<T> {
    pub fn from_skel(skel: T) -> Self {
        BpfBuilder {
            skel,
            memmap_counter_sets_builder: |_| Vec::new(),
            memmap_distributions_builder: |_| Vec::new(),
        }
        .build()
    }

    pub fn add_memmap_counter_set(&mut self, name: &str, counters: Vec<Counter>) {
        self.with_mut(|this| {
            this.memmap_counter_sets
                .push(MemmapCounterSet::new(this.skel.map(name), counters));
        })
    }

    pub fn add_memmap_distribution(&mut self, name: &str, heatmap: &'static LazyHeatmap) {
        self.with_mut(|this| {
            this.memmap_distributions
                .push(MemmapDistribution::new(this.skel.map(name), heatmap));
        })
    }

    pub fn refresh_counters(&mut self, now: Instant, elapsed: f64) {
        self.with_mut(|this| {
            for counter_set in this.memmap_counter_sets.iter_mut() {
                counter_set.refresh(now, elapsed);
            }
        })
    }

    pub fn refresh_distributions(&mut self, now: Instant) {
        self.with_mut(|this| {
            for distribution in this.memmap_distributions.iter_mut() {
                distribution.refresh(now);
            }
        })
    }
}
