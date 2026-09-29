use super::*;
use crate::agent::{MAX_CPUS, MAX_FILESYSTEMS};

use libbpf_rs::{Map, MapCore, MapFlags};
use memmap2::{MmapMut, MmapOptions};
use metriken::{CounterGroup, LazyCounter};

use crate::agent::timing::AcquisitionGroup;
use metriken::group::SlotIdentity;

use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

/// This wraps the BPF map along with an opened memory-mapped region for the map
/// values.
struct CounterMap<'a> {
    _map: &'a Map<'a>,
    mmap: MmapMut,
    bank_width: usize,
}

impl<'a> CounterMap<'a> {
    /// Create a new `CounterMap` from the provided BPF map that holds the
    /// provided number of counters, one bank per CPU.
    pub fn new(map: &'a Map, counters: usize) -> Result<Self, ()> {
        Self::with_banks(map, counters, MAX_CPUS)
    }

    /// As [`new`](Self::new), with `banks` banks of `counters` counters: one
    /// per CPU, or one per (CPU, slot) pair for a slotted map.
    pub fn with_banks(map: &'a Map, counters: usize, banks: usize) -> Result<Self, ()> {
        // each bank of counters is the next nearest whole number of cachelines
        // wide
        let bank_cachelines = whole_cachelines::<u64>(counters);

        // the number of possible slots per bank of counters
        let bank_width = bank_cachelines * COUNTERS_PER_CACHELINE;

        // our total mapped region size in bytes
        let total_bytes = bank_cachelines * CACHELINE_SIZE * banks;

        let fd = map.as_fd().as_raw_fd();
        let file = unsafe { std::fs::File::from_raw_fd(fd as _) };
        let mmap: MmapMut = unsafe {
            MmapOptions::new()
                .len(total_bytes)
                .map_mut(&file)
                .map_err(|e| error!("failed to mmap() bpf counterset: {e}"))
        }?;

        let (_prefix, values, _suffix) = unsafe { mmap.align_to::<u64>() };

        if values.len() != banks * bank_width {
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

    /// Borrow the raw values mutably. Only a slot the BPF side has stopped
    /// writing (its device is not in the lookup map) may be written from here:
    /// the loads and stores are plain, and a concurrent BPF increment on the
    /// same word would be lost.
    fn values_mut(&mut self) -> &mut [u64] {
        let (_prefix, values, _suffix) = unsafe { self.mmap.align_to_mut::<u64>() };
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
///
/// The bracket is wider than any single value's actual read: `finish()`
/// only runs after every counter has been set (the stamp-last rule forces
/// this ordering — see [`AcquisitionGroup::acquire`]), so the window's
/// `end` is the time the *whole* sweep finished, not the time any one
/// counter's value was captured. That makes the reported width an upper
/// bound on the true acquisition time, never an underestimate — it can only
/// over-state how long the read took, exactly the safe direction for an
/// uncertainty window. Measured ~1.75× wider than the old per-entry windows
/// (12–37 µs → 21–65 µs), an accepted cost against a scrape interval
/// measured in tens or hundreds of milliseconds (journal proposal 2).
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
///
/// The bracket is wider than any single entry's actual read, for the same
/// reason as [`Counters`]: `finish()` only runs once every per-CPU entry has
/// been set (stamp-last forces this — see [`AcquisitionGroup::acquire`]),
/// so the window's `end` marks when the whole sweep finished, not when any
/// individual entry's value was captured. The reported width is therefore
/// an upper bound on the true per-entry acquisition time, never an
/// underestimate — over-stating the uncertainty is the safe direction.
/// Measured ~1.75× wider than the old per-entry windows this replaces
/// (12–37 µs → 21–65 µs), an accepted cost against a scrape interval
/// measured in tens or hundreds of milliseconds (journal proposal 2).
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

        // Boot-fixed membership: the CPU ids this host actually has, distinct
        // from each member `CounterGroup`'s `MAX_CPUS`-sized backing array (an
        // implementation ceiling — see docs/principles.md principle 6).
        //
        // An explicit SET, not a `0..possible_cpus()` bound. The possible mask
        // is what could be hot-added rather than what exists, and it is
        // `max_id + 1`, so a dense bound over it declares members this host
        // will never populate — 256 of them on a VM with `possible: 0-255,
        // present: 0-31`. Those slots are read from the zero-filled BPF mmap
        // and published as a real `0`, because a DECLARED group is exactly
        // where the snapshot builder skips the value-sentinel that would
        // otherwise have hidden them.
        group.set_member_set(&present_cpus());

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
///
/// # Windowing
///
/// Unlike [`Counters`]/[`CpuCounters`], there is no `refresh()`-time read to
/// bracket: values are read directly from the attached mmap by the
/// exposition code (`create`/`create_v3`), not copied out by a sampler task.
/// The acquisition IS that exposition read, so `new()` marks the group
/// [reader-stamped](crate::agent::timing::AcquisitionGroup::set_reader_stamped)
/// instead of ever calling `acquire()`/`finish()` itself — the snapshot
/// builder brackets the group's window at exposition time (walk-spanning:
/// acquire at the group's first member touch, finish once its last
/// member's values have been read), documented on `create`/`create_v3`.
pub struct PackedCounters<'a> {
    _map: &'a Map<'a>,
    _mmap: MmapMut,
}

impl<'a> PackedCounters<'a> {
    /// Create a new set of counters from the provided BPF map and collection of
    /// counter metrics. `group` is the declared [`AcquisitionGroup`] this
    /// map's values belong to — marked reader-stamped here (idempotent: two
    /// `PackedCounters` sharing one like-entities group, e.g. `cgroup_syscall`'s
    /// 16 op-class maps, both mark the same group harmlessly).
    ///
    /// The map layout is not cacheline padded. The ordering of the dynamic
    /// counters must exactly match the layout in the BPF map.
    pub fn new(
        map: &'a Map,
        counters: &'static CounterGroup,
        group: &'static AcquisitionGroup,
    ) -> Self {
        group.set_reader_stamped();

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
    /// Kept for API compatibility with the sampler refresh loop. This group
    /// is reader-stamped (see the struct-level doc comment) — its window is
    /// bracketed by `create`/`create_v3` at exposition time, not here.
    pub fn refresh(&mut self) {}
}

/// The index of counter `idx` in CPU `cpu`'s bank for filesystem `slot`, in a
/// map laid out as `MAX_CPUS × MAX_FILESYSTEMS` banks of `bank_width`: every
/// CPU owns a contiguous run of one bank per slot, so two CPUs never share a
/// cacheline whatever slot they hit, and the BPF side computes the same
/// index as `(cpu * MAX_FILESYSTEMS + slot) * COUNTER_GROUP_WIDTH + counter`
/// (`bpf/filesystem.h`).
pub(crate) fn filesystem_index(cpu: usize, slot: usize, idx: usize, bank_width: usize) -> usize {
    (cpu * MAX_FILESYSTEMS + slot) * bank_width + idx
}

/// Per-filesystem totals from per-CPU banks: `Counters`' layout with a slot
/// dimension. The BPF program indexes by `(cpu, slot)` where the slot comes
/// from a `dev_t` lookup map this reader keeps in step with the mount table
/// (see `bpf/filesystems.rs`); the refresh sums each slot over the CPUs and
/// publishes it as one `CounterGroup` entry, labeled with the mount.
///
/// Padding per (CPU, slot) rather than one shared counter per slot is
/// principle 7's call: a filesystem is written from every CPU (fsync,
/// allocation), so it is `Counters`' writer pattern, not `PackedCounters`'
/// one-writer-per-slot. The map costs `MAX_CPUS × MAX_FILESYSTEMS × bank`
/// bytes, allocated eagerly by the kernel: 8 MiB for `ext4_journal`'s 16-wide
/// bank, 12 MiB for `ext4_alloc`'s 24-wide one. The refresh reads only
/// possible CPUs × occupied slots.
///
/// # Windowing
///
/// Same stamp-last discipline as [`Counters`]: one acquisition brackets the
/// whole sweep, members are set plain, `finish()` last. The population is a
/// bound, one past the highest occupied slot, revised whenever the assignment
/// changes; a vacant slot under it reads absent.
pub struct FilesystemCounters<'a> {
    counter_map: CounterMap<'a>,
    lookup: &'a Map<'a>,
    counters: Vec<&'static CounterGroup>,
    group: &'static AcquisitionGroup,
    identity: &'static SlotIdentity,
    applied: Arc<super::filesystems::Assignment>,
    /// Slot 0's sums at the last refresh. Movement means an unknown device is
    /// producing events, and asks for a mount-table rescan.
    other: Vec<u64>,
}

impl<'a> FilesystemCounters<'a> {
    pub fn new(
        map: &'a Map,
        lookup: &'a Map,
        counters: Vec<&'static CounterGroup>,
        group: &'static AcquisitionGroup,
        identity: &'static SlotIdentity,
    ) -> Self {
        let counter_map = CounterMap::with_banks(map, counters.len(), MAX_CPUS * MAX_FILESYSTEMS)
            .expect("failed to initialize");
        let other = vec![0; counters.len()];

        let mut this = Self {
            counter_map,
            lookup,
            counters,
            group,
            identity,
            applied: Arc::new(super::filesystems::Assignment::default()),
            other,
        };

        // Slot 0 exists from the start, whatever the mount table says. The
        // population is declared as a BOUND, not a member set: the set is
        // write-once (`AcquisitionGroup::set_member_set`) and this population
        // changes with every mount and unmount. Principle 18 allows a changing
        // population to revise its bound each read, as the `filesystem`
        // sampler does. A vacant slot under the bound reads absent, not 0:
        // these groups are owned (not mmap-attached) and `apply` writes the
        // never-written sentinel when a slot empties.
        this.group.set_member_bound(1);
        this.identity.assign(
            0,
            [("mount".to_string(), super::filesystems::OTHER.to_string())]
                .into_iter()
                .collect(),
        );
        this.apply(super::filesystems::current());
        this
    }

    /// Bring the lookup map, the identities and the member bound in line with
    /// `assignment`. A slot changing hands is zeroed while no device maps to
    /// it, so the next occupant starts from zero.
    fn apply(&mut self, assignment: Arc<super::filesystems::Assignment>) {
        if assignment.generation == self.applied.generation {
            return;
        }
        for slot in 1..MAX_FILESYSTEMS {
            let before = self.applied.slots.get(slot).cloned().flatten();
            let after = assignment.slots.get(slot).cloned().flatten();
            if before == after {
                continue;
            }
            let same_device = matches!((&before, &after), (Some(b), Some(a)) if b.dev == a.dev);
            if let Some(old) = &before {
                if !same_device {
                    if let Err(e) = self.lookup.delete(&old.dev.to_ne_bytes()) {
                        debug!("filesystem slots: delete {} from lookup: {e}", old.devnum);
                    }
                    self.identity.release(slot);
                    self.zero(slot);
                    for counter in &self.counters {
                        // u64::MAX is CounterGroup's never-written sentinel.
                        counter.set(slot, u64::MAX);
                    }
                }
            }
            if let Some(new) = &after {
                if !same_device {
                    self.zero(slot);
                    if let Err(e) = self.lookup.update(
                        &new.dev.to_ne_bytes(),
                        &(slot as u32).to_ne_bytes(),
                        MapFlags::ANY,
                    ) {
                        warn!(
                            "filesystem slots: could not map {} ({}) to slot {slot}: {e}",
                            new.mount, new.devnum
                        );
                    }
                }
                // A relabel (same device, moved mount point) is a set too.
                self.identity.assign(slot, new.labels());
            }
        }
        // Stored before the next `finish()`, as the bound contract requires.
        self.group.set_member_bound(assignment.bound());
        self.applied = assignment;
    }

    /// Zero every CPU's bank for `slot`. Only called while no device maps to
    /// the slot, so no BPF increment races the stores (see `values_mut`).
    fn zero(&mut self, slot: usize) {
        let bank_width = self.counter_map.bank_width();
        let width = self.counters.len();
        let values = self.counter_map.values_mut();
        for cpu in 0..MAX_CPUS {
            let base = filesystem_index(cpu, slot, 0, bank_width);
            values[base..base + width].fill(0);
        }
    }

    /// Sum each occupied slot over the CPUs and publish. Slot 0 moving since
    /// the last refresh asks the registry for a rescan, so a device the table
    /// does not know yet gets its slot on the refresh after next (this one
    /// notices, the next one's `current()` rescans and applies). Slot 0 always
    /// moves a little at startup: the programs attach before `new()` fills the
    /// lookup map, so the first events land there.
    pub fn refresh(&mut self) {
        self.apply(super::filesystems::current());

        let bank_width = self.counter_map.bank_width();
        let width = self.counters.len();
        let members = self.applied.members();
        let mut sums = vec![0u64; width];

        let acq = self.group.acquire();
        let mut other_moved = false;

        for &slot in &members {
            sums.fill(0);
            {
                let counters = self.counter_map.values();
                for cpu in 0..possible_cpus() {
                    let base = filesystem_index(cpu, slot, 0, bank_width);
                    for (sum, value) in sums.iter_mut().zip(&counters[base..base + width]) {
                        *sum = sum.wrapping_add(*value);
                    }
                }
            }
            for (counter, sum) in self.counters.iter().zip(&sums) {
                counter.set(slot, *sum);
            }
            if slot == 0 && self.other != sums {
                other_moved = true;
                self.other.copy_from_slice(&sums);
            }
        }

        acq.finish();

        if other_moved {
            super::filesystems::request_rescan();
        }
    }
}

#[cfg(test)]
mod filesystem_layout_tests {
    use super::*;

    /// The BPF side computes `(cpu * MAX_FILESYSTEMS + slot) * width + counter`;
    /// the reader must agree, and two CPUs must never share a bank.
    #[test]
    fn filesystem_index_matches_the_bpf_layout_and_separates_cpus() {
        let width = 16;
        assert_eq!(filesystem_index(0, 0, 0, width), 0);
        assert_eq!(filesystem_index(0, 1, 0, width), width);
        assert_eq!(filesystem_index(0, 1, 5, width), width + 5);
        assert_eq!(filesystem_index(1, 0, 0, width), MAX_FILESYSTEMS * width);
        assert_eq!(
            filesystem_index(3, 7, 2, width),
            (3 * MAX_FILESYSTEMS + 7) * width + 2
        );
        // A bank is a whole number of cachelines, so CPU banks never share one.
        assert_eq!((MAX_FILESYSTEMS * width * COUNTER_SIZE) % CACHELINE_SIZE, 0);
    }
}
