use super::*;
use core::time::Duration;
use metriken::DynBoxedMetric;
use metriken::RwLockHistogram;
use ouroboros::*;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::Arc;

pub use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};

mod counters;
mod distribution;

use counters::Counters;
use distribution::Distribution;

pub use counters::PercpuCounters;

const PAGE_SIZE: usize = 4096;
const CACHELINE_SIZE: usize = 64;

/// The maximum number of CPUs supported. Allows a normal bpf map behave like
/// per-CPU counters by packing counters into cacheline sized chunks such that
/// no CPUs will share cacheline sized segments of the counter map.
static MAX_CPUS: usize = 1024;

/// Returns the next nearest whole number of pages that fits a histogram with
/// the provided config.
pub fn histogram_pages(buckets: usize) -> usize {
    ((buckets * std::mem::size_of::<u64>()) + PAGE_SIZE - 1) / PAGE_SIZE
}

/// A type that builds the userspace components of a BPF program including
/// registering counters, distributions, and intiailizing a map with values.
pub struct BpfBuilder<T: 'static> {
    bpf: _Bpf<T>,
}

impl<T: 'static + GetMap> BpfBuilder<T> {
    pub fn new(skel: T) -> Self {
        Self {
            bpf: _Bpf::from_skel(skel),
        }
    }

    pub fn build(self) -> Bpf<T> {
        Bpf { bpf: self.bpf }
    }

    pub fn counters(mut self, name: &str, counters: Vec<Counter>) -> Self {
        self.bpf.add_counters(name, counters);
        self
    }

    pub fn percpu_counters(
        mut self,
        name: &str,
        counters: Vec<Counter>,
        percpu: Arc<PercpuCounters>,
    ) -> Self {
        self.bpf.add_counters_with_percpu(name, counters, percpu);
        self
    }

    pub fn distribution(mut self, name: &str, histogram: &'static RwLockHistogram) -> Self {
        self.bpf.add_distribution(name, histogram);
        self
    }

    pub fn map(self, name: &str, values: &[u64]) -> Self {
        let fd = self.bpf.map(name).as_fd().as_raw_fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mut mmap = unsafe {
            memmap2::MmapOptions::new()
                .len(std::mem::size_of_val(values))
                .map_mut(&file)
                .expect("failed to mmap() bpf map")
        };

        for (index, bytes) in mmap
            .chunks_exact_mut(std::mem::size_of::<u64>())
            .enumerate()
        {
            let value = bytes.as_mut_ptr() as *mut u64;
            unsafe {
                *value = values[index];
            }
        }

        let _ = mmap.flush();

        self
    }
}

pub struct Bpf<T: 'static> {
    bpf: _Bpf<T>,
}

impl<T: 'static + GetMap> Bpf<T> {
    pub fn refresh_counters(&mut self, elapsed: Duration) {
        self.bpf.refresh_counters(elapsed.as_secs_f64())
    }

    pub fn refresh_distributions(&mut self) {
        self.bpf.refresh_distributions()
    }
}

#[self_referencing]
struct _Bpf<T: 'static> {
    skel: T,
    #[borrows(skel)]
    #[covariant]
    counters: Vec<Counters<'this>>,
    #[borrows(skel)]
    #[covariant]
    distributions: Vec<Distribution<'this>>,
}

pub trait GetMap {
    fn map(&self, name: &str) -> &libbpf_rs::Map;
}

impl<T: 'static + GetMap> _Bpf<T> {
    pub fn from_skel(skel: T) -> Self {
        _BpfBuilder {
            skel,
            counters_builder: |_| Vec::new(),
            distributions_builder: |_| Vec::new(),
        }
        .build()
    }

    pub fn map(&self, name: &str) -> &libbpf_rs::Map {
        self.with(|this| this.skel.map(name))
    }

    pub fn add_counters(&mut self, name: &str, counters: Vec<Counter>) {
        self.with_mut(|this| {
            this.counters.push(Counters::new(
                this.skel.map(name),
                counters,
                Default::default(),
            ));
        })
    }

    pub fn add_counters_with_percpu(
        &mut self,
        name: &str,
        counters: Vec<Counter>,
        percpu_counters: Arc<PercpuCounters>,
    ) {
        self.with_mut(|this| {
            this.counters.push(Counters::new(
                this.skel.map(name),
                counters,
                percpu_counters,
            ));
        })
    }

    pub fn add_distribution(&mut self, name: &str, histogram: &'static RwLockHistogram) {
        self.with_mut(|this| {
            this.distributions
                .push(Distribution::new(this.skel.map(name), histogram));
        })
    }

    pub fn refresh_counters(&mut self, elapsed: f64) {
        self.with_mut(|this| {
            for counters in this.counters.iter_mut() {
                counters.refresh(elapsed);
            }
        })
    }

    pub fn refresh_distributions(&mut self) {
        self.with_mut(|this| {
            for distribution in this.distributions.iter_mut() {
                distribution.refresh();
            }
        })
    }
}
