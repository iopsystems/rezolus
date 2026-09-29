//! XFS's own per-mount counters, from `/sys/fs/xfs/<block_device>/stats/stats`:
//! the log (writes, forces, force sleeps, in-core log buffer stalls), log
//! space (reservations and the ones that slept for space), the AIL pusher's
//! outcomes, transactions, the inode cache, the allocator, directories, file
//! I/O and the metadata buffer cache.
//!
//! ```text
//! filesystem slot registry (mountinfo) -> for each XFS mount:
//!   read /sys/fs/xfs/<block_device>/stats/stats -> parse lines -> counters[slot]
//! -> member bound -> window
//! ```
//!
//! # Why sysfs and not BPF
//!
//! XFS maintains these counters itself, at event rate, in the same code paths
//! its tracepoints instrument; a BPF sampler counting the same events would
//! cross about 5.6 log-family hooks per fsync (measured, see the journal
//! entry) to reproduce numbers the kernel already has. Principle 15 prefers
//! probes over parsing, and the exception here is the one `filesystem_errors`
//! and `filesystem_written_bytes` received: the kernel keeps the counter, the
//! read is one small sysfs file per mount, and it works on every kernel with
//! XFS. What sysfs cannot say, the *time* a transaction waited for log space
//! or a log force took, and which cgroup waited, is the `xfs_log` BPF
//! sampler's job (docs/journal/2026-09-29-xfs-samplers.md).
//!
//! # Cadence
//!
//! One read of the file costs about 159 µs on the probe guest, a refresh
//! budget on its own, so the sweep runs on the blocking pool at most once per
//! `interval` (1 s by default: these are counters, and rates want a cadence
//! finer than `filesystem`'s 60 s occupancy sweep). `refresh()` pays a time
//! check and, once per interval, a dispatch. Sweeps never overlap.
//!
//! # Per mount
//!
//! Slots and labels come from the shared filesystem registry
//! (`bpf/filesystems.rs`), so an XFS mount's series here carry the same
//! `mount`, `fstype`, `devnum` and `block_device` the `filesystem` sampler
//! gives it, and the ext4 samplers give theirs. A slot whose filesystem is not
//! XFS, or has no sysfs directory (no block device name), reads absent. The
//! sysfs directory is named after the block device, the same `/sys/dev/block`
//! link the label resolves.

const NAME: &str = "xfs_stats";

/// Built-in sweep cadence when `[samplers.xfs_stats] interval` is unset.
const DEFAULT_READ_INTERVAL: Duration = Duration::from_secs(1);

const SYS_FS_XFS: &str = "/sys/fs/xfs";

use crate::agent::bpf::filesystems::{self, Assignment};
use crate::agent::*;
use metriken::CounterGroup;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// The sampler is `xfs_stats` and its metrics file is `stats.rs`, as in
// every other sampler; the module path repeats the name for that reason.
#[allow(clippy::module_inception)]
mod stats;

use stats::*;

/// Every published family, in one list so slot clearing and identity cover
/// them all.
static GROUPS: &[&CounterGroup] = &[
    &XFS_LOG_WRITES,
    &XFS_LOG_BLOCKS_WRITTEN,
    &XFS_LOG_ICLOG_STALLS,
    &XFS_LOG_FORCES,
    &XFS_LOG_FORCE_SLEEPS,
    &XFS_LOG_SPACE_REQUESTS,
    &XFS_LOG_SPACE_SLEEPS,
    &XFS_AIL_PUSHES,
    &XFS_AIL_PUSH_SUCCESS,
    &XFS_AIL_PUSH_PUSHBUF,
    &XFS_AIL_PUSH_PINNED,
    &XFS_AIL_PUSH_LOCKED,
    &XFS_AIL_PUSH_FLUSHING,
    &XFS_AIL_PUSH_RESTARTS,
    &XFS_AIL_FLUSHES,
    &XFS_TRANSACTIONS_SYNC,
    &XFS_TRANSACTIONS_ASYNC,
    &XFS_TRANSACTIONS_EMPTY,
    &XFS_INODE_CACHE_FOUND,
    &XFS_INODE_CACHE_MISSED,
    &XFS_INODE_CACHE_RECYCLED,
    &XFS_INODE_CACHE_DUPLICATE,
    &XFS_INODE_RECLAIMS,
    &XFS_EXTENTS_ALLOCATED,
    &XFS_EXTENTS_FREED,
    &XFS_EXTENT_BLOCKS_ALLOCATED,
    &XFS_EXTENT_BLOCKS_FREED,
    &XFS_DIRECTORY_LOOKUPS,
    &XFS_DIRECTORY_CREATES,
    &XFS_DIRECTORY_REMOVES,
    &XFS_DIRECTORY_GETDENTS,
    &XFS_FILE_WRITE_CALLS,
    &XFS_FILE_READ_CALLS,
    &XFS_FILE_BYTES_WRITTEN,
    &XFS_FILE_BYTES_READ,
    &XFS_BUFFER_LOOKUPS,
    &XFS_BUFFER_CREATES,
    &XFS_BUFFER_LOCK_WAITS,
    &XFS_BUFFER_BUSY_LOCKS,
    &XFS_BUFFER_MISSES,
    &XFS_BUFFER_READS,
];

/// What a filesystem slot means here, published when it changes. The same
/// slot numbering as the ext4 samplers' (one registry), a separate identity
/// because these are this sampler's metrics.
static IDENTITY: metriken::group::SlotIdentity =
    metriken::group::SlotIdentity::grouped(IDENTITY_GROUPS);

static IDENTITY_GROUPS: &[&[&dyn metriken::group::SlotMetadata]] = &[&[
    &XFS_LOG_WRITES,
    &XFS_LOG_BLOCKS_WRITTEN,
    &XFS_LOG_ICLOG_STALLS,
    &XFS_LOG_FORCES,
    &XFS_LOG_FORCE_SLEEPS,
    &XFS_LOG_SPACE_REQUESTS,
    &XFS_LOG_SPACE_SLEEPS,
    &XFS_AIL_PUSHES,
    &XFS_AIL_PUSH_SUCCESS,
    &XFS_AIL_PUSH_PUSHBUF,
    &XFS_AIL_PUSH_PINNED,
    &XFS_AIL_PUSH_LOCKED,
    &XFS_AIL_PUSH_FLUSHING,
    &XFS_AIL_PUSH_RESTARTS,
    &XFS_AIL_FLUSHES,
    &XFS_TRANSACTIONS_SYNC,
    &XFS_TRANSACTIONS_ASYNC,
    &XFS_TRANSACTIONS_EMPTY,
    &XFS_INODE_CACHE_FOUND,
    &XFS_INODE_CACHE_MISSED,
    &XFS_INODE_CACHE_RECYCLED,
    &XFS_INODE_CACHE_DUPLICATE,
    &XFS_INODE_RECLAIMS,
    &XFS_EXTENTS_ALLOCATED,
    &XFS_EXTENTS_FREED,
    &XFS_EXTENT_BLOCKS_ALLOCATED,
    &XFS_EXTENT_BLOCKS_FREED,
    &XFS_DIRECTORY_LOOKUPS,
    &XFS_DIRECTORY_CREATES,
    &XFS_DIRECTORY_REMOVES,
    &XFS_DIRECTORY_GETDENTS,
    &XFS_FILE_WRITE_CALLS,
    &XFS_FILE_READ_CALLS,
    &XFS_FILE_BYTES_WRITTEN,
    &XFS_FILE_BYTES_READ,
    &XFS_BUFFER_LOOKUPS,
    &XFS_BUFFER_CREATES,
    &XFS_BUFFER_LOCK_WAITS,
    &XFS_BUFFER_BUSY_LOCKS,
    &XFS_BUFFER_MISSES,
    &XFS_BUFFER_READS,
]];

/// Where each counter comes from: the stats line, the 0-based field on it,
/// and the group it fills. The field order is the kernel's `xfsstats`
/// (`fs/xfs/xfs_stats.h`), confirmed on 6.12 in the journal entry.
const FIELDS: &[(&str, usize, &CounterGroup)] = &[
    ("log", 0, &XFS_LOG_WRITES),
    ("log", 1, &XFS_LOG_BLOCKS_WRITTEN),
    ("log", 2, &XFS_LOG_ICLOG_STALLS),
    ("log", 3, &XFS_LOG_FORCES),
    ("log", 4, &XFS_LOG_FORCE_SLEEPS),
    ("push_ail", 0, &XFS_LOG_SPACE_REQUESTS),
    ("push_ail", 1, &XFS_LOG_SPACE_SLEEPS),
    ("push_ail", 2, &XFS_AIL_PUSHES),
    ("push_ail", 3, &XFS_AIL_PUSH_SUCCESS),
    ("push_ail", 4, &XFS_AIL_PUSH_PUSHBUF),
    ("push_ail", 5, &XFS_AIL_PUSH_PINNED),
    ("push_ail", 6, &XFS_AIL_PUSH_LOCKED),
    ("push_ail", 7, &XFS_AIL_PUSH_FLUSHING),
    ("push_ail", 8, &XFS_AIL_PUSH_RESTARTS),
    ("push_ail", 9, &XFS_AIL_FLUSHES),
    ("trans", 0, &XFS_TRANSACTIONS_SYNC),
    ("trans", 1, &XFS_TRANSACTIONS_ASYNC),
    ("trans", 2, &XFS_TRANSACTIONS_EMPTY),
    ("ig", 1, &XFS_INODE_CACHE_FOUND),
    ("ig", 2, &XFS_INODE_CACHE_RECYCLED),
    ("ig", 3, &XFS_INODE_CACHE_MISSED),
    ("ig", 4, &XFS_INODE_CACHE_DUPLICATE),
    ("ig", 5, &XFS_INODE_RECLAIMS),
    ("extent_alloc", 0, &XFS_EXTENTS_ALLOCATED),
    ("extent_alloc", 1, &XFS_EXTENT_BLOCKS_ALLOCATED),
    ("extent_alloc", 2, &XFS_EXTENTS_FREED),
    ("extent_alloc", 3, &XFS_EXTENT_BLOCKS_FREED),
    ("dir", 0, &XFS_DIRECTORY_LOOKUPS),
    ("dir", 1, &XFS_DIRECTORY_CREATES),
    ("dir", 2, &XFS_DIRECTORY_REMOVES),
    ("dir", 3, &XFS_DIRECTORY_GETDENTS),
    ("rw", 0, &XFS_FILE_WRITE_CALLS),
    ("rw", 1, &XFS_FILE_READ_CALLS),
    ("xpc", 1, &XFS_FILE_BYTES_WRITTEN),
    ("xpc", 2, &XFS_FILE_BYTES_READ),
    ("buf", 0, &XFS_BUFFER_LOOKUPS),
    ("buf", 1, &XFS_BUFFER_CREATES),
    ("buf", 3, &XFS_BUFFER_LOCK_WAITS),
    ("buf", 4, &XFS_BUFFER_BUSY_LOCKS),
    ("buf", 5, &XFS_BUFFER_MISSES),
    ("buf", 8, &XFS_BUFFER_READS),
];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    // No XFS mounted, or the module not loaded, is not a failure: a mount
    // can appear later and the registry will hand it a slot. Only a kernel
    // with no `/sys/fs` at all would make the read impossible, and that is
    // reported by the sweep's debug log rather than refused here.
    let interval = config
        .sampler_interval(NAME)
        .unwrap_or(DEFAULT_READ_INTERVAL);

    debug!("{NAME}: sweeping every {interval:?}");

    // No member until the first sweep lands.
    XFS_STATS_ACQ.set_member_bound(0);

    Ok(Some(Box::new(XfsStats {
        interval,
        last_read: Mutex::new(None),
        reading: Arc::new(AtomicBool::new(false)),
        state: Arc::new(Mutex::new(SweepState::default())),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct XfsStats {
    /// Minimum spacing between sweeps.
    interval: Duration,
    /// When the last sweep was dispatched; `None` until the first.
    last_read: Mutex<Option<Instant>>,
    /// True while a sweep is in flight, so sweeps never overlap.
    reading: Arc<AtomicBool>,
    state: Arc<Mutex<SweepState>>,
}

/// Which slots this sampler has labeled, so a slot that stops being an XFS
/// mount is cleared once, not re-cleared (and re-published) every sweep.
#[derive(Default)]
struct SweepState {
    labeled: Vec<bool>,
}

/// Clears the in-flight latch when the sweep ends, by return or by unwind.
struct InFlight(Arc<AtomicBool>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[async_trait]
impl Sampler for XfsStats {
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

        // Off the async worker: one ~160 µs sysfs read per XFS mount. This
        // task is `XFS_STATS_ACQ`'s single writer (principle 18); `refresh()`
        // only dispatches it.
        tokio::task::spawn_blocking(move || {
            let _in_flight = InFlight(reading);
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            sweep(&mut state, Path::new(SYS_FS_XFS), &filesystems::current());
        });
    }
}

/// Every value family reads absent for `slot`.
fn unset(slot: usize) {
    for group in GROUPS {
        // u64::MAX is CounterGroup's never-written sentinel.
        let _ = group.set(slot, u64::MAX);
    }
}

/// One sweep over the registry's slots: every XFS mount's stats file is read
/// and published under its labels; every other slot reads absent. Bracketed
/// as one acquisition; a sweep that published nothing discards, so the
/// previous window stands.
fn sweep(state: &mut SweepState, sys_fs_xfs: &Path, assignment: &Assignment) -> usize {
    let guard = XFS_STATS_ACQ.acquire();
    let started = Instant::now();
    let mut published = 0;
    // One past the highest slot this sweep gave values: the group's own
    // population. The registry's bound covers ext4 slots this sampler never
    // writes, and declaring those would put an all-null XFS column for each
    // into every tick's snapshot on a host with no XFS at all.
    let mut bound = 0;
    let mut read_us = 0u128;

    if state.labeled.len() < assignment.slots.len() {
        state.labeled.resize(assignment.slots.len(), false);
    }

    for (slot, held) in assignment.slots.iter().enumerate() {
        let text = match held {
            Some(fs) if fs.fstype == "xfs" => match &fs.block_device {
                Some(device) => {
                    let path = sys_fs_xfs.join(device).join("stats").join("stats");
                    let read_started = Instant::now();
                    let text = std::fs::read_to_string(&path);
                    read_us += read_started.elapsed().as_micros();
                    match text {
                        Ok(text) => Some((fs, text)),
                        Err(e) => {
                            debug!("{NAME}: reading {} failed: {e}", path.display());
                            None
                        }
                    }
                }
                None => None,
            },
            _ => None,
        };

        match text {
            Some((fs, text)) => {
                if !state.labeled[slot] {
                    IDENTITY.assign(slot, fs.labels());
                    state.labeled[slot] = true;
                } else {
                    // A relabel (same device, moved mount point) is a set too;
                    // SlotIdentity ignores a re-announcement of the same labels.
                    IDENTITY.assign(slot, fs.labels());
                }
                if publish(slot, &text) > 0 {
                    published += 1;
                    bound = slot + 1;
                }
            }
            None => {
                if state.labeled[slot] {
                    IDENTITY.release(slot);
                    state.labeled[slot] = false;
                    unset(slot);
                }
            }
        }
    }

    // The sweep is the sole writer; the bound must be stored before finish().
    XFS_STATS_ACQ.set_member_bound(bound);

    if published > 0 {
        guard.finish();
    } else {
        guard.discard();
    }

    debug!(
        "{NAME} sweep: {published} XFS mount(s) in {} us (reads {read_us} us)",
        started.elapsed().as_micros()
    );
    published
}

/// The fields of the stats line named `name`, or `None` if the file has no
/// such line.
fn line_fields(text: &str, name: &str) -> Option<Vec<u64>> {
    text.lines().find_map(|line| {
        let mut parts = line.split_ascii_whitespace();
        if parts.next()? != name {
            return None;
        }
        // A token that is not a number makes the whole line absent: dropping
        // it would shift every later field onto the wrong counter.
        parts.map(|f| f.parse().ok()).collect()
    })
}

/// Set every counter the file carries for `slot`; a line or field the
/// running kernel does not have leaves that counter absent. Returns how many
/// were set.
fn publish(slot: usize, text: &str) -> usize {
    let mut set = 0;
    let mut current: Option<(&str, Vec<u64>)> = None;
    for (line, field, group) in FIELDS {
        if current.as_ref().map(|(l, _)| l) != Some(line) {
            current = line_fields(text, line).map(|fields| (*line, fields));
        }
        match current.as_ref().and_then(|(_, fields)| fields.get(*field)) {
            Some(value) => {
                let _ = group.set(slot, *value);
                set += 1;
            }
            None => {
                let _ = group.set(slot, u64::MAX);
            }
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/sys/fs/xfs/loop0/stats/stats` on 6.12.63 after 1,349 write+fsync
    /// pairs, 500 small files and a cold stat of each (journal entry).
    const SAMPLE: &str = "\
extent_alloc 531 39844 0 0
abt 0 0 0 0
blk_map 2399 4714 0 2361 0 8968 0
bmbt 0 0 0 0
dir 1008 504 0 2
trans 0 7579 0
ig 1011 0 0 1011 0 504 505
log 1326 31174 0 1850 3186
push_ail 7579 0 174 66 0 0 0 14 0 0
xstrat 0 0
rw 1849 0
attr 504 0 0 0
icluster 0 16 505
vnodes 507 0 0 0 504 504 504 0
buf 22128 107 22024 18336 360 104 0 90 49
abtb2 560 967 2 2 0 0 0 0 0 0 0 0 0 0 2
xpc 0 34197504 0
defer_relog 0
debug 0
";

    /// Serializes tests that write the shared groups.
    static GLOBALS: Mutex<()> = Mutex::new(());

    #[test]
    fn every_field_the_sampler_names_is_on_the_sample_file() {
        let _g = GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        let slot = MAX_FILESYSTEMS - 1;
        assert_eq!(publish(slot, SAMPLE), FIELDS.len());
        assert_eq!(XFS_LOG_FORCES.value(slot), Some(1850));
        assert_eq!(XFS_LOG_FORCE_SLEEPS.value(slot), Some(3186));
        assert_eq!(XFS_LOG_BLOCKS_WRITTEN.value(slot), Some(31174));
        assert_eq!(XFS_LOG_SPACE_REQUESTS.value(slot), Some(7579));
        assert_eq!(XFS_LOG_SPACE_SLEEPS.value(slot), Some(0));
        assert_eq!(XFS_AIL_PUSHES.value(slot), Some(174));
        assert_eq!(XFS_AIL_PUSH_SUCCESS.value(slot), Some(66));
        assert_eq!(XFS_AIL_PUSH_FLUSHING.value(slot), Some(14));
        assert_eq!(XFS_TRANSACTIONS_ASYNC.value(slot), Some(7579));
        assert_eq!(XFS_INODE_CACHE_MISSED.value(slot), Some(1011));
        assert_eq!(XFS_INODE_RECLAIMS.value(slot), Some(504));
        assert_eq!(XFS_EXTENTS_ALLOCATED.value(slot), Some(531));
        assert_eq!(XFS_EXTENT_BLOCKS_ALLOCATED.value(slot), Some(39844));
        assert_eq!(XFS_DIRECTORY_CREATES.value(slot), Some(504));
        assert_eq!(XFS_FILE_WRITE_CALLS.value(slot), Some(1849));
        assert_eq!(XFS_FILE_BYTES_WRITTEN.value(slot), Some(34197504));
        assert_eq!(XFS_BUFFER_LOOKUPS.value(slot), Some(22128));
        assert_eq!(XFS_BUFFER_MISSES.value(slot), Some(104));
        assert_eq!(XFS_BUFFER_READS.value(slot), Some(49));
        unset(slot);
        assert_eq!(XFS_LOG_FORCES.value(slot), None);
    }

    #[test]
    fn a_missing_line_or_short_line_leaves_those_counters_absent() {
        let _g = GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        let slot = MAX_FILESYSTEMS - 2;
        // An older kernel without the xpc line and a truncated push_ail.
        let text = "log 1 2 3 4 5\npush_ail 10 20\n";
        let set = publish(slot, text);
        assert_eq!(set, 7, "five log fields and two push_ail fields");
        assert_eq!(XFS_LOG_SPACE_SLEEPS.value(slot), Some(20));
        assert_eq!(XFS_AIL_PUSHES.value(slot), None);
        assert_eq!(XFS_FILE_BYTES_WRITTEN.value(slot), None);
        unset(slot);
    }

    #[test]
    fn line_fields_match_by_whole_name() {
        assert_eq!(line_fields(SAMPLE, "rw"), Some(vec![1849, 0]));
        assert_eq!(line_fields(SAMPLE, "log").map(|f| f.len()), Some(5));
        // "abt" must not match "abtb2".
        assert_eq!(line_fields(SAMPLE, "abt"), Some(vec![0, 0, 0, 0]));
        assert_eq!(line_fields(SAMPLE, "nope"), None);
    }

    #[test]
    fn a_sweep_publishes_xfs_mounts_only_and_labels_them_once() {
        let _g = GLOBALS.lock().unwrap_or_else(|p| p.into_inner());
        let sys = tempfile::tempdir().unwrap();
        let dir = sys.path().join("sdz1").join("stats");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stats"), SAMPLE).unwrap();

        let (xfs_slot, ext4_slot) = (MAX_FILESYSTEMS - 3, MAX_FILESYSTEMS - 4);
        let mut assignment = Assignment::default();
        assignment.slots[xfs_slot] = Some(filesystems::Filesystem {
            dev: 8 << 20 | 401,
            devnum: "8:401".to_string(),
            mount: "/scratch".to_string(),
            fstype: "xfs".to_string(),
            block_device: Some("sdz1".to_string()),
        });
        assignment.slots[ext4_slot] = Some(filesystems::Filesystem {
            dev: 8 << 20 | 402,
            devnum: "8:402".to_string(),
            mount: "/data".to_string(),
            fstype: "ext4".to_string(),
            block_device: Some("sdz2".to_string()),
        });
        assignment.generation = 1;

        let mut state = SweepState::default();
        assert_eq!(sweep(&mut state, sys.path(), &assignment), 1);
        assert_eq!(XFS_LOG_FORCES.value(xfs_slot), Some(1850));
        assert_eq!(XFS_LOG_FORCES.value(ext4_slot), None);
        // The population is the XFS slots this sweep filled, not the shared
        // registry's bound (which the ext4 slot above would raise).
        assert_eq!(XFS_STATS_ACQ.member_bound(), Some(xfs_slot + 1));
        let labels = XFS_LOG_FORCES.load_metadata(xfs_slot).expect("labels set");
        assert_eq!(labels.get("mount").map(String::as_str), Some("/scratch"));
        assert_eq!(labels.get("fstype").map(String::as_str), Some("xfs"));
        assert!(state.labeled[xfs_slot] && !state.labeled[ext4_slot]);

        // The mount goes away: its slot is cleared once and reads absent.
        assignment.slots[xfs_slot] = None;
        assert_eq!(sweep(&mut state, sys.path(), &assignment), 0);
        assert_eq!(XFS_LOG_FORCES.value(xfs_slot), None);
        assert!(!state.labeled[xfs_slot]);
        // With no XFS mount left the group declares no members, so an
        // ext4-only host gets no XFS table at all.
        assert_eq!(XFS_STATS_ACQ.member_bound(), Some(0));
    }

    #[test]
    fn a_line_with_a_non_numeric_token_reads_absent_rather_than_shifted() {
        let text = "log 12 34 x 56 78\nrw 1 2\n";
        assert_eq!(line_fields(text, "log"), None);
        assert_eq!(line_fields(text, "rw"), Some(vec![1, 2]));
    }
}
