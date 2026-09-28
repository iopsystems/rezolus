//! Memory gauges from `/proc/meminfo`, one read and parse per refresh.
//!
//! Every line the sampler knows is mapped to a gauge below. A line the running
//! kernel does not print (`HardwareCorrupted` without `CONFIG_MEMORY_FAILURE`,
//! the huge-page lines without `CONFIG_TRANSPARENT_HUGEPAGE` or
//! `CONFIG_HUGETLB_PAGE`) leaves its gauge never set, and a never-set lazy
//! gauge is absent from every snapshot rather than reading 0 — so absence in
//! a recording means the kernel had no such line, not that the value was zero.
//!
//! Most lines are in kibibytes and are scaled to bytes. The `HugePages_*`
//! lines are page counts with no unit and are exported as counts.

const NAME: &str = "memory_meminfo";

use crate::agent::*;

use metriken::LazyGauge;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

use std::collections::HashMap;

mod stats;

use stats::*;

/// `/proc/meminfo` key (with its trailing colon, as the file prints it), the
/// gauge it fills, and the multiplier that turns the printed number into the
/// gauge's unit: `KIBIBYTES` for the `kB` lines, 1 for the page counts.
const FIELDS: &[(&str, &LazyGauge, i64)] = &[
    // capacity
    ("MemTotal:", &MEMORY_TOTAL, KIBIBYTES as i64),
    ("MemFree:", &MEMORY_FREE, KIBIBYTES as i64),
    ("MemAvailable:", &MEMORY_AVAILABLE, KIBIBYTES as i64),
    ("Buffers:", &MEMORY_BUFFERS, KIBIBYTES as i64),
    ("Cached:", &MEMORY_CACHED, KIBIBYTES as i64),
    // writeback
    ("Dirty:", &MEMORY_DIRTY, KIBIBYTES as i64),
    ("Writeback:", &MEMORY_WRITEBACK, KIBIBYTES as i64),
    // LRU lists
    ("Active(file):", &MEMORY_ACTIVE_FILE, KIBIBYTES as i64),
    ("Active(anon):", &MEMORY_ACTIVE_ANON, KIBIBYTES as i64),
    ("Inactive(file):", &MEMORY_INACTIVE_FILE, KIBIBYTES as i64),
    ("Inactive(anon):", &MEMORY_INACTIVE_ANON, KIBIBYTES as i64),
    ("Unevictable:", &MEMORY_UNEVICTABLE, KIBIBYTES as i64),
    ("Mlocked:", &MEMORY_MLOCKED, KIBIBYTES as i64),
    ("Shmem:", &MEMORY_SHMEM, KIBIBYTES as i64),
    ("Mapped:", &MEMORY_MAPPED, KIBIBYTES as i64),
    ("AnonPages:", &MEMORY_ANON, KIBIBYTES as i64),
    // kernel
    ("SReclaimable:", &MEMORY_SLAB_RECLAIMABLE, KIBIBYTES as i64),
    ("SUnreclaim:", &MEMORY_SLAB_UNRECLAIMABLE, KIBIBYTES as i64),
    (
        "KReclaimable:",
        &MEMORY_KERNEL_RECLAIMABLE,
        KIBIBYTES as i64,
    ),
    ("KernelStack:", &MEMORY_KERNEL_STACK, KIBIBYTES as i64),
    ("PageTables:", &MEMORY_PAGE_TABLES, KIBIBYTES as i64),
    ("Percpu:", &MEMORY_PERCPU, KIBIBYTES as i64),
    // swap
    ("SwapTotal:", &MEMORY_SWAP_TOTAL, KIBIBYTES as i64),
    ("SwapFree:", &MEMORY_SWAP_FREE, KIBIBYTES as i64),
    ("SwapCached:", &MEMORY_SWAP_CACHED, KIBIBYTES as i64),
    // overcommit
    ("CommitLimit:", &MEMORY_COMMIT_LIMIT, KIBIBYTES as i64),
    ("Committed_AS:", &MEMORY_COMMITTED, KIBIBYTES as i64),
    // huge pages
    ("AnonHugePages:", &MEMORY_HUGEPAGES_ANON, KIBIBYTES as i64),
    ("ShmemHugePages:", &MEMORY_HUGEPAGES_SHMEM, KIBIBYTES as i64),
    ("FileHugePages:", &MEMORY_HUGEPAGES_FILE, KIBIBYTES as i64),
    ("Hugetlb:", &MEMORY_HUGETLB, KIBIBYTES as i64),
    ("HugePages_Total:", &MEMORY_HUGETLB_PAGES_TOTAL, 1),
    ("HugePages_Free:", &MEMORY_HUGETLB_PAGES_FREE, 1),
    ("HugePages_Rsvd:", &MEMORY_HUGETLB_PAGES_RESERVED, 1),
    ("HugePages_Surp:", &MEMORY_HUGETLB_PAGES_SURPLUS, 1),
    // errors
    (
        "HardwareCorrupted:",
        &MEMORY_HARDWARE_CORRUPTED,
        KIBIBYTES as i64,
    ),
];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = MeminfoInner::new()?;

    Ok(Some(Box::new(Meminfo {
        inner: inner.into(),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Meminfo {
    inner: Mutex<MeminfoInner>,
}

#[async_trait]
impl Sampler for Meminfo {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

struct MeminfoInner {
    data: String,
    file: File,
    gauges: HashMap<&'static str, (&'static LazyGauge, i64)>,
}

impl MeminfoInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let file = std::fs::File::open("/proc/meminfo").map(File::from_std)?;

        Ok(Self {
            data: String::new(),
            file,
            gauges: field_map(),
        })
    }

    // Acquisition-group bracket (principle 18): `MEMINFO_ACQ.acquire()` before
    // the read, `guard.finish()` after every value from this parse is set.
    // Any read error (`?`) drops the guard without `finish()` — the previous
    // window stands (discard-on-error; missing beats wrong). A partial parse
    // (some recognized keys found, others missing from this particular
    // `/proc/meminfo` snapshot) is NOT an error path: the loop only ever sets
    // the keys it actually finds, so there is no "some values set, then an
    // error" case here to decide between finish/discard — the read itself is
    // the only failure mode, and it fails before any `set()` call.
    pub async fn refresh(&mut self) -> Result<(), std::io::Error> {
        let guard = MEMINFO_ACQ.acquire();

        self.file.rewind().await?;

        self.data.clear();

        self.file.read_to_string(&mut self.data).await?;

        parse(&self.data, &self.gauges);

        guard.finish();

        Ok(())
    }
}

fn field_map() -> HashMap<&'static str, (&'static LazyGauge, i64)> {
    FIELDS
        .iter()
        .map(|(key, gauge, scale)| (*key, (*gauge, *scale)))
        .collect()
}

/// Set every gauge whose key appears in `data`. Keys the text lacks are left
/// untouched, which for a never-set lazy gauge means absent from snapshots.
fn parse(data: &str, gauges: &HashMap<&'static str, (&'static LazyGauge, i64)>) {
    for line in data.lines() {
        let mut parts = line.split_whitespace();

        let Some(key) = parts.next() else {
            continue;
        };

        if let Some((gauge, scale)) = gauges.get(key) {
            if let Some(Ok(v)) = parts.next().map(|v| v.parse::<i64>()) {
                gauge.set(v.saturating_mul(*scale));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `/proc/meminfo` prints: a key with a trailing colon, a
    /// number, and `kB` on every line except the `HugePages_*` counts.
    const SAMPLE: &str = "MemTotal:       131072000 kB
MemFree:         8192000 kB
MemAvailable:   65536000 kB
Buffers:          102400 kB
Cached:         52428800 kB
SwapCached:            0 kB
Active(anon):   20480000 kB
Inactive(anon):   512000 kB
Active(file):   30720000 kB
Inactive(file): 21708800 kB
Unevictable:       65536 kB
Mlocked:           65536 kB
SwapTotal:       4194304 kB
SwapFree:        4194304 kB
Dirty:            307200 kB
Writeback:         12288 kB
AnonPages:      20971520 kB
Mapped:          1048576 kB
Shmem:            524288 kB
KReclaimable:    6291456 kB
Slab:            8388608 kB
SReclaimable:    6291456 kB
SUnreclaim:      2097152 kB
KernelStack:       32768 kB
PageTables:       262144 kB
Percpu:            65536 kB
CommitLimit:    69730304 kB
Committed_AS:   40000000 kB
HardwareCorrupted:     0 kB
AnonHugePages:   2097152 kB
ShmemHugePages:        0 kB
FileHugePages:         0 kB
HugePages_Total:      40
HugePages_Free:       38
HugePages_Rsvd:        2
HugePages_Surp:        0
Hugepagesize:    1048576 kB
Hugetlb:        41943040 kB
";

    #[test]
    fn kilobyte_lines_scale_to_bytes_and_page_counts_do_not() {
        let gauges = field_map();
        parse(SAMPLE, &gauges);
        assert_eq!(MEMORY_DIRTY.value(), 307200 * 1024);
        assert_eq!(MEMORY_WRITEBACK.value(), 12288 * 1024);
        assert_eq!(MEMORY_ACTIVE_FILE.value(), 30720000 * 1024);
        assert_eq!(MEMORY_SLAB_RECLAIMABLE.value(), 6291456 * 1024);
        assert_eq!(MEMORY_COMMITTED.value(), 40000000 * 1024);
        assert_eq!(MEMORY_HUGETLB.value(), 41943040 * 1024);
        assert_eq!(MEMORY_HUGETLB_PAGES_TOTAL.value(), 40);
        assert_eq!(MEMORY_HUGETLB_PAGES_FREE.value(), 38);
        assert_eq!(MEMORY_HUGETLB_PAGES_RESERVED.value(), 2);
        assert_eq!(MEMORY_HUGETLB_PAGES_SURPLUS.value(), 0);
    }

    /// Every key in the table is one the kernel prints, and no key is mapped
    /// twice: a typo here would silently leave a gauge unset forever.
    #[test]
    fn every_field_is_present_in_the_sample_exactly_once() {
        let mut keys: Vec<&str> = FIELDS.iter().map(|(k, _, _)| *k).collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "a key is mapped twice");
        for key in keys {
            assert!(
                SAMPLE.lines().any(|l| l.starts_with(key)),
                "{key} is not a line /proc/meminfo prints"
            );
        }
    }

    /// A key the kernel does not print leaves its gauge alone.
    #[test]
    fn a_missing_line_leaves_its_gauge_untouched() {
        let gauges = field_map();
        parse("MemTotal: 1024 kB\n", &gauges);
        assert_eq!(MEMORY_TOTAL.value(), 1024 * 1024);
        // Set to a marker, parse text without the key, marker survives.
        MEMORY_PERCPU.set(7);
        parse("MemFree: 1 kB\n", &gauges);
        assert_eq!(MEMORY_PERCPU.value(), 7);
    }
}
