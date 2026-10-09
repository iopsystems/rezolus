//! Block IO counters on macOS, from each `IOBlockStorageDriver`'s
//! `Statistics` dictionary in the I/O registry (the counters `iostat` reads).
//!
//! The driver keeps cumulative operations, bytes, errors, retries and time for
//! reads and writes. This sampler sums them over every driver into the
//! system-wide `blockio_operations` and `blockio_bytes` counters the Linux
//! sampler publishes, and the macOS-only counters in `macos_stats`. Disk
//! images have drivers of their own, so IO to an image is counted both there
//! and on the disk holding the image file when it reaches that disk.
//!
//! There is no per-request view, so nothing writes the size and latency
//! histograms, the classified error counters or the requeue counters. The
//! histograms are scalar statics, which a member bound does not remove, so
//! they appear in every snapshot with no value. On platforms without a
//! `blockio` sampler they appear the same way under `unattributed`.

const NAME: &str = "blockio";

use crate::agent::samplers::iokit::{CfDictionary, CfString, Service};
use crate::agent::*;

use std::collections::HashMap;
use tokio::sync::Mutex;

mod stats {
    include!("../linux/blockio/stats.rs");
}
mod macos_stats;

use macos_stats::*;
use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    // Every metric this sampler publishes belongs to the `requests` part; the
    // `latency` part has nothing to read here.
    if !config.enabled(NAME) || !config.part(NAME, "requests") {
        return Ok(None);
    }

    let keys = Keys::new().ok_or_else(|| anyhow::anyhow!("{NAME}: could not build IOKit keys"))?;
    if read_drivers(&keys)
        .unwrap_or_default()
        .iter()
        .all(|(_, s)| s.is_none())
    {
        return Err(crate::agent::sampler_status::Unsupported(
            "no IOBlockStorageDriver reports statistics".to_string(),
        )
        .into());
    }

    Ok(Some(Box::new(BlockIo {
        inner: Mutex::new(Inner {
            keys,
            last: HashMap::new(),
        }),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct BlockIo {
    inner: Mutex<Inner>,
}

#[async_trait]
impl Sampler for BlockIo {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        self.inner.lock().await.refresh();
    }
}

/// Index of the read and write halves of each per-op pair.
const READ: usize = 0;
const WRITE: usize = 1;

/// One driver's cumulative counters, `[read, write]` each.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct DriverStats {
    operations: [u64; 2],
    bytes: [u64; 2],
    errors: [u64; 2],
    retries: [u64; 2],
    total_time: [u64; 2],
}

impl DriverStats {
    /// The increase from `prev` to `self`, field by field. A field that went
    /// down means the driver restarted its count, so the increase is the
    /// whole new value.
    fn since(&self, prev: &DriverStats) -> DriverStats {
        fn d(now: [u64; 2], prev: [u64; 2]) -> [u64; 2] {
            [0, 1].map(|i| {
                if now[i] >= prev[i] {
                    now[i] - prev[i]
                } else {
                    now[i]
                }
            })
        }
        DriverStats {
            operations: d(self.operations, prev.operations),
            bytes: d(self.bytes, prev.bytes),
            errors: d(self.errors, prev.errors),
            retries: d(self.retries, prev.retries),
            total_time: d(self.total_time, prev.total_time),
        }
    }

    fn add(&mut self, other: &DriverStats) {
        for i in [READ, WRITE] {
            self.operations[i] += other.operations[i];
            self.bytes[i] += other.bytes[i];
            self.errors[i] += other.errors[i];
            self.retries[i] += other.retries[i];
            self.total_time[i] += other.total_time[i];
        }
    }
}

/// The `Statistics` dictionary and its keys, as `IOBlockStorageDriver.h`
/// names them.
struct Keys {
    statistics: CfString,
    operations: [CfString; 2],
    bytes: [CfString; 2],
    errors: [CfString; 2],
    retries: [CfString; 2],
    total_time: [CfString; 2],
}

impl Keys {
    fn new() -> Option<Self> {
        let pair = |what: &str| -> Option<[CfString; 2]> {
            Some([
                CfString::new(&format!("{what} (Read)"))?,
                CfString::new(&format!("{what} (Write)"))?,
            ])
        };
        Some(Self {
            statistics: CfString::new("Statistics")?,
            operations: pair("Operations")?,
            bytes: pair("Bytes")?,
            errors: pair("Errors")?,
            retries: pair("Retries")?,
            total_time: pair("Total Time")?,
        })
    }

    /// The driver's counters, or `None` if any of them is missing. A missing
    /// counter is not read as zero: the next read that has it would count
    /// its whole value again.
    fn read(&self, d: &CfDictionary) -> Option<DriverStats> {
        let pair = |k: &[CfString; 2]| -> Option<[u64; 2]> {
            Some([d.get_u64(&k[READ])?, d.get_u64(&k[WRITE])?])
        };
        Some(DriverStats {
            operations: pair(&self.operations)?,
            bytes: pair(&self.bytes)?,
            errors: pair(&self.errors)?,
            retries: pair(&self.retries)?,
            total_time: pair(&self.total_time)?,
        })
    }
}

/// Every block storage driver, keyed by registry entry ID, with its counters
/// or `None` if this read of them failed. `None` if the drivers could not be
/// listed.
fn read_drivers(keys: &Keys) -> Option<Vec<(u64, Option<DriverStats>)>> {
    let drivers = Service::matching("IOBlockStorageDriver")?;
    let read = drivers
        .iter()
        .filter_map(|s| {
            let id = s.id()?;
            Some((
                id,
                s.dictionary(&keys.statistics).and_then(|d| keys.read(&d)),
            ))
        })
        .collect();
    Some(read)
}

/// Fold one refresh's readings into `last` and return the increase since the
/// previous refresh. A driver seen for the first time contributes its whole
/// count. A driver whose read failed keeps its previous reading, so the next
/// successful read counts only what is new. A driver no longer enumerated is
/// dropped, and what it counted stays in the totals.
///
/// Returns `None` and leaves `last` unchanged when no driver was read, which
/// is what a failed enumeration looks like: dropping every driver then would
/// count each one's whole total again on the next read.
fn account(
    last: &mut HashMap<u64, DriverStats>,
    now: Vec<(u64, Option<DriverStats>)>,
) -> Option<DriverStats> {
    if now.iter().all(|(_, s)| s.is_none()) {
        return None;
    }
    let mut add = DriverStats::default();
    let mut next = HashMap::with_capacity(now.len());
    for (id, stats) in now {
        match stats {
            Some(stats) => {
                add.add(&stats.since(&last.get(&id).copied().unwrap_or_default()));
                next.insert(id, stats);
            }
            None => {
                if let Some(prev) = last.get(&id) {
                    next.insert(id, *prev);
                }
            }
        }
    }
    *last = next;
    Some(add)
}

struct Inner {
    keys: Keys,
    /// Each driver's counters at the previous refresh.
    last: HashMap<u64, DriverStats>,
}

impl Inner {
    fn refresh(&mut self) {
        let counters = COUNTERS_ACQ.acquire();
        let errors = ERRORS_ACQ.acquire();

        // A refresh that reads no driver publishes nothing and keeps the
        // groups' previous windows.
        let Some(add) = read_drivers(&self.keys).and_then(|now| account(&mut self.last, now))
        else {
            counters.discard();
            errors.discard();
            return;
        };

        BLOCKIO_READ_OPS.add(add.operations[READ]);
        BLOCKIO_WRITE_OPS.add(add.operations[WRITE]);
        BLOCKIO_READ_BYTES.add(add.bytes[READ]);
        BLOCKIO_WRITE_BYTES.add(add.bytes[WRITE]);
        BLOCKIO_READ_RETRIES.add(add.retries[READ]);
        BLOCKIO_WRITE_RETRIES.add(add.retries[WRITE]);
        BLOCKIO_READ_SERVICE_TIME.add(add.total_time[READ]);
        BLOCKIO_WRITE_SERVICE_TIME.add(add.total_time[WRITE]);
        counters.finish();

        BLOCKIO_READ_DRIVER_ERRORS.add(add.errors[READ]);
        BLOCKIO_WRITE_DRIVER_ERRORS.add(add.errors[WRITE]);
        errors.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(ops: u64) -> DriverStats {
        DriverStats {
            operations: [ops, ops * 2],
            bytes: [ops * 4096, ops * 8192],
            ..Default::default()
        }
    }

    #[test]
    fn since_is_the_per_field_increase() {
        let d = stats(15).since(&stats(10));
        assert_eq!(d.operations, [5, 10]);
        assert_eq!(d.bytes, [5 * 4096, 5 * 8192]);
    }

    #[test]
    fn since_a_restarted_count_is_the_new_value() {
        let d = stats(3).since(&stats(10));
        assert_eq!(d.operations, [3, 6]);
    }

    #[test]
    fn the_first_refresh_counts_everything_since_boot() {
        let mut last = HashMap::new();
        let add = account(&mut last, vec![(1, Some(stats(10))), (2, Some(stats(5)))]).unwrap();
        assert_eq!(add.operations, [15, 30]);
    }

    #[test]
    fn a_failed_read_does_not_count_a_driver_twice() {
        let mut last = HashMap::new();
        account(&mut last, vec![(1, Some(stats(10)))]);
        assert_eq!(account(&mut last, vec![(1, None)]), None);
        let add = account(&mut last, vec![(1, Some(stats(12)))]).unwrap();
        assert_eq!(add.operations, [2, 4]);
    }

    #[test]
    fn a_failed_enumeration_does_not_count_every_driver_again() {
        let mut last = HashMap::new();
        account(&mut last, vec![(1, Some(stats(10)))]);
        assert_eq!(account(&mut last, vec![]), None);
        let add = account(&mut last, vec![(1, Some(stats(12)))]).unwrap();
        assert_eq!(add.operations, [2, 4]);
    }

    #[test]
    fn a_driver_that_goes_away_keeps_its_count_and_a_new_one_adds_its_own() {
        let mut last = HashMap::new();
        account(&mut last, vec![(1, Some(stats(10))), (2, Some(stats(5)))]);
        let add = account(&mut last, vec![(1, Some(stats(11))), (3, Some(stats(2)))]).unwrap();
        assert_eq!(add.operations, [1 + 2, 2 + 4]);
        assert!(!last.contains_key(&2));
    }

    /// Reads the real registry. Every Mac has at least its boot disk's
    /// driver, and its counters are cumulative since boot, so they cannot
    /// all be zero. Skips on a host with no block storage driver.
    #[test]
    fn reads_the_boot_disk_driver() {
        let keys = Keys::new().unwrap();
        let drivers = read_drivers(&keys).unwrap_or_default();
        if drivers.is_empty() {
            eprintln!("no IOBlockStorageDriver on this host; skipping");
            return;
        }
        assert!(
            drivers
                .iter()
                .any(|(_, s)| s.is_some_and(|s| s.operations[READ] > 0 && s.bytes[READ] > 0)),
            "no driver reports reads: {drivers:?}"
        );
    }
}
