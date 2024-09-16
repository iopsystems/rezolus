use crate::common::bpf::*;
use crate::common::*;
use crate::*;

use libbpf_rs::Map;
use memmap2::{MmapMut, MmapOptions};
use metriken::LazyCounter;

use std::os::fd::{AsFd, AsRawFd, FromRawFd};

/// This wraps the BPF map along with an opened memory-mapped region for the map
/// values.
struct CounterMap<'a> {
    _map: &'a Map<'a>,
    mmap: MmapMut,
    bank_width: usize,
}

impl<'a> CounterMap<'a> {
    /// Create a new `CounterMap` from the provided BPF map that holds the
    /// provided number of counters.
    pub fn new(map: &'a Map, counters: usize) -> Result<Self, ()> {
        // each CPU has its own bank of counters, this bank is the next nearest
        // whole number of cachelines wide
        let bank_cachelines = whole_cachelines::<u64>(counters);

        // the number of possible slots per bank of counters
        let bank_width = bank_cachelines * COUNTERS_PER_CACHELINE;

        // our total mapped region size in bytes
        let total_bytes = bank_cachelines * CACHELINE_SIZE * MAX_CPUS;

        let fd = map.as_fd().as_raw_fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mmap: MmapMut = unsafe {
            MmapOptions::new()
                .len(total_bytes)
                .map_mut(&file)
                .map_err(|e| error!("failed to mmap() bpf counterset: {e}"))
        }?;

        let (_prefix, values, _suffix) = unsafe { mmap.align_to::<u64>() };

        if values.len() != MAX_CPUS * bank_width {
            error!("mmap region not aligned or width doesn't match");
            return Err(());
        }

        Ok(Self {
            _map: map,
            mmap,
            bank_width,
        })
    }

    /// Borrow a reference to the raw values.
    pub fn values(&self) -> &[u64] {
        let (_prefix, values, _suffix) = unsafe { self.mmap.align_to::<u64>() };
        values
    }

    /// Get the bank width which is the stride for reading through the values
    /// slice.
    pub fn bank_width(&self) -> usize {
        self.bank_width
    }
}

/// Represents a set of counters where the BPF map has one bank of counters per
/// CPU to avoid contention. Each bank of counters is a whole number of
/// cachelines to avoid false sharing. Per-CPU counters are not individually
/// tracked.
pub struct Counters<'a> {
    counter_map: CounterMap<'a>,
    counters: Vec<&'static LazyCounter>,
    values: Vec<u64>,
}

impl<'a> Counters<'a> {
    /// Create a new set of counters from the provided BPF map and collection of
    /// counter metrics.
    pub fn new(map: &'a Map, counters: Vec<&'static LazyCounter>) -> Self {
        // we need temporary buffer so we can total up the per-CPU values
        let values = vec![0; counters.len()];

        // load the BPF counter map
        let counter_map = CounterMap::new(map, counters.len()).expect("failed to initialize");

        Self {
            counter_map,
            counters,
            values,
        }
    }

    /// Refreshes the counters by reading from the BPF map and setting each
    /// counter metric to the current value.
    pub fn refresh(&mut self) {
        // zero out temp counters
        self.values.fill(0);

        let bank_width = self.counter_map.bank_width();

        // borrow the BPF counters map so we can read per-cpu values
        let counters = self.counter_map.values();

        // iterate through and increment our local value for each cpu counter
        for cpu in 0..MAX_CPUS {
            for idx in 0..self.counters.len() {
                let value = counters[idx + cpu * bank_width];

                // add this CPU's counter to the combined value for this counter
                self.values[idx] = self.values[idx].wrapping_add(value);
            }
        }

        // set each counter metric to its new combined value
        for (value, counter) in self.values.iter().zip(self.counters.iter_mut()) {
            counter.set(*value);
        }
    }
}

/// Represents a set of counters where the BPF map has one bank of counters per
/// CPU. Like `Counters`, each bank is a whole number of cachelines to avoid
/// false sharing. Unlike `Counters`, each CPU's counters are also individually
/// tracked.
pub struct CpuCounters<'a> {
    counter_map: CounterMap<'a>,
    totals: Vec<&'static LazyCounter>,
    individual: ScopedCounters,
    values: Vec<u64>,
}

impl<'a> CpuCounters<'a> {
    /// Create a new set of counters from the provided BPF map and collection of
    /// counter metrics.
    pub fn new(
        map: &'a Map,
        totals: Vec<&'static LazyCounter>,
        individual: ScopedCounters,
    ) -> Self {
        // we need temporary buffer so we can total up the per-CPU values
        let values = vec![0; totals.len()];

        // load the BPF counter map
        let counter_map = CounterMap::new(map, totals.len()).expect("failed to initialize");

        Self {
            counter_map,
            totals,
            individual,
            values,
        }
    }

    /// Refreshes the counters by reading from the BPF map and setting each
    /// counter metric to the current value.
    pub fn refresh(&mut self) {
        // zero out temp counters
        self.values.fill(0);

        let bank_width = self.counter_map.bank_width();

        // borrow the BPF counters map so we can read per-cpu values
        let counters = self.counter_map.values();

        // iterate through and increment our local value for each cpu counter
        for cpu in 0..MAX_CPUS {
            for idx in 0..self.totals.len() {
                let value = counters[idx + cpu * bank_width];

                // add this CPU's counter to the combined value for this counter
                self.values[idx] = self.values[idx].wrapping_add(value);

                // set this CPU's counter to the new value
                let _ = self.individual.set(cpu, idx, value);
            }
        }

        // set each counter metric to its new combined value
        for (value, counter) in self.values.iter().zip(self.totals.iter_mut()) {
            counter.set(*value);
        }
    }
}
