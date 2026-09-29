//! Sizes of a fixed set of slab caches from `/proc/slabinfo`: the dentry and
//! inode caches, ext4's and XFS's inode caches, ext4's extent-status cache,
//! jbd2 journal heads, buffer heads and the page-cache index nodes.
//!
//! The question these answer is "does the metadata cache fit". A filesystem
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
//! and, once per interval, a dispatch. The sweep's read and parse durations
//! are logged at debug level.
//!
//! A cache the running kernel does not have (`xfs_inode` without XFS loaded,
//! `ext4_*` without ext4) leaves its gauges never set, which the snapshot
//! reports as absent rather than 0. So does a cache SLUB has merged: a cache
//! without a constructor can share a pool with others of its size and flags,
//! and the file then lists the pool under one name only. `ext4_extent_status`
//! (40 bytes, no constructor) is merged on the Debian 13 6.12 kernel and is
//! absent there; `jbd2_journal_head` was not, and the inode and dentry caches
//! have constructors and are never merged. `slab_nomerge` on the kernel
//! command line lists every cache under its own name.

const NAME: &str = "memory_slabinfo";

use crate::agent::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod stats;

use stats::*;

/// Built-in sweep cadence when `[samplers.memory_slabinfo] interval` is unset.
const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(60);

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // Fail at init, not on the first sweep, when the file cannot be read: an
    // unprivileged agent gets EACCES here and `rezolus status` says so.
    std::fs::read_to_string("/proc/slabinfo")?;

    let interval = config
        .sampler_interval(NAME)
        .unwrap_or(DEFAULT_READ_INTERVAL);

    debug!("{NAME}: sweeping every {interval:?}");

    Ok(Some(Box::new(Slabinfo {
        interval,
        last_read: Mutex::new(None),
        reading: Arc::new(AtomicBool::new(false)),
        page_size: page_size(),
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
    page_size: u64,
}

/// Clears the in-flight latch when the sweep ends, by return or by unwind.
struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
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
        let page_size = self.page_size;

        // Off the async worker: the read holds the slab lock in the kernel and
        // the parse walks a few hundred lines. This task is `SLABINFO_ACQ`'s
        // single writer (principle 18); `refresh()` only dispatches it.
        tokio::task::spawn_blocking(move || {
            let _in_flight = InFlight(reading);
            sweep(page_size);
        });
    }
}

/// One read of `/proc/slabinfo` and one parse, bracketed as one acquisition.
/// A failed read, or a file naming none of the caches followed, discards:
/// the previous window stands and the gauges keep their values.
fn sweep(page_size: u64) {
    let guard = SLABINFO_ACQ.acquire();
    let started = Instant::now();

    let data = match std::fs::read_to_string("/proc/slabinfo") {
        Ok(data) => data,
        Err(e) => {
            warn!("{NAME}: reading /proc/slabinfo failed: {e}");
            guard.discard();
            return;
        }
    };
    let read = started.elapsed();

    let published = parse(&data, page_size);
    let parsed = started.elapsed() - read;

    debug!(
        "{NAME} sweep: read {} us, parse {} us, {} of {} caches present",
        read.as_micros(),
        parsed.as_micros(),
        published,
        CACHES.len()
    );

    if published > 0 {
        guard.finish();
    } else {
        guard.discard();
    }
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

/// Set the gauges of every followed cache the text names; return how many.
fn parse(data: &str, page_size: u64) -> usize {
    let mut published = 0;

    for line in data.lines() {
        let Some((name, parsed)) = parse_line(line, page_size) else {
            continue;
        };
        if let Some(cache) = CACHES.iter().find(|c| c.name == name) {
            cache.active_objects.set(parsed.active_objects as i64);
            cache.total_objects.set(parsed.total_objects as i64);
            cache.bytes.set(parsed.bytes as i64);
            published += 1;
        }
    }

    published
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file's shape on a SLUB kernel: two header lines, then one line per
    /// cache with the tunables and slabdata sections.
    const SAMPLE: &str = "slabinfo - version: 2.1
# name            <active_objs> <num_objs> <objsize> <objperslab> <pagesperslab> : tunables <limit> <batchcount> <sharedfactor> : slabdata <active_slabs> <num_slabs> <sharedavail>
ext4_inode_cache  1234567 1300000   1176   27    8 : tunables    0    0    0 : slabdata  48148  48148      0
ext4_extent_status 500000  600000     40  102    1 : tunables    0    0    0 : slabdata   5882   5882      0
jbd2_journal_head   1200    1500    120   34    1 : tunables    0    0    0 : slabdata     44     44      0
buffer_head       800000  850000    104   39    1 : tunables    0    0    0 : slabdata  21795  21795      0
dentry           2000000 2100000    192   21    1 : tunables    0    0    0 : slabdata 100000 100000      0
inode_cache        60000   61000    640   25    4 : tunables    0    0    0 : slabdata   2440   2440      0
radix_tree_node   300000  310000    584   28    4 : tunables    0    0    0 : slabdata  11072  11072      0
kmalloc-8            999    1024      8  512    1 : tunables    0    0    0 : slabdata      2      2      0
";

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
    fn the_followed_caches_are_set_and_others_ignored() {
        let published = parse(SAMPLE, 4096);
        // Seven of the eight followed caches are in the sample (no xfs_inode).
        assert_eq!(published, 7);
        assert_eq!(EXT4_INODE_CACHE_ACTIVE.value(), 1234567);
        assert_eq!(EXT4_INODE_CACHE_BYTES.value(), 48148 * 8 * 4096);
        assert_eq!(DENTRY_TOTAL.value(), 2100000);
        assert_eq!(RADIX_TREE_NODE_BYTES.value(), 11072 * 4 * 4096);
        assert_eq!(BUFFER_HEAD_ACTIVE.value(), 800000);
    }

    /// A cache the kernel does not have leaves its gauges untouched.
    #[test]
    fn a_missing_cache_is_left_alone() {
        XFS_INODE_BYTES.set(7);
        parse(SAMPLE, 4096);
        assert_eq!(XFS_INODE_BYTES.value(), 7);
    }

    /// Every followed cache name is spelled as the kernel spells it: a typo
    /// would leave three gauges unset forever without any error.
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
        for cache in CACHES {
            assert!(
                known.contains(&cache.name),
                "{} is not a kernel slab name",
                cache.name
            );
        }
        assert_eq!(CACHES.len(), known.len());
    }

    /// The real file, where there is one (Linux, root or `slabinfo`
    /// readable): the parse must find at least the dentry cache.
    #[test]
    fn the_real_file_parses_where_readable() {
        if let Ok(data) = std::fs::read_to_string("/proc/slabinfo") {
            assert!(parse(&data, page_size()) >= 1);
            assert!(DENTRY_TOTAL.value() > 0);
        }
    }
}
