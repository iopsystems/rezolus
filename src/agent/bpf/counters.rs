use super::*;
use crate::agent::*;
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

/// Tracks total counts for a set of-per CPU counters. The BPF map must have one
/// bank of counters per CPU, padded to a whole number of cachelines. This
/// avoids contention and false sharing. Does not track per-CPU counts.
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

/// Tracks per-CPU counters. The BPF map layout is the same as for `Counters`,
/// however, instead of tracking totals, only the per-CPU counts are tracked as
/// a `CounterGroup`.
pub struct CpuCounters<'a> {
    counter_map: CounterMap<'a>,
    counters: Vec<&'static CounterGroup>,
}

impl<'a> CpuCounters<'a> {
    /// Create a new set of counters from the provided BPF map and collection of
    /// counter metrics.
    pub fn new(map: &'a Map, counters: Vec<&'static CounterGroup>) -> Self {
        // load the BPF counter map
        let counter_map = CounterMap::new(map, counters.len()).expect("failed to initialize");

        Self {
            counter_map,
            counters,
        }
    }

    /// Refreshes the counters by reading from the BPF map and setting each
    /// counter metric to the current value.
    pub fn refresh(&mut self) {
        let bank_width = self.counter_map.bank_width();

        // borrow the BPF counters map so we can read per-cpu values
        let counters = self.counter_map.values();

        // iterate through and increment our local value for each cpu counter
        for cpu in 0..MAX_CPUS {
            for idx in 0..self.counters.len() {
                let value = counters[idx + cpu * bank_width];

                // set this CPU's counter to the new value
                let _ = self.counters[idx].set(cpu, value);
            }
        }
    }
}

/// Represents a set of counters where the BPF map is a dense set of counters,
/// meaning there is no padding. No aggregation is performed, and the values are
/// updated into a single `RwLockCounterGroup`.
pub struct PackedCounters<'a> {
    _map: &'a Map<'a>,
    mmap: MmapMut,
    counters: &'static CounterGroup,
}

impl<'a> PackedCounters<'a> {
    /// Create a new set of counters from the provided BPF map and collection of
    /// counter metrics.
    ///
    /// The map layout is not cacheline padded. The ordering of the dynamic
    /// counters must exactly match the layout in the BPF map.
    pub fn new(map: &'a Map, counters: &'static CounterGroup) -> Self {
        let total_bytes = counters.len() * std::mem::size_of::<u64>();

        let fd = map.as_fd().as_raw_fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mmap: MmapMut = unsafe {
            MmapOptions::new()
                .len(total_bytes)
                .map_mut(&file)
                .expect("failed to mmap() bpf counterset")
        };

        let (_prefix, values, _suffix) = unsafe { mmap.align_to::<u64>() };

        if values.len() != counters.len() {
            panic!("mmap region not aligned or width doesn't match");
        }

        Self {
            _map: map,
            mmap,
            counters,
        }
    }

    /// Refreshes the counters by reading from the BPF map and setting each
    /// counter metric to the current value.
    pub fn refresh(&mut self) {
        let (_prefix, values, _suffix) = unsafe { self.mmap.align_to::<u64>() };

        // update all individual counters
        for (idx, value) in values.iter().enumerate() {
            if *value != 0 {
                let _ = self.counters.set(idx, *value);
            }
        }
    }
}
