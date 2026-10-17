//! Sizes of slab caches from `/proc/slabinfo`: the bytes every cache holds,
//! labeled by cache name, and object counts for a fixed set of filesystem
//! metadata caches (the dentry and inode caches, ext4's and XFS's inode caches,
//! ext4's extent-status cache, jbd2 journal heads, buffer heads and the
//! page-cache index nodes).
//!
//! Per-cache bytes attribute slab growth. `memory_slab` (meminfo's
//! `SReclaimable` and `SUnreclaim`) says that slab memory grew;
//! `memory_slab_cache_bytes` says which cache holds it. With `caches = "all"`
//! (the default) every cache is published, so a cache that grows without
//! bound shows by name whichever subsystem owns it. With `caches = "fixed"`
//! only the followed caches' bytes are published.
//!
//! The followed caches answer "does the metadata cache fit". A filesystem
//! with tens of millions of files whose inode cache is evicted pays a
//! synchronous inode-table read on `stat` and on every atime update
//! (`ext4_inode_loads` in `ext4_alloc` is that rate); `ext4_inode_cache` here
//! is the cause on the same axis. `vm.vfs_cache_pressure` is the knob that
//! trades this memory against the page cache.
//!
//! `/proc/slabinfo` is root-only, lists every cache on the machine (a few
//! hundred lines), and the kernel takes the slab lock to generate it, so it is
//! a principle 15 exception: read at most once per `interval` (60 s by
//! default), off the scrape cycle on the blocking pool, following the
//! `filesystem` and `drivehealth` samplers. The refresh path pays a time check
//! and, once per interval, a dispatch. Each sweep logs at debug level
//! `memory_slabinfo sweep: read <us> us (<bytes> bytes), parse and publish
//! <us> us, <n> caches published, <n> over the cap`.
//!
//! A cache's bytes are slabs × pages per slab × the page size. The kernel
//! prints pages per slab at the cache's preferred order (`s->oo`), but under
//! memory pressure SLUB allocates a slab at the smaller minimum order
//! (`s->min`, `allocate_slab` in `mm/slub.c`), so the product is an upper
//! bound on the cache's pages, exact while no slab used the fallback.
//!
//! # Slots
//!
//! Each published cache holds one slot, shared by `memory_slab_cache_bytes`
//! and both `memory_slab_cache_objects` families, while the file lists it,
//! keyed by name. A cache that appears between sweeps (a module loaded: `kvm`,
//! `zfs`) takes a free slot and is labeled then; one that disappears (a module
//! unloaded, a cache destroyed) has its values unset in every family and its
//! labels released, and the slot is reused. A cache that comes back can land
//! on a different slot, so its `id` label can change. Slot assignment and the
//! group's member bound follow the `filesystem` sampler (principle 18's
//! changing population). At most [`MAX_CACHES`] are published; when more are
//! listed, the followed caches are placed first and the rest by size, largest
//! first, and a slot once held is kept until its cache goes, so a followed
//! cache that first appears while every slot is held is left out. The first
//! cache left out is a warning; changes in the count are logged at debug, and
//! fitting again at info.
//!
//! # What the file does not show
//!
//! A cache the running kernel does not list (`xfs_inode` without XFS loaded,
//! `ext4_*` without ext4) has no series. Neither does a cache SLUB has merged: a cache
//! without a constructor or a usercopy region can share a pool with others of
//! its size and flags, and the file then lists the pool under one name only,
//! with the pool's totals. `ext4_extent_status` (40 bytes, no constructor) is
//! merged on the Debian 13 6.12 kernel and is absent there; `jbd2_journal_head`
//! was not, and the inode and dentry caches have constructors and are never
//! merged. A merged pool's members are the names in `/sys/kernel/slab/` that
//! link to one directory. `slab_nomerge` on the kernel command line lists
//! every cache under its own name.
//!
//! On SLUB, and on SLAB from 6.1, `kmalloc` requests larger than two pages
//! (`kmalloc-8k` with 4 KiB pages) go straight to the page allocator. Since
//! Linux 5.4 (when `kmalloc_order` in `mm/slab_common.c` gained the
//! accounting), below the agent's 5.8 minimum, those pages are counted in
//! `SUnreclaim` and are in no cache. A `CONFIG_SLAB` kernel before 6.1 keeps
//! kmalloc caches up to 4 MiB with 4 KiB pages (2^(MAX_ORDER + PAGE_SHIFT − 1),
//! at most 32 MiB), so there the difference below is about zero. With every
//! cache published, `memory_slab` minus the sum of
//! `memory_slab_cache_bytes` approximates the memory in large `kmalloc`
//! allocations; since the cache bytes are an upper bound, the difference can
//! understate it.
//!
//! OpenZFS's SPL creates a cache whose objects are at most
//! `spl_kmem_cache_slab_limit` bytes (`SPL_MAX_KMEM_ORDER_NR_PAGES` pages,
//! which is 4 under SLUB, so 16 KiB with 4 KiB pages) as a Linux slab cache
//! under the same name, so it is listed here. A cache with larger objects (the
//! larger `zio_buf_*`, `zio_data_buf_*` and `zio_buf_comb_*`, the last when
//! ZFS gives data and metadata one cache) is SPL's own, built on `vmalloc`: it is not in this file and not in
//! `memory_slab`, and only `/proc/spl/kmem/slab` lists it. ARC data held in
//! scatter ABDs (the default, `zfs_abd_scatter_enabled=1`) comes from the
//! page allocator and is in neither file.

const NAME: &str = "memory_slabinfo";

const SLABINFO: &str = "/proc/slabinfo";

use crate::agent::samplers::{count_change, CountChange};
use crate::agent::*;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod stats;

use stats::*;

/// Built-in sweep cadence when `[samplers.memory_slabinfo] interval` is unset.
const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(60);

/// Which cache a `memory_slab_cache_bytes` slot holds. A slot is reused when
/// its cache goes, so its labels are written per occupant, not once.
static CACHE_IDENTITY: metriken::group::SlotIdentity = metriken::group::SlotIdentity::new(&[
    &SLAB_CACHE_BYTES,
    &SLAB_CACHE_OBJECTS_ACTIVE,
    &SLAB_CACHE_OBJECTS_TOTAL,
]);

/// Every family keyed by a cache slot.
static GROUPS: &[&metriken::GaugeGroup] = &[
    &SLAB_CACHE_BYTES,
    &SLAB_CACHE_OBJECTS_ACTIVE,
    &SLAB_CACHE_OBJECTS_TOTAL,
];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Fail at init, not on the first sweep, when the file cannot be read: an
    // unprivileged agent gets EACCES here and `rezolus status` says so.
    std::fs::read_to_string(SLABINFO)?;

    let interval = config
        .sampler_interval(NAME)
        .unwrap_or(DEFAULT_READ_INTERVAL);
    let all = config.all_caches(NAME);

    debug!(
        "{NAME}: sweeping every {interval:?}, publishing {} caches",
        if all { "all" } else { "the fixed set of" }
    );

    // No member until the first sweep lands.
    SLABINFO_ACQ.set_member_bound(0);

    Ok(Some(Box::new(Slabinfo {
        interval,
        last_read: Mutex::new(None),
        reading: Arc::new(AtomicBool::new(false)),
        state: Arc::new(Mutex::new(SweepState::new(all, page_size()))),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

fn page_size() -> u64 {
    // SAFETY: sysconf has no preconditions.
    match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } {
        n if n > 0 => n as u64,
        _ => 4096,
    }
}

struct Slabinfo {
    /// Minimum spacing between sweeps.
    interval: Duration,
    /// When the last sweep was dispatched; `None` until the first.
    last_read: Mutex<Option<Instant>>,
    /// True while a sweep is in flight, so sweeps never overlap.
    reading: Arc<AtomicBool>,
    state: Arc<Mutex<SweepState>>,
}

/// One slot per published cache name while the file lists it; freed slots
/// are reused.
struct Slots {
    by_name: HashMap<String, usize>,
    occupied: Vec<Option<String>>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Assignment {
    /// Slots whose cache is no longer listed.
    freed: Vec<usize>,
    /// Caches listed but not placed, for want of a slot.
    dropped: usize,
}

impl Slots {
    fn new(capacity: usize) -> Self {
        Self {
            by_name: HashMap::new(),
            occupied: vec![None; capacity],
        }
    }

    /// Free the slots of caches not in `names`, then place the new names in
    /// order while slots remain. A name already placed keeps its slot.
    fn assign(&mut self, names: &[&str]) -> Assignment {
        let mut outcome = Assignment::default();

        for (slot, held) in self.occupied.iter_mut().enumerate() {
            if let Some(name) = held {
                if !names.contains(&name.as_str()) {
                    self.by_name.remove(name);
                    *held = None;
                    outcome.freed.push(slot);
                }
            }
        }

        for name in names {
            if self.by_name.contains_key(*name) {
                continue;
            }
            match self.occupied.iter().position(Option::is_none) {
                Some(slot) => {
                    self.occupied[slot] = Some(name.to_string());
                    self.by_name.insert(name.to_string(), slot);
                }
                None => outcome.dropped += 1,
            }
        }

        outcome
    }

    fn slot_of(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }

    /// One past the highest occupied slot.
    fn bound(&self) -> usize {
        self.occupied
            .iter()
            .rposition(Option::is_some)
            .map_or(0, |slot| slot + 1)
    }
}

/// What persists between sweeps.
struct SweepState {
    /// Publish every cache (`caches = "all"`) or only the followed ones.
    all: bool,
    page_size: u64,
    slots: Slots,
    /// Caches over the cap at the last sweep, so its warning fires on change.
    dropped: usize,
}

impl SweepState {
    fn new(all: bool, page_size: u64) -> Self {
        Self {
            all,
            page_size,
            slots: Slots::new(MAX_CACHES),
            dropped: 0,
        }
    }
}

/// Clears the in-flight latch when the sweep ends, by return or by unwind.
struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// The slot reads absent and carries no labels, ready for another cache.
fn vacate(slot: usize) {
    for group in GROUPS {
        // i64::MIN is GaugeGroup's never-written sentinel, read as absent.
        let _ = group.set(slot, i64::MIN);
    }
    CACHE_IDENTITY.release(slot);
}

/// Lock the sweep state, starting it afresh if a panicking sweep poisoned it:
/// a slot map abandoned mid-update cannot be trusted.
fn lock_state(state: &Mutex<SweepState>) -> std::sync::MutexGuard<'_, SweepState> {
    state.lock().unwrap_or_else(|poisoned| {
        warn!("{NAME}: a previous sweep panicked; restarting slot assignment");
        state.clear_poison();
        // The abandoned map may not know which slots hold readings.
        for slot in 0..MAX_CACHES {
            vacate(slot);
        }
        let mut guard = poisoned.into_inner();
        *guard = SweepState::new(guard.all, guard.page_size);
        guard
    })
}

#[async_trait]
impl Sampler for Slabinfo {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        // Throttle: dispatch a sweep at most once per `interval`. Cheap time
        // check on the scrape path.
        {
            let mut last = self.last_read.lock().unwrap();
            match *last {
                Some(t) if t.elapsed() < self.interval => return,
                _ => *last = Some(Instant::now()),
            }
        }

        // Never overlap sweeps.
        if self.reading.swap(true, Ordering::AcqRel) {
            return;
        }

        let reading = self.reading.clone();
        let state = self.state.clone();

        // Off the async worker: the read holds the slab lock in the kernel and
        // the parse walks a few hundred lines. This task is `SLABINFO_ACQ`'s
        // single writer (principle 18); `refresh()` only dispatches it.
        tokio::task::spawn_blocking(move || {
            let _in_flight = InFlight(reading);
            sweep(&mut lock_state(&state), SLABINFO);
        });
    }
}

/// One read of the file at `path` and one parse, bracketed as one
/// acquisition. Returns how many caches were published. A failed read, or a
/// file naming no cache this sampler publishes, discards: the previous window
/// stands and the gauges keep their values.
fn sweep(state: &mut SweepState, path: &str) -> usize {
    let guard = SLABINFO_ACQ.acquire();
    let started = Instant::now();

    let data = match std::fs::read_to_string(path) {
        Ok(data) => data,
        Err(e) => {
            warn!("{NAME}: reading {path} failed: {e}");
            // The last known population: a failed first read must not leave
            // the group walking its full capacity.
            SLABINFO_ACQ.set_member_bound(state.slots.bound());
            guard.discard();
            return 0;
        }
    };
    let read = started.elapsed();

    let published = publish(state, &data);
    let parsed = started.elapsed() - read;

    // The sweep is the sole writer; the bound must be stored before finish().
    SLABINFO_ACQ.set_member_bound(state.slots.bound());

    debug!(
        "{NAME} sweep: read {} us ({} bytes), parse and publish {} us, {} caches published, \
         {} over the cap",
        read.as_micros(),
        data.len(),
        parsed.as_micros(),
        published,
        state.dropped,
    );

    if published > 0 {
        guard.finish();
    } else {
        guard.discard();
    }
    published
}

/// A parsed `/proc/slabinfo` line for one cache.
#[derive(Debug, PartialEq, Eq)]
struct Line {
    active_objects: u64,
    total_objects: u64,
    /// slabs × pages per slab × page size
    bytes: u64,
}

/// Parse one data line. The format (`slabinfo - version: 2.1`):
/// `name active_objs num_objs objsize objperslab pagesperslab : tunables
/// limit batchcount sharedfactor : slabdata active_slabs num_slabs sharedavail`.
fn parse_line(line: &str, page_size: u64) -> Option<(&str, Line)> {
    let mut parts = line.split_whitespace();
    let name = parts.next()?;
    if name.starts_with('#') || name == "slabinfo" {
        return None;
    }
    let active_objects: u64 = parts.next()?.parse().ok()?;
    let total_objects: u64 = parts.next()?.parse().ok()?;
    let _objsize: u64 = parts.next()?.parse().ok()?;
    let _objperslab: u64 = parts.next()?.parse().ok()?;
    let pagesperslab: u64 = parts.next()?.parse().ok()?;

    // Skip to the `slabdata` section: `: tunables a b c : slabdata x y z`.
    let mut after_slabdata = parts.skip_while(|p| *p != "slabdata");
    after_slabdata.next()?; // "slabdata"
    let _active_slabs: u64 = after_slabdata.next()?.parse().ok()?;
    let num_slabs: u64 = after_slabdata.next()?.parse().ok()?;

    Some((
        name,
        Line {
            active_objects,
            total_objects,
            bytes: num_slabs
                .saturating_mul(pagesperslab)
                .saturating_mul(page_size),
        },
    ))
}

fn followed(name: &str) -> bool {
    FOLLOWED.contains(&name)
}

fn gauge(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Set every published cache's bytes, and the followed caches' object counts,
/// from the text of one read; return how many caches were published. A cache
/// the text no longer lists reads absent in every family.
fn publish(state: &mut SweepState, data: &str) -> usize {
    let lines: Vec<(&str, Line)> = data
        .lines()
        .filter_map(|line| parse_line(line, state.page_size))
        .collect();

    // Placement order when slots run short: followed caches, then by size.
    let mut published: Vec<&(&str, Line)> = lines
        .iter()
        .filter(|(name, _)| state.all || followed(name))
        .collect();
    published.sort_by(|(a, x), (b, y)| followed(b).cmp(&followed(a)).then(y.bytes.cmp(&x.bytes)));
    // A name listed twice is published once, at its larger line: the two
    // would share one slot.
    let mut seen = HashSet::new();
    published.retain(|(name, _)| seen.insert(*name));
    let names: Vec<&str> = published.iter().map(|(name, _)| *name).collect();

    // A read naming no cache this sampler publishes (an empty file, or one cut
    // off before the first cache line) changes nothing; the sweep discards it.
    if names.is_empty() {
        return 0;
    }

    let assignment = state.slots.assign(&names);
    // Must clear a freed slot before another cache's labels go on it.
    for slot in &assignment.freed {
        vacate(*slot);
    }
    match count_change(state.dropped, assignment.dropped) {
        Some(CountChange::Began(n)) => {
            warn!("{NAME}: {n} slab cache(s) beyond the {MAX_CACHES}-cache cap are not published")
        }
        Some(CountChange::Moved(n)) => {
            debug!("{NAME}: {n} slab cache(s) beyond the {MAX_CACHES}-cache cap are not published")
        }
        Some(CountChange::Ended) => {
            info!("{NAME}: every slab cache fits within the {MAX_CACHES}-cache cap again")
        }
        None => {}
    }
    state.dropped = assignment.dropped;

    let mut count = 0;
    for (name, line) in published {
        let Some(slot) = state.slots.slot_of(name) else {
            continue;
        };
        // A no-op when the slot already holds this cache.
        CACHE_IDENTITY.assign(
            slot,
            BTreeMap::from([("cache".to_string(), name.to_string())]),
        );
        let _ = SLAB_CACHE_BYTES.set(slot, gauge(line.bytes));
        if followed(name) {
            let _ = SLAB_CACHE_OBJECTS_ACTIVE.set(slot, gauge(line.active_objects));
            let _ = SLAB_CACHE_OBJECTS_TOTAL.set(slot, gauge(line.total_objects));
        }
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that write the process-wide gauges, slot labels or
    /// acquisition group.
    static GLOBALS: Mutex<()> = Mutex::new(());

    fn globals() -> std::sync::MutexGuard<'static, ()> {
        GLOBALS.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A first sweep: `/proc/slabinfo` lines copied from a 7.0.14 kernel
    /// (Docker Desktop's arm64 VM, 4 KiB pages), cut to the followed caches
    /// and a few others. `xfs_inode` is listed with no slabs.
    const FIRST: &str = "slabinfo - version: 2.1
# name            <active_objs> <num_objs> <objsize> <objperslab> <pagesperslab> : tunables <limit> <batchcount> <sharedfactor> : slabdata <active_slabs> <num_slabs> <sharedavail>
xfs_inode              0      0   1024   32    8 : tunables    0    0    0 : slabdata      0      0      0
fuse_inode          1036   1044    896   36    8 : tunables    0    0    0 : slabdata     29     29      0
jbd2_journal_head   1080   2992    120  136    4 : tunables    0    0    0 : slabdata     22     22      0
ext4_inode_cache   11616  33205   1128   29    8 : tunables    0    0    0 : slabdata   1145   1145      0
buffer_head       325980 326560    104  157    4 : tunables    0    0    0 : slabdata   2080   2080      0
inode_cache          924   1066    616   26    4 : tunables    0    0    0 : slabdata     41     41      0
dentry             21900  49895    192   85    4 : tunables    0    0    0 : slabdata    587    587      0
radix_tree_node    34132  34132    584   28    4 : tunables    0    0    0 : slabdata   1219   1219      0
kmalloc-cg-4k        224    224   4096    8    8 : tunables    0    0    0 : slabdata     28     28      0
kmalloc-8k           136    156   8192    4    8 : tunables    0    0    0 : slabdata     39     39      0
kmalloc-64          4353   4864     64  256    4 : tunables    0    0    0 : slabdata     19     19      0
";

    /// The next sweep: `kvm` and `zfs` loaded (three caches appear, written in
    /// the kernel's `cache_show` format and widths), XFS unloaded
    /// (`xfs_inode` gone), and `kmalloc-64` grown.
    const SECOND: &str = "slabinfo - version: 2.1
# name            <active_objs> <num_objs> <objsize> <objperslab> <pagesperslab> : tunables <limit> <batchcount> <sharedfactor> : slabdata <active_slabs> <num_slabs> <sharedavail>
kvm_vcpu              48     48   9792    3    8 : tunables    0    0    0 : slabdata     16     16      0
kvm_mmu_page_header   1820   1836    184   22    1 : tunables    0    0    0 : slabdata     83     83      0
zio_buf_comb_512    2048   2048    512   32    4 : tunables    0    0    0 : slabdata     64     64      0
fuse_inode          1036   1044    896   36    8 : tunables    0    0    0 : slabdata     29     29      0
jbd2_journal_head   1080   2992    120  136    4 : tunables    0    0    0 : slabdata     22     22      0
ext4_inode_cache   11616  33205   1128   29    8 : tunables    0    0    0 : slabdata   1145   1145      0
buffer_head       325980 326560    104  157    4 : tunables    0    0    0 : slabdata   2080   2080      0
inode_cache          924   1066    616   26    4 : tunables    0    0    0 : slabdata     41     41      0
dentry             21900  49895    192   85    4 : tunables    0    0    0 : slabdata    587    587      0
radix_tree_node    34132  34132    584   28    4 : tunables    0    0    0 : slabdata   1219   1219      0
kmalloc-cg-4k        224    224   4096    8    8 : tunables    0    0    0 : slabdata     28     28      0
kmalloc-8k           136    156   8192    4    8 : tunables    0    0    0 : slabdata     39     39      0
kmalloc-64        2400000 2400000     64  256    4 : tunables    0    0    0 : slabdata   9375   9375      0
";

    fn bytes(state: &SweepState, cache: &str) -> Option<i64> {
        SLAB_CACHE_BYTES.value(state.slots.slot_of(cache)?)
    }

    fn objects(state: &SweepState, cache: &str) -> (Option<i64>, Option<i64>) {
        let slot = state.slots.slot_of(cache);
        (
            slot.and_then(|s| SLAB_CACHE_OBJECTS_ACTIVE.value(s)),
            slot.and_then(|s| SLAB_CACHE_OBJECTS_TOTAL.value(s)),
        )
    }

    fn label(slot: usize) -> Option<String> {
        SLAB_CACHE_BYTES
            .load_metadata(slot)
            .and_then(|m| m.get("cache").cloned())
    }

    fn fixture(dir: &tempfile::TempDir, name: &str, text: &str) -> String {
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn a_cache_line_parses_to_objects_and_bytes() {
        let (name, line) = parse_line(
            "ext4_inode_cache  1234567 1300000   1176   27    8 : tunables    0    0    0 : slabdata  48148  48148      0",
            4096,
        )
        .expect("a data line parses");
        assert_eq!(name, "ext4_inode_cache");
        assert_eq!(line.active_objects, 1234567);
        assert_eq!(line.total_objects, 1300000);
        // 48148 slabs of 8 pages of 4 KiB
        assert_eq!(line.bytes, 48148 * 8 * 4096);
    }

    #[test]
    fn header_lines_are_skipped() {
        assert!(parse_line("slabinfo - version: 2.1", 4096).is_none());
        assert!(parse_line("# name <active_objs> <num_objs>", 4096).is_none());
        assert!(parse_line("", 4096).is_none());
    }

    #[test]
    fn every_cache_is_published_by_name() {
        let _g = globals();
        let mut state = SweepState::new(true, 4096);
        assert_eq!(publish(&mut state, FIRST), 11);
        assert_eq!(bytes(&state, "kmalloc-64"), Some(19 * 4 * 4096));
        assert_eq!(bytes(&state, "kmalloc-cg-4k"), Some(28 * 8 * 4096));
        assert_eq!(bytes(&state, "dentry"), Some(587 * 4 * 4096));
        assert_eq!(bytes(&state, "xfs_inode"), Some(0), "listed, so present");
        let slot = state.slots.slot_of("kmalloc-8k").unwrap();
        assert_eq!(label(slot).as_deref(), Some("kmalloc-8k"));
        assert_eq!(state.slots.bound(), 11);
        // The followed caches' object counts come from the same read.
        assert_eq!(
            objects(&state, "ext4_inode_cache"),
            (Some(11616), Some(33205))
        );
        assert_eq!(objects(&state, "dentry"), (Some(21900), Some(49895)));
        // Object counts are for the followed caches only.
        assert_eq!(objects(&state, "kmalloc-64"), (None, None));
        let labels = SLAB_CACHE_OBJECTS_ACTIVE.load_metadata(slot).unwrap();
        assert_eq!(labels.get("cache").map(String::as_str), Some("kmalloc-8k"));
    }

    #[test]
    fn the_fixed_set_publishes_only_the_followed_caches() {
        let _g = globals();
        let mut state = SweepState::new(false, 4096);
        // Seven of the eight followed caches are in the file (no
        // ext4_extent_status).
        assert_eq!(publish(&mut state, FIRST), 7);
        assert_eq!(bytes(&state, "kmalloc-64"), None);
        assert_eq!(state.slots.slot_of("kmalloc-64"), None);
        assert_eq!(bytes(&state, "radix_tree_node"), Some(1219 * 4 * 4096));
        assert_eq!(state.slots.bound(), 7);
    }

    /// A module loading adds caches between sweeps and one unloading removes
    /// them: new caches take free slots with their own labels, a cache that
    /// stays keeps its slot and occupant, and a cache that went reads absent
    /// with no labels until its slot is reused.
    #[test]
    fn caches_that_appear_and_disappear_between_sweeps_take_and_release_slots() {
        let _g = globals();
        let mut state = SweepState::new(true, 4096);
        publish(&mut state, FIRST);
        let xfs = state.slots.slot_of("xfs_inode").unwrap();
        let kmalloc = state.slots.slot_of("kmalloc-64").unwrap();
        let kmalloc_uid = CACHE_IDENTITY.uid(kmalloc);
        assert_eq!(state.slots.slot_of("kvm_vcpu"), None);

        assert_eq!(publish(&mut state, SECOND), 13);

        assert_eq!(state.slots.slot_of("kmalloc-64"), Some(kmalloc));
        assert_eq!(CACHE_IDENTITY.uid(kmalloc), kmalloc_uid, "same occupant");
        assert_eq!(bytes(&state, "kmalloc-64"), Some(9375 * 4 * 4096));

        assert_eq!(state.slots.slot_of("xfs_inode"), None);
        for cache in ["kvm_vcpu", "kvm_mmu_page_header", "zio_buf_comb_512"] {
            let slot = state.slots.slot_of(cache).expect("a new cache gets a slot");
            assert_eq!(label(slot).as_deref(), Some(cache));
            assert!(SLAB_CACHE_BYTES.value(slot).is_some());
        }
        // xfs_inode's slot went to the largest new cache.
        assert_eq!(label(xfs).as_deref(), Some("zio_buf_comb_512"));
        assert_eq!(state.slots.bound(), 13);

        // Back to the first file: the module caches go and read absent.
        assert_eq!(publish(&mut state, FIRST), 11);
        for cache in ["kvm_vcpu", "kvm_mmu_page_header", "zio_buf_comb_512"] {
            assert_eq!(state.slots.slot_of(cache), None);
        }
        let freed: Vec<usize> = (0..13)
            .filter(|slot| state.slots.occupied[*slot].is_none())
            .collect();
        assert_eq!(freed.len(), 2, "xfs_inode retook one of the three");
        for slot in freed {
            assert_eq!(SLAB_CACHE_BYTES.value(slot), None, "absent, not zero");
            assert_eq!(label(slot), None, "labels released");
        }
    }

    #[test]
    fn caches_past_the_cap_are_dropped_and_the_followed_caches_placed_first() {
        let mut slots = Slots::new(2);
        let outcome = slots.assign(&["dentry", "kmalloc-64", "kmalloc-8k"]);
        assert_eq!(outcome.dropped, 1);
        assert_eq!(slots.slot_of("kmalloc-8k"), None);
        assert_eq!(slots.bound(), 2);

        let _g = globals();
        let mut state = SweepState::new(true, 4096);
        state.slots = Slots::new(3);
        publish(&mut state, FIRST);
        // Placement is followed caches by size, then the rest by size; the
        // three largest followed caches win.
        let placed: Vec<&str> = (0..3)
            .map(|slot| state.slots.occupied[slot].as_deref().unwrap())
            .collect();
        assert_eq!(
            placed,
            vec!["ext4_inode_cache", "buffer_head", "radix_tree_node"]
        );
        assert_eq!(state.dropped, 8);
    }

    /// A file that parses to no cache (header only) leaves every published
    /// cache as it was, and the sweep discards.
    #[test]
    fn a_file_naming_no_cache_changes_nothing() {
        let _g = globals();
        let dir = tempfile::tempdir().unwrap();
        let mut state = SweepState::new(true, 4096);
        publish(&mut state, FIRST);
        let before = bytes(&state, "dentry");
        let slot = state.slots.slot_of("dentry").unwrap();
        let header_only = "slabinfo - version: 2.1\n# name <active_objs>\n";
        assert_eq!(sweep(&mut state, &fixture(&dir, "empty", header_only)), 0);
        assert_eq!(state.slots.slot_of("dentry"), Some(slot));
        assert_eq!(bytes(&state, "dentry"), before);
        assert_eq!(label(slot).as_deref(), Some("dentry"));
        assert_eq!(SLABINFO_ACQ.member_bound(), Some(11));
    }

    /// A name listed twice takes one slot and its larger line.
    #[test]
    fn a_cache_listed_twice_is_published_once_at_its_larger_line() {
        let _g = globals();
        let mut state = SweepState::new(true, 4096);
        let twice = "slabinfo - version: 2.1
kmalloc-64        4353   4864     64  256    4 : tunables    0    0    0 : slabdata     19     19      0
kmalloc-64          10     10     64  256    1 : tunables    0    0    0 : slabdata      1      1      0
";
        assert_eq!(publish(&mut state, twice), 1);
        assert_eq!(bytes(&state, "kmalloc-64"), Some(19 * 4 * 4096));
        assert_eq!(state.slots.bound(), 1);
    }

    #[test]
    fn a_sweep_stamps_the_group_and_bounds_it_to_the_published_caches() {
        let _g = globals();
        let dir = tempfile::tempdir().unwrap();
        let mut state = SweepState::new(true, 4096);

        assert_eq!(sweep(&mut state, "/definitely/not/slabinfo"), 0);
        assert_eq!(SLABINFO_ACQ.member_bound(), Some(0));

        assert_eq!(sweep(&mut state, &fixture(&dir, "first", FIRST)), 11);
        assert_eq!(SLABINFO_ACQ.member_bound(), Some(11));
        assert!(SLABINFO_ACQ.window().is_some());

        assert_eq!(sweep(&mut state, &fixture(&dir, "second", SECOND)), 13);
        assert_eq!(SLABINFO_ACQ.member_bound(), Some(13));

        // A failed read keeps the population it last published.
        assert_eq!(sweep(&mut state, "/definitely/not/slabinfo"), 0);
        assert_eq!(SLABINFO_ACQ.member_bound(), Some(13));
    }

    /// A followed cache that stops being listed reads absent in every
    /// family, object counts included, not its last value.
    #[test]
    fn a_followed_cache_that_goes_reads_absent_in_every_family() {
        let _g = globals();
        let mut state = SweepState::new(true, 4096);
        publish(&mut state, FIRST);
        let slot = state.slots.slot_of("xfs_inode").unwrap();
        assert_eq!(objects(&state, "xfs_inode"), (Some(0), Some(0)));
        publish(&mut state, SECOND);
        for group in GROUPS {
            let value = group.value(slot);
            // The slot was reused by a module cache, which has bytes only.
            if std::ptr::eq(*group, &SLAB_CACHE_BYTES) {
                assert!(value.is_some());
            } else {
                assert_eq!(value, None, "no object count left behind");
            }
        }
        assert_eq!(label(slot).as_deref(), Some("zio_buf_comb_512"));
    }

    #[test]
    fn a_poisoned_sweep_state_is_restarted_with_every_slot_cleared() {
        let _g = globals();
        let state = Arc::new(Mutex::new(SweepState::new(true, 4096)));
        publish(&mut state.lock().unwrap(), FIRST);
        let slot = state.lock().unwrap().slots.slot_of("dentry").unwrap();
        let poisoner = state.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("sweep failed");
        })
        .join();

        let guard = lock_state(&state);
        assert_eq!(guard.slots.bound(), 0);
        assert!(guard.all, "the configured mode survives the restart");
        assert_eq!(SLAB_CACHE_BYTES.value(slot), None);
        assert_eq!(label(slot), None);
    }

    /// Every followed cache name is spelled as the kernel spells it: a typo
    /// would leave its gauges unset forever without any error.
    #[test]
    fn followed_cache_names_are_the_kernels() {
        let known = [
            "dentry",
            "inode_cache",
            "ext4_inode_cache",
            "ext4_extent_status",
            "jbd2_journal_head",
            "buffer_head",
            "xfs_inode",
            "radix_tree_node",
        ];
        for cache in FOLLOWED {
            assert!(known.contains(cache), "{cache} is not a kernel slab name");
        }
        assert_eq!(FOLLOWED.len(), known.len());
    }

    /// Manual phase measurement:
    ///   cargo test --release --bin rezolus -- slabinfo::tests::sweep_phase_timing --ignored --nocapture
    #[test]
    #[ignore]
    fn sweep_phase_timing() {
        let _g = globals();
        let n = 200u32;
        let mut state = SweepState::new(true, page_size());
        let (mut t_read, mut t_publish, mut published) = (0u128, 0u128, 0);
        for _ in 0..n {
            let t0 = Instant::now();
            let data = std::fs::read_to_string(SLABINFO).unwrap();
            let t1 = Instant::now();
            published = publish(&mut state, &data);
            t_read += (t1 - t0).as_nanos();
            t_publish += t1.elapsed().as_nanos();
        }
        let n = u128::from(n) * 1000;
        println!(
            "{published} caches: read {} us, parse and publish {} us",
            t_read / n,
            t_publish / n
        );
    }

    /// The real file, where there is one (Linux, root or `slabinfo`
    /// readable): the parse must find at least the dentry cache.
    #[test]
    fn the_real_file_parses_where_readable() {
        let _g = globals();
        let mut state = SweepState::new(true, page_size());
        if std::fs::read_to_string(SLABINFO).is_ok() {
            assert!(sweep(&mut state, SLABINFO) >= 1);
            assert!(objects(&state, "dentry").1.is_some_and(|n| n > 0));
            assert!(bytes(&state, "dentry").is_some_and(|b| b > 0));
        }
    }
}
