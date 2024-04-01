use super::*;
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

/// The maximum number of CPUs supported. Used to make `CounterSet`s behave like
/// per-CPU counters by packing counters into cacheline sized chunks such that
/// no CPUs will share cacheline sized segments of the counter map.
static MAX_CPUS: usize = 1024;

/// The number of histogram buckets based on a rustcommon histogram with the
/// parameters `grouping_power = 7` `max_value_power = 64`.
///
/// NOTE: this *must* remain in-sync across both C and Rust components of BPF
/// code.
const HISTOGRAM_BUCKETS: usize = 7424;
const HISTOGRAM_PAGES: usize = 15;

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
