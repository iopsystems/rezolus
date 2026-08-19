use super::*;
use crate::agent::MAX_CPUS;

use libbpf_rs::Map;
use memmap2::{MmapMut, MmapOptions};
use metriken::{CounterGroup, LazyCounter};

use crate::agent::timing::AcquisitionGroup;

use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::atomic::AtomicU64;

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
///
/// # Windowing
///
/// The whole mmap read + per-CPU summation is bracketed by one
/// [`AcquisitionGroup`] acquisition: `acquire()` before the sweep starts,
/// `finish()` after every member counter has been set (stamp-last — see
/// [`AcquisitionGroup::acquire`]). Member values are set with the plain,
/// windowless `LazyCounter::set`; the group's single acquisition window
/// covers the whole sweep, not each entry individually. This replaces the
/// old per-entry-window discipline: entries no longer encode sweep order
/// (each used to carry a marginally later window than the one before it in
/// the sweep), because there is no longer a per-entry window to encode —
/// see `docs/journal/2026-08-17-window-sidecar-cost.md` proposal 2.
pub struct Counters<'a> {
    counter_map: CounterMap<'a>,
    counters: Vec<&'static LazyCounter>,
    values: Vec<u64>,
    group: &'static AcquisitionGroup,
}

impl<'a> Counters<'a> {
    /// Create a new set of counters from the provided BPF map and collection of
    /// counter metrics, stamping the group's acquisition window on every
    /// refresh.
    pub fn new(
        map: &'a Map,
        counters: Vec<&'static LazyCounter>,
        group: &'static AcquisitionGroup,
    ) -> Self {
        // we need temporary buffer so we can total up the per-CPU values
        let values = vec![0; counters.len()];

        let counter_map = CounterMap::new(map, counters.len()).expect("failed to initialize");

        Self {
            counter_map,
            counters,
            values,
            group,
        }
    }

    /// Refreshes the counters by reading from the BPF map and setting each
    /// counter metric to the current value.
    pub fn refresh(&mut self) {
        self.values.fill(0);

        let bank_width = self.counter_map.bank_width();

        // borrow the BPF counters map so we can read per-cpu values
        let counters = self.counter_map.values();

        // Bracket the mmap read + per-CPU summation as one acquisition;
        // values are set plain, the group's window covers the whole sweep.
        let acq = self.group.acquire();

        for cpu in 0..possible_cpus() {
            for idx in 0..self.values.len() {
                let value = counters[idx + cpu * bank_width];

                self.values[idx] = self.values[idx].wrapping_add(value);
            }
        }

        for (value, counter) in self.values.iter().zip(self.counters.iter_mut()) {
            counter.set(*value);
        }

        acq.finish();
    }
}

/// Tracks per-CPU counters. The BPF map layout is the same as for `Counters`,
/// however, instead of tracking totals, only the per-CPU counts are tracked as
/// a `CounterGroup`.
///
/// # Windowing
///
/// Same stamp-last discipline as [`Counters`]: one [`AcquisitionGroup`]
/// acquisition brackets the whole per-CPU sweep, member values are set with
/// the plain, windowless `CounterGroup::set`, and `finish()` is called once
/// every entry has been written. There is no longer a per-entry window —
/// entries do not encode sweep order — see `docs/journal/2026-08-17-window-sidecar-cost.md`
/// proposal 2.
pub struct CpuCounters<'a> {
    counter_map: CounterMap<'a>,
    counters: Vec<&'static CounterGroup>,
    group: &'static AcquisitionGroup,
}

impl<'a> CpuCounters<'a> {
    /// Create a new set of counters from the provided BPF map and collection of
    /// counter metrics, stamping the group's acquisition window on every
    /// refresh.
    pub fn new(
        map: &'a Map,
        counters: Vec<&'static CounterGroup>,
        group: &'static AcquisitionGroup,
    ) -> Self {
        let counter_map = CounterMap::new(map, counters.len()).expect("failed to initialize");

        Self {
            counter_map,
            counters,
            group,
        }
    }

    /// Refreshes the counters by reading from the BPF map and setting each
    /// counter metric to the current value.
    pub fn refresh(&mut self) {
        let bank_width = self.counter_map.bank_width();

        // borrow the BPF counters map so we can read per-cpu values
        let counters = self.counter_map.values();

        // One acquisition per refresh over the whole mmap read; member
        // values are set plain and the group's single window (stamped by
        // `finish()`, last) covers the whole sweep.
        let acq = self.group.acquire();

        for cpu in 0..possible_cpus() {
            for idx in 0..self.counters.len() {
                let value = counters[idx + cpu * bank_width];

                self.counters[idx].set(cpu, value);
            }
        }

        acq.finish();
    }
}

/// Represents a set of counters where the BPF map is a dense set of counters,
/// meaning there is no padding. No aggregation is performed, and the values are
/// read directly from the memory-mapped BPF map via `attach_external`.
pub struct PackedCounters<'a> {
    _map: &'a Map<'a>,
    _mmap: MmapMut,
}

impl<'a> PackedCounters<'a> {
    /// Create a new set of counters from the provided BPF map and collection of
    /// counter metrics.
    ///
    /// The map layout is not cacheline padded. The ordering of the dynamic
    /// counters must exactly match the layout in the BPF map.
    pub fn new(map: &'a Map, counters: &'static CounterGroup) -> Self {
        let total_bytes = counters.entries() * std::mem::size_of::<u64>();

        let fd = map.as_fd().as_raw_fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mmap: MmapMut = unsafe {
            MmapOptions::new()
                .len(total_bytes)
                .map_mut(&file)
                .expect("failed to mmap() bpf counterset")
        };

        let (_prefix, values, _suffix) = unsafe { mmap.align_to::<AtomicU64>() };

        if values.len() != counters.entries() {
            panic!("mmap region not aligned or width doesn't match");
        }

        // Attach the mmap directly to the counter group so the exposition code
        // can read values without an intermediate copy.
        //
        // SAFETY: The mmap is kept alive by self._mmap for the lifetime of
        // this struct (which is the process lifetime for BPF samplers).
        // AtomicU64 has the same layout as u64.
        unsafe {
            counters.attach_external(std::mem::transmute::<&[AtomicU64], &'static [AtomicU64]>(
                values,
            ));
        }

        Self {
            _map: map,
            _mmap: mmap,
        }
    }

    /// No-op: values are read directly from the mmap by the exposition code.
    /// Kept for API compatibility with the sampler refresh loop. These metrics
    /// fall through to the fleet window (level 4); read-section bracketing is
    /// deferred to the mmap-direct follow-on (level 3).
    pub fn refresh(&mut self) {}
}
