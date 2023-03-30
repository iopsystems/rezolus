#![allow(dead_code)]

pub use ouroboros::*;

use super::*;
use std::os::fd::FromRawFd;

mod keys;

use keys::KEYS;

#[cfg(feature = "bpf")]
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

pub struct Distribution<'a> {
    map: &'a libbpf_rs::Map,
    key_buf: Vec<u8>,
    val_buf: Vec<u8>,
    prev: [u64; 7424],
    heatmap: &'static LazyHeatmap,
}

impl<'a> Distribution<'a> {
    pub fn new(map: &'a libbpf_rs::Map, heatmap: &'static LazyHeatmap) -> Self {
        Self {
            map,
            key_buf: Vec::new(),
            val_buf: Vec::new(),
            prev: [0; 7424],
            heatmap,
        }
    }

    pub fn refresh(&mut self, now: Instant) {
        let opts = libbpf_sys::bpf_map_batch_opts {
            sz: 24 as libbpf_sys::size_t,
            elem_flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
            flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
        };

        self.key_buf.clear();
        self.key_buf.extend_from_slice(&KEYS[0..(7424 * 4)]);

        self.val_buf.clear();
        self.val_buf.resize(7424 * 8, 0);

        let mut nkeys: u32 = 7424;
        let in_batch = std::ptr::null_mut();
        let mut out_batch = 0_u32;

        let ret = unsafe {
            libbpf_sys::bpf_map_lookup_batch(
                self.map.fd(),
                in_batch as *mut core::ffi::c_void,
                &mut out_batch as *mut _ as *mut core::ffi::c_void,
                self.key_buf.as_mut_ptr() as *mut core::ffi::c_void,
                self.val_buf.as_mut_ptr() as *mut core::ffi::c_void,
                &mut nkeys as *mut libbpf_sys::__u32,
                &opts as *const libbpf_sys::bpf_map_batch_opts,
            )
        };

        let nkeys = nkeys as usize;

        if ret == 0 {
            unsafe {
                self.key_buf.set_len(4 * nkeys);
                self.val_buf.set_len(8 * nkeys);
            }
        } else {
            return;
        }

        let mut key = [0; 4];
        let mut current = [0; 8];

        for i in 0..nkeys {
            key.copy_from_slice(&self.key_buf[(i * 4)..((i + 1) * 4)]);
            current.copy_from_slice(&self.val_buf[(i * 8)..((i + 1) * 8)]);

            let k = u32::from_ne_bytes(key) as usize;
            let c = u64::from_ne_bytes(current);

            let delta = c.wrapping_sub(self.prev[k]);
            self.prev[k] = c;

            if delta > 0 {
                let value = key_to_value(k as u64);
                self.heatmap.increment(now, value as _, delta as _);
            }
        }
    }
}

pub struct MemmapDistribution<'a> {
    map: &'a libbpf_rs::Map,
    mmap: memmap2::MmapMut,
    prev: [u64; 7424],
    heatmap: &'static LazyHeatmap,
}

impl<'a> MemmapDistribution<'a> {
    pub fn new(map: &'a libbpf_rs::Map, heatmap: &'static LazyHeatmap) -> Self {
        let fd = map.fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mmap = unsafe {
            memmap2::MmapOptions::new()
                .len(61440)
                .map_mut(&file)
                .expect("failed to mmap() bpf distribution")
        };

        // let mmap = unsafe { memmap2::MmapOptions::new::map(&file).expect("failed to memmap distribution") };

        Self {
            map,
            mmap,
            prev: [0; 7424],
            heatmap,
        }
    }

    pub fn refresh(&mut self, now: Instant) {
        for (idx, prev) in self.prev.iter_mut().enumerate() {
            let start = idx * 8;
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

pub struct CounterSet<'a> {
    map: &'a libbpf_rs::Map,
    key_buf: Vec<u8>,
    val_buf: Vec<u8>,
    counters: Vec<Counter>,
}

impl<'a> CounterSet<'a> {
    pub fn new(map: &'a libbpf_rs::Map, counters: Vec<Counter>) -> Self {
        Self {
            map,
            key_buf: Vec::new(),
            val_buf: Vec::new(),
            counters,
        }
    }

    pub fn refresh(&mut self, now: Instant, elapsed: f64) {
        let ncounters = self.counters.len();

        let opts = libbpf_sys::bpf_map_batch_opts {
            sz: 24 as libbpf_sys::size_t,
            elem_flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
            flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
        };

        self.key_buf.clear();
        self.key_buf.extend_from_slice(&KEYS[0..(ncounters * 4)]);

        self.val_buf.clear();
        self.val_buf.resize(ncounters * 8, 0);

        let mut nkeys: u32 = ncounters as _;
        let in_batch = std::ptr::null_mut();
        let mut out_batch = 0_u32;

        let ret = unsafe {
            libbpf_sys::bpf_map_lookup_batch(
                self.map.fd(),
                in_batch as *mut core::ffi::c_void,
                &mut out_batch as *mut _ as *mut core::ffi::c_void,
                self.key_buf.as_mut_ptr() as *mut core::ffi::c_void,
                self.val_buf.as_mut_ptr() as *mut core::ffi::c_void,
                &mut nkeys as *mut libbpf_sys::__u32,
                &opts as *const libbpf_sys::bpf_map_batch_opts,
            )
        };

        let nkeys = nkeys as usize;

        if ret == 0 {
            unsafe {
                self.val_buf.set_len(8 * nkeys);
                self.key_buf.set_len(4 * nkeys);
            }
        } else {
            return;
        }

        let mut key = [0; 4];
        let mut current = [0; 8];

        for i in 0..nkeys {
            key.copy_from_slice(&self.key_buf[(i * 4)..((i + 1) * 4)]);
            current.copy_from_slice(&self.val_buf[(i * 8)..((i + 1) * 8)]);

            let k = u32::from_ne_bytes(key) as usize;
            let c = u64::from_ne_bytes(current);

            if let Some(counter) = self.counters.get_mut(k) {
                counter.set(now, elapsed, c)
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

impl<'a> MemmapCounterSet<'a> {
    pub fn new(map: &'a libbpf_rs::Map, counters: Vec<Counter>) -> Self {
        let ncounters = counters.len();
        let cachelines = (ncounters as f64 / 8.0).ceil() as usize;

        let fd = map.fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mmap = unsafe {
            memmap2::MmapOptions::new()
                .len(cachelines * 65536)
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

        for cpu in 0..1024 {
            for idx in 0..self.counters.len() {
                let start = (cpu * self.cachelines * 64) + (idx * 8);
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

    // pub fn refresh(&mut self, now: Instant, elapsed: f64) {
    //     let ncounters = self.counters.len();

    //     let opts = libbpf_sys::bpf_map_batch_opts {
    //         sz: 24 as libbpf_sys::size_t,
    //         elem_flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
    //         flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
    //     };

    //     self.key_buf.clear();
    //     self.key_buf.extend_from_slice(&KEYS[0..(ncounters * 4)]);

    //     self.val_buf.clear();
    //     self.val_buf.resize(ncounters * 8, 0);

    //     let mut nkeys: u32 = ncounters as _;
    //     let in_batch = std::ptr::null_mut();
    //     let mut out_batch = 0_u32;

    //     let ret = unsafe {
    //         libbpf_sys::bpf_map_lookup_batch(
    //             self.map.fd(),
    //             in_batch as *mut core::ffi::c_void,
    //             &mut out_batch as *mut _ as *mut core::ffi::c_void,
    //             self.key_buf.as_mut_ptr() as *mut core::ffi::c_void,
    //             self.val_buf.as_mut_ptr() as *mut core::ffi::c_void,
    //             &mut nkeys as *mut libbpf_sys::__u32,
    //             &opts as *const libbpf_sys::bpf_map_batch_opts,
    //         )
    //     };

    //     let nkeys = nkeys as usize;

    //     if ret == 0 {
    //         unsafe {
    //             self.val_buf.set_len(8 * nkeys);
    //             self.key_buf.set_len(4 * nkeys);
    //         }
    //     } else {
    //         return;
    //     }

    //     let mut key = [0; 4];
    //     let mut current = [0; 8];

    //     for i in 0..nkeys {
    //         key.copy_from_slice(&self.key_buf[(i * 4)..((i + 1) * 4)]);
    //         current.copy_from_slice(&self.val_buf[(i * 8)..((i + 1) * 8)]);

    //         let k = u32::from_ne_bytes(key) as usize;
    //         let c = u64::from_ne_bytes(current);

    //         if let Some(counter) = self.counters.get_mut(k) {
    //             counter.set(now, elapsed, c)
    //         }
    //     }
    // }
}

pub struct PercpuCounter<'a> {
    map: &'a libbpf_rs::Map,
    buf: Vec<u8>,
    counter: Counter,
}

impl<'a> PercpuCounter<'a> {
    pub fn new(map: &'a libbpf_rs::Map, counter: Counter) -> Self {
        Self {
            map,
            buf: Vec::new(),
            counter,
        }
    }

    pub fn refresh(&mut self, now: Instant, elapsed: f64) {
        let num_cpu = libbpf_rs::num_possible_cpus().expect("failed to get number of cpus");

        let mut result: u64 = 0;

        let key = [0x00, 0x00, 0x00, 0x00];

        self.buf.clear();
        self.buf.resize(num_cpu * 8, 0);

        let ret = unsafe {
            libbpf_sys::bpf_map_lookup_elem(
                self.map.fd(),
                key.as_ptr() as *mut core::ffi::c_void,
                self.buf.as_mut_ptr() as *mut core::ffi::c_void,
            )
        };

        if ret != 0 {
            println!("ret: {ret}");
            return;
        }

        let mut current = [0; 8];

        for i in 0..num_cpu {
            current.copy_from_slice(&self.buf[(i * 8)..((i + 1) * 8)]);

            result = result.wrapping_add(u64::from_ne_bytes(current));
        }

        self.counter.set(now, elapsed, result);
    }
}

#[self_referencing]
pub struct Bpf<T: 'static> {
    skel: T,
    #[borrows(skel)]
    #[covariant]
    percpu_counters: Vec<PercpuCounter<'this>>,
    #[borrows(skel)]
    #[covariant]
    counter_sets: Vec<CounterSet<'this>>,
    #[borrows(skel)]
    #[covariant]
    memmap_counter_sets: Vec<MemmapCounterSet<'this>>,
    #[borrows(skel)]
    #[covariant]
    distributions: Vec<Distribution<'this>>,
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
            percpu_counters_builder: |_| Vec::new(),
            counter_sets_builder: |_| Vec::new(),
            memmap_counter_sets_builder: |_| Vec::new(),
            distributions_builder: |_| Vec::new(),
            memmap_distributions_builder: |_| Vec::new(),
        }
        .build()
    }

    pub fn add_percpu_counter(&mut self, name: &str, counter: Counter) {
        self.with_mut(|this| {
            this.percpu_counters
                .push(PercpuCounter::new(this.skel.map(name), counter));
        })
    }

    pub fn add_counter_set(&mut self, name: &str, counters: Vec<Counter>) {
        self.with_mut(|this| {
            this.counter_sets
                .push(CounterSet::new(this.skel.map(name), counters));
        })
    }

    pub fn add_memmap_counter_set(&mut self, name: &str, counters: Vec<Counter>) {
        self.with_mut(|this| {
            this.memmap_counter_sets
                .push(MemmapCounterSet::new(this.skel.map(name), counters));
        })
    }

    pub fn add_distribution(&mut self, name: &str, heatmap: &'static LazyHeatmap) {
        self.with_mut(|this| {
            this.distributions
                .push(Distribution::new(this.skel.map(name), heatmap));
        })
    }

    pub fn add_memmap_distribution(&mut self, name: &str, heatmap: &'static LazyHeatmap) {
        self.with_mut(|this| {
            this.memmap_distributions
                .push(MemmapDistribution::new(this.skel.map(name), heatmap));
        })
    }

    fn sample_percpu_counters(&mut self, now: Instant, elapsed: f64) {
        self.with_mut(|this| {
            for counter in this.percpu_counters.iter_mut() {
                counter.refresh(now, elapsed);
            }
        })
    }

    fn sample_counter_sets(&mut self, now: Instant, elapsed: f64) {
        self.with_mut(|this| {
            for counter_set in this.counter_sets.iter_mut() {
                counter_set.refresh(now, elapsed);
            }
        });

        self.with_mut(|this| {
            for counter_set in this.memmap_counter_sets.iter_mut() {
                counter_set.refresh(now, elapsed);
            }
        });
    }

    pub fn refresh_counters(&mut self, now: Instant, elapsed: f64) {
        self.sample_percpu_counters(now, elapsed);
        self.sample_counter_sets(now, elapsed);
    }

    pub fn refresh_distributions(&mut self, now: Instant) {
        self.with_mut(|this| {
            for distribution in this.distributions.iter_mut() {
                distribution.refresh(now);
            }
        });
        self.with_mut(|this| {
            for distribution in this.memmap_distributions.iter_mut() {
                distribution.refresh(now);
            }
        });
    }
}
