use super::*;
use ouroboros::*;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};

pub use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};

mod counters;
mod distribution;

use counters::Counters;
use distribution::Distribution;

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

#[self_referencing]
pub struct Bpf<T: 'static> {
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

impl<T: 'static + GetMap> Bpf<T> {
    pub fn from_skel(skel: T) -> Self {
        BpfBuilder {
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
            this.counters
                .push(Counters::new(this.skel.map(name), counters));
        })
    }

    pub fn add_distribution(&mut self, name: &str, heatmap: &'static Heatmap) {
        self.with_mut(|this| {
            this.distributions
                .push(Distribution::new(this.skel.map(name), heatmap));
        })
    }

    pub fn refresh_counters(&mut self, now: Instant, elapsed: f64) {
        self.with_mut(|this| {
            for counters in this.counters.iter_mut() {
                counters.refresh(now, elapsed);
            }
        })
    }

    pub fn refresh_distributions(&mut self, now: Instant) {
        self.with_mut(|this| {
            for distribution in this.distributions.iter_mut() {
                distribution.refresh(now);
            }
        })
    }
}
